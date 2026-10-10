//! Input-device enumeration and the Tauri commands behind the Settings
//! → General mic picker.
//!
//! cpal identifies devices by name. There is no stable opaque ID on
//! macOS either, so we round-trip the device name as the "selection
//! key" between Rust and the frontend. If the user has a specific
//! device selected and that device later disappears (unplugged
//! headphones, etc.), the capture code falls back to the current
//! system default rather than erroring.
//!
//! Thread-safety: cpal `Host::input_devices()` is fine to call from
//! any thread. The Tauri commands run on the async runtime.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::audio::AudioHandle;
use crate::permissions::{self, PermissionState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioInputDevice {
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioInputDevicesChanged {
    pub current_default: Option<String>,
    pub devices: Vec<AudioInputDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioOutputDevice {
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioOutputDevicesChanged {
    pub current_default: Option<String>,
    pub devices: Vec<AudioOutputDevice>,
}

/// Preferred output used to clock the system-audio tap for the next meeting.
/// `None` follows the macOS default output. A named choice affects Murmur only;
/// it never reroutes the Mac's sound and captures already in flight are fixed.
#[derive(Clone, Default)]
pub struct OutputDeviceState {
    preferred: Arc<Mutex<Option<String>>>,
}

impl OutputDeviceState {
    pub fn preferred(&self) -> Option<String> {
        self.preferred.lock().clone()
    }

    pub fn set_preferred(&self, name: Option<String>) {
        *self.preferred.lock() = name;
    }
}

pub fn current_default_input_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}

pub fn current_default_output_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|device| device.name().ok())
}

/// Heuristic for Murmur's Automatic mode. macOS used to switch many
/// USB mics to the system default on plug-in, but recent releases can
/// leave the built-in mic selected. For dictation, a USB input is very
/// likely intentional and higher quality, so prefer it inside Murmur
/// without changing the user's global macOS default.
pub fn is_preferred_automatic_input(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("usb")
}

pub fn automatic_input_device_name() -> Option<String> {
    let host = cpal::default_host();
    let devices = host.input_devices().ok()?;
    devices
        .filter_map(|d| d.name().ok())
        .find(|name| is_preferred_automatic_input(name))
}

/// List every input device cpal can see, with an `is_default` flag on
/// whichever one macOS currently considers the system default input.
#[tauri::command]
pub fn list_input_devices() -> Vec<AudioInputDevice> {
    if permissions::current().microphone != PermissionState::Granted {
        return Vec::new();
    }

    let host = cpal::default_host();
    let default_name = current_default_input_name();

    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };

    devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let is_default = default_name.as_deref() == Some(name.as_str());
            Some(AudioInputDevice { name, is_default })
        })
        .collect()
}

/// List every output device cpal can see. Unlike input enumeration this does
/// not require Microphone permission and therefore never triggers a TCC prompt.
#[tauri::command]
pub fn list_output_devices() -> Vec<AudioOutputDevice> {
    let host = cpal::default_host();
    let default_name = current_default_output_name();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };

    devices
        .filter_map(|device| {
            let name = device.name().ok()?;
            let is_default = default_name.as_deref() == Some(name.as_str());
            Some(AudioOutputDevice { name, is_default })
        })
        .collect()
}

/// Persist the user's preferred input device on the audio handle. A
/// `None` value clears the preference and reverts to the system
/// default on the next capture. Captures in flight are unaffected —
/// the change takes hold next time the user holds the hotkey.
#[tauri::command]
pub fn set_input_device(app: AppHandle, audio: State<AudioHandle>, name: Option<String>) {
    audio.set_preferred_device(name.clone());
    crate::tray::sync_microphone_selection(&app, name);
}

/// Select the output device Murmur will use for its next meeting capture.
/// This does not change the global macOS sound route.
#[tauri::command]
pub fn set_output_device(app: AppHandle, output: State<OutputDeviceState>, name: Option<String>) {
    output.set_preferred(name.clone());
    crate::tray::sync_output_selection(&app, name);
}

/// Watch macOS/CoreAudio for input-device changes and notify both the
/// frontend device picker and the user. No-op on non-macOS targets.
#[cfg(target_os = "macos")]
pub fn spawn_watcher(app: tauri::AppHandle, audio: AudioHandle) {
    macos_device_watcher::spawn(app, audio);
}

#[cfg(not(target_os = "macos"))]
pub fn spawn_watcher(_app: tauri::AppHandle, _audio: AudioHandle) {}

#[cfg(target_os = "macos")]
mod macos_device_watcher {
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    use coreaudio_sys::{
        kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMaster,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, AudioObjectAddPropertyListener,
        AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyListenerProc, OSStatus,
        UInt32,
    };
    use tauri::AppHandle;

    use super::{
        automatic_input_device_name, current_default_input_name, current_default_output_name,
        list_input_devices, list_output_devices, AudioInputDevicesChanged,
        AudioOutputDevicesChanged,
    };
    use crate::audio::AudioHandle;
    use crate::notify;
    use crate::permissions::{self, PermissionState};

    const DEVICE_CHANGE_DEBOUNCE: Duration = Duration::from_millis(300);

    pub fn spawn(app: AppHandle, audio: AudioHandle) {
        std::thread::Builder::new()
            .name("murmur-audio-devices".into())
            .spawn(move || watcher_loop(app, audio))
            .expect("spawn audio device watcher");
    }

    fn watcher_loop(app: AppHandle, audio: AudioHandle) {
        // Do not touch CoreAudio input-device enumeration before the user
        // explicitly grants Microphone. On macOS, even asking cpal for the
        // default input device can trigger the native microphone TCC prompt,
        // which made onboarding appear blank/frozen on first launch.
        while permissions::current().microphone != PermissionState::Granted {
            std::thread::sleep(Duration::from_secs(1));
        }

        let (tx, rx) = mpsc::channel::<()>();
        let callback_tx = Box::into_raw(Box::new(tx.clone())) as *mut c_void;

        register_listener(kAudioHardwarePropertyDefaultInputDevice, callback_tx);
        register_listener(kAudioHardwarePropertyDefaultOutputDevice, callback_tx);
        register_listener(kAudioHardwarePropertyDevices, callback_tx);

        let mut last_default = current_default_input_name();
        let mut last_automatic = automatic_input_device_name().or_else(|| last_default.clone());
        let mut preferred_unavailable = false;

        while rx.recv().is_ok() {
            while rx.recv_timeout(DEVICE_CHANGE_DEBOUNCE).is_ok() {}

            let current_default = current_default_input_name();
            let current_automatic =
                automatic_input_device_name().or_else(|| current_default.clone());
            let devices = list_input_devices();
            let payload = AudioInputDevicesChanged {
                current_default: current_default.clone(),
                devices: devices.clone(),
            };
            crate::window_events::emit_main(&app, "murmur://input-devices-changed", payload);
            let output_payload = AudioOutputDevicesChanged {
                current_default: current_default_output_name(),
                devices: list_output_devices(),
            };
            crate::window_events::emit_main(
                &app,
                "murmur://output-devices-changed",
                output_payload,
            );
            crate::tray::refresh_microphones(&app);
            crate::tray::refresh_outputs(&app);

            let preferred = audio.preferred_device();
            if let Some(preferred_name) = preferred.as_deref() {
                let unavailable_now = !devices.iter().any(|device| device.name == preferred_name);
                if unavailable_now && !preferred_unavailable {
                    notify::error(
                        &app,
                        "Selected microphone unavailable",
                        "The selected input disconnected. Murmur will use the system default.",
                    );
                }
                preferred_unavailable = unavailable_now;
            } else if current_automatic != last_automatic || current_default != last_default {
                if current_automatic.is_some() {
                    notify::info(
                        &app,
                        "Microphone switched",
                        "Murmur selected an available input device.",
                    );
                } else {
                    notify::error(
                        &app,
                        "No microphone available",
                        "Murmur can't see an input device right now.",
                    );
                }
            }
            last_default = current_default;
            last_automatic = current_automatic;
        }
    }

    fn register_listener(selector: u32, callback_tx: *mut c_void) {
        let address = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMaster,
        };
        let listener: AudioObjectPropertyListenerProc = Some(device_change_callback);
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject,
                &address as *const _,
                listener,
                callback_tx,
            )
        };
        if status != 0 {
            crate::murmur_dev_log!(
                "[murmur] failed to register CoreAudio device listener {selector}: {status}"
            );
        }
    }

    unsafe extern "C" fn device_change_callback(
        _object_id: AudioObjectID,
        _number_addresses: UInt32,
        _addresses: *const AudioObjectPropertyAddress,
        client_data: *mut c_void,
    ) -> OSStatus {
        if !client_data.is_null() {
            let tx = &*(client_data as *const mpsc::Sender<()>);
            let _ = tx.send(());
        }
        0
    }
}
