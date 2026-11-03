//! Bluetooth headset awareness for dictation.
//!
//! Classic Bluetooth cannot carry high-quality stereo playback (A2DP) and the
//! headset microphone at the same time. Opening a headset mic makes macOS
//! renegotiate the link into the hands-free profile: playback drops to
//! narrowband mono (the "hallway, no bass" sound), audio gaps for roughly
//! 0.5–2 s while the link switches, and the mic delivers nothing until the
//! switch completes. Closing the mic reverses it, with another gap and a short
//! stretch of call-quality playback. No macOS API lets an app avoid this
//! (`AVAudioSession`'s high-quality Bluetooth recording option is iOS-only).
//!
//! Murmur therefore keeps the headset mic and hides the transition instead:
//! `mute_audio` silences output before the mic opens and only restores it once
//! the headset is back in music mode, and the indicator shows "Connecting mic…"
//! until frames arrive. This module answers the two questions that needs:
//! is the next capture a Bluetooth mic, and has the output left call mode?
//!
//! Everything here is a no-op unless a Bluetooth input is actually selected,
//! so built-in and USB microphones keep their existing path untouched.

use std::time::{Duration, Instant};

use crate::audio::CaptureReadiness;

/// Headsets deliver zero-filled frames while the link switches; real audio
/// followed ~0.6 s after the press on a WF-1000XM5. A mic that gates true
/// silence until speech never crosses the signal threshold, so after this
/// long without signal the capture is treated as live anyway.
const LIVE_SIGNAL_MAX_WAIT: Duration = Duration::from_millis(1500);
const LIVE_POLL: Duration = Duration::from_millis(20);

/// Hands-free profile rates are 8, 16 or 24 kHz; music playback runs at
/// 44.1/48 kHz. An output already below this when dictation starts is in call
/// mode (another app holds the mic), so opening ours causes no switch.
const CALL_MODE_MAX_RATE: f64 = 32_000.0;
#[cfg(target_os = "macos")]
const RETURN_POLL: Duration = Duration::from_millis(25);
/// Margin after the rate flips back so the restart gap, not the tail of
/// call-quality audio, is what plays when output is restored.
#[cfg(target_os = "macos")]
const RETURN_SETTLE: Duration = Duration::from_millis(250);
/// Used when the device never reported a rate change we can watch.
#[cfg(target_os = "macos")]
const RETURN_FALLBACK: Duration = Duration::from_millis(1500);
/// Hard cap: output is never held silent longer than this after release.
#[cfg(target_os = "macos")]
const RETURN_MAX: Duration = Duration::from_secs(4);

/// The output side of a Bluetooth dictation route: the headset whose playback
/// degrades while its mic is open, and the rate it plays music at.
#[derive(Clone, Copy, Debug)]
pub struct BluetoothOutput {
    device: u32,
    music_rate: f64,
}

/// A dictation that will open a Bluetooth microphone.
#[derive(Clone, Copy, Debug)]
pub struct BluetoothRoute {
    /// `Some` when the default output is a Bluetooth device in music mode, so
    /// opening the mic will audibly switch it into call mode.
    pub output: Option<BluetoothOutput>,
}

/// How the wait for music mode ended. Logged so real devices tell us whether
/// rate tracking works for them or they fall back to the fixed delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnOutcome {
    RateRestored,
    FixedDelay,
    OutputGone,
    TimedOut,
}

/// How a Bluetooth capture was judged live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveVia {
    /// The mic delivered non-silent audio.
    Signal,
    /// Frames arrived but stayed digitally silent for [`LIVE_SIGNAL_MAX_WAIT`].
    Timeout,
}

/// Wait until a Bluetooth mic is actually hearing the room: frames alone are
/// not enough, because the headset fills the switch with digital silence.
/// Returns `None` if the capture ends first.
pub async fn wait_for_live_audio(ready: &CaptureReadiness) -> Option<LiveVia> {
    let mut frames_at = None;
    loop {
        if !ready.is_active() {
            return None;
        }
        if ready.since_first_signal().is_some() {
            return Some(LiveVia::Signal);
        }
        if ready.since_first_callback().is_some() {
            let frames_at = *frames_at.get_or_insert_with(Instant::now);
            if frames_at.elapsed() >= LIVE_SIGNAL_MAX_WAIT {
                return Some(LiveVia::Timeout);
            }
        }
        tokio::time::sleep(LIVE_POLL).await;
    }
}

/// Whether the default output is a Bluetooth device. On Bluetooth outputs,
/// muting (or volume 0) also silences Murmur's chimes, unlike built-in
/// speakers where alerts keep their own volume.
pub fn output_is_bluetooth() -> bool {
    imp::output_is_bluetooth()
}

/// Whether any other process is sending audio to an output right now.
/// Chrome keeps its stream open ~10 s after a pause, so a just-paused video
/// still counts; that only costs a chime, never an unmuted transition.
pub fn other_audio_playing() -> bool {
    imp::other_audio_playing()
}

/// Inspect the route the next capture will use. Returns `None` for any
/// non-Bluetooth microphone; the cheap Core Audio pre-check means that path
/// never enumerates through cpal.
pub fn probe(preferred_input: Option<&str>) -> Option<BluetoothRoute> {
    imp::probe(preferred_input)
}

impl BluetoothOutput {
    pub fn music_rate(&self) -> f64 {
        self.music_rate
    }

    /// Current nominal rate, or `None` if the device has gone away.
    pub fn current_rate(&self) -> Option<f64> {
        imp::nominal_rate(self.device)
    }

    /// Wait until the headset has left call mode after the mic closed.
    /// `call_rate` is the rate observed while the mic was live, if known.
    pub async fn wait_for_music_mode(&self, call_rate: Option<f64>) -> (ReturnOutcome, Duration) {
        imp::wait_for_music_mode(self, call_rate).await
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_music_mode_rate(rate: f64) -> bool {
    rate >= CALL_MODE_MAX_RATE
}

fn same_rate(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0
}

/// Whether this headset's nominal rate reveals call mode, so the return to
/// music mode can be watched rather than waited out with a fixed delay. True
/// if the rate differed from the music rate while the mic was live, or still
/// differs now that it has closed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn rate_tracks_mode(music_rate: f64, call_rate: Option<f64>, rate_now: f64) -> bool {
    call_rate.is_some_and(|rate| !same_rate(rate, music_rate)) || !same_rate(rate_now, music_rate)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::mem::{size_of, MaybeUninit};

    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use coreaudio_sys::{
        kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreams,
        kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeBluetooth,
        kAudioDeviceTransportTypeBluetoothLE, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioHardwarePropertyDevices, kAudioHardwarePropertyProcessObjectList,
        kAudioObjectPropertyElementMaster, kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
        kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID, AudioObjectGetPropertyData,
        AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    };

    use super::*;

    pub(super) fn probe(preferred_input: Option<&str>) -> Option<BluetoothRoute> {
        let bluetooth_inputs: Vec<String> = devices()
            .into_iter()
            .filter(|&device| is_bluetooth(device) && has_input_streams(device))
            .filter_map(device_name)
            .collect();
        if bluetooth_inputs.is_empty() {
            return None;
        }
        let input = crate::audio::resolved_input_device_name(preferred_input)?;
        if !bluetooth_inputs.contains(&input) {
            return None;
        }
        let output = default_output().and_then(|device| {
            if !is_bluetooth(device) {
                return None;
            }
            let music_rate = nominal_rate(device)?;
            is_music_mode_rate(music_rate).then_some(BluetoothOutput { device, music_rate })
        });
        crate::murmur_dev_log!(
            "[murmur] bluetooth mic route: input={input} output_music_rate={:?}",
            output.map(|o| o.music_rate)
        );
        Some(BluetoothRoute { output })
    }

    pub(super) fn output_is_bluetooth() -> bool {
        default_output().is_some_and(is_bluetooth)
    }

    pub(super) fn other_audio_playing() -> bool {
        let own_pid = std::process::id() as i32;
        let playing = list(
            kAudioObjectSystemObject,
            kAudioHardwarePropertyProcessObjectList,
        )
        .into_iter()
        .any(|process| {
            get::<u32>(
                process,
                kAudioProcessPropertyIsRunningOutput,
                kAudioObjectPropertyScopeGlobal,
            ) == Some(1)
                && get::<i32>(
                    process,
                    kAudioProcessPropertyPID,
                    kAudioObjectPropertyScopeGlobal,
                ) != Some(own_pid)
        });
        crate::murmur_dev_log!("[murmur] other audio playing: {playing}");
        playing
    }

    pub(super) async fn wait_for_music_mode(
        output: &BluetoothOutput,
        call_rate: Option<f64>,
    ) -> (ReturnOutcome, Duration) {
        let started = Instant::now();
        let Some(rate_now) = output.current_rate() else {
            return (ReturnOutcome::OutputGone, started.elapsed());
        };
        if !rate_tracks_mode(output.music_rate, call_rate, rate_now) {
            tokio::time::sleep(RETURN_FALLBACK).await;
            return (ReturnOutcome::FixedDelay, started.elapsed());
        }
        loop {
            match output.current_rate() {
                None => return (ReturnOutcome::OutputGone, started.elapsed()),
                Some(rate) if same_rate(rate, output.music_rate) => {
                    tokio::time::sleep(RETURN_SETTLE).await;
                    return (ReturnOutcome::RateRestored, started.elapsed());
                }
                Some(_) => {}
            }
            if started.elapsed() >= RETURN_MAX {
                return (ReturnOutcome::TimedOut, started.elapsed());
            }
            tokio::time::sleep(RETURN_POLL).await;
        }
    }

    pub(super) fn nominal_rate(device: AudioObjectID) -> Option<f64> {
        get::<f64>(
            device,
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        )
        .filter(|rate| rate.is_finite() && *rate > 0.0)
    }

    fn is_bluetooth(device: AudioObjectID) -> bool {
        matches!(
            get::<u32>(device, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal),
            Some(t) if t == kAudioDeviceTransportTypeBluetooth || t == kAudioDeviceTransportTypeBluetoothLE
        )
    }

    fn default_output() -> Option<AudioObjectID> {
        get::<AudioObjectID>(
            kAudioObjectSystemObject,
            kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal,
        )
        .filter(|&device| device != 0)
    }

    fn devices() -> Vec<AudioObjectID> {
        list(kAudioObjectSystemObject, kAudioHardwarePropertyDevices)
    }

    /// Read a variable-length list of object IDs.
    fn list(object: AudioObjectID, selector: u32) -> Vec<AudioObjectID> {
        let address = address(selector, kAudioObjectPropertyScopeGlobal);
        let mut size = 0_u32;
        // SAFETY: valid address and out-pointer; no qualifier.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(object, &address, 0, std::ptr::null(), &mut size)
        };
        if status != 0 || size == 0 {
            return Vec::new();
        }
        let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
        // SAFETY: `ids` holds exactly `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                ids.as_mut_ptr() as *mut c_void,
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / size_of::<AudioObjectID>());
        ids
    }

    fn has_input_streams(device: AudioObjectID) -> bool {
        let address = address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput);
        let mut size = 0_u32;
        // SAFETY: valid address and out-pointer; no qualifier.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(device, &address, 0, std::ptr::null(), &mut size)
        };
        status == 0 && size > 0
    }

    /// Matches cpal's device name (`kAudioDevicePropertyDeviceNameCFString`
    /// is the same selector as `kAudioObjectPropertyName`).
    fn device_name(device: AudioObjectID) -> Option<String> {
        let raw = get::<CFStringRef>(
            device,
            kAudioObjectPropertyName,
            kAudioObjectPropertyScopeGlobal,
        )?;
        if raw.is_null() {
            return None;
        }
        // SAFETY: Core Audio returns the name +1; adopt that reference.
        Some(unsafe { CFString::wrap_under_create_rule(raw) }.to_string())
    }

    fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMaster,
        }
    }

    /// Read a fixed-size property. Rejects short reads.
    fn get<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
        let address = address(selector, scope);
        let mut value = MaybeUninit::<T>::zeroed();
        let mut size = size_of::<T>() as u32;
        // SAFETY: `value` has room for `size` bytes and Core Audio writes at
        // most that many; a zeroed `T` is valid for the plain types used here.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                value.as_mut_ptr() as *mut c_void,
            )
        };
        (status == 0 && size as usize == size_of::<T>()).then(|| unsafe { value.assume_init() })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub(super) fn probe(_preferred_input: Option<&str>) -> Option<BluetoothRoute> {
        None
    }

    pub(super) fn nominal_rate(_device: u32) -> Option<f64> {
        None
    }

    pub(super) fn output_is_bluetooth() -> bool {
        false
    }

    pub(super) fn other_audio_playing() -> bool {
        false
    }

    pub(super) async fn wait_for_music_mode(
        _output: &BluetoothOutput,
        _call_rate: Option<f64>,
    ) -> (ReturnOutcome, Duration) {
        (ReturnOutcome::OutputGone, Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn silent_placeholder_frames_are_not_live_audio() {
        let ready = CaptureReadiness::new();
        let producer = ready.clone();
        let feed = tokio::spawn(async move {
            // Zero-filled frames during the switch, then real audio.
            producer.note_callback(128, 0.0);
            tokio::time::sleep(Duration::from_millis(100)).await;
            producer.note_callback(128, 0.01);
        });
        let started = Instant::now();
        assert_eq!(wait_for_live_audio(&ready).await, Some(LiveVia::Signal));
        assert!(started.elapsed() >= Duration::from_millis(100));
        feed.await.unwrap();
    }

    #[tokio::test]
    async fn digitally_silent_mic_goes_live_after_the_cap() {
        let ready = CaptureReadiness::new();
        ready.note_callback(128, 0.0);
        let started = Instant::now();
        assert_eq!(wait_for_live_audio(&ready).await, Some(LiveVia::Timeout));
        assert!(started.elapsed() >= LIVE_SIGNAL_MAX_WAIT);
    }

    #[tokio::test]
    async fn ended_capture_is_never_live() {
        let ready = CaptureReadiness::new();
        ready.close();
        assert_eq!(wait_for_live_audio(&ready).await, None);
    }

    #[test]
    fn hands_free_rates_are_call_mode_and_music_rates_are_not() {
        for rate in [8_000.0, 16_000.0, 24_000.0] {
            assert!(!is_music_mode_rate(rate), "{rate}");
        }
        for rate in [44_100.0, 48_000.0] {
            assert!(is_music_mode_rate(rate), "{rate}");
        }
    }

    #[test]
    fn rate_tracking_needs_an_observed_rate_change() {
        // Headset dropped to 24 kHz while the mic was live: watch for 48 kHz.
        assert!(rate_tracks_mode(48_000.0, Some(24_000.0), 24_000.0));
        // Already back by release time: still tracked, so no fixed delay.
        assert!(rate_tracks_mode(48_000.0, Some(16_000.0), 48_000.0));
        // Released before frames arrived but the link already switched.
        assert!(rate_tracks_mode(48_000.0, None, 16_000.0));
        // Rate never moved: the device hides its mode, use the fixed delay.
        assert!(!rate_tracks_mode(48_000.0, Some(48_000.0), 48_000.0));
        assert!(!rate_tracks_mode(44_100.0, None, 44_100.0));
    }
}
