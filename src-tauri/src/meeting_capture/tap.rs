//! The system-audio source: a Core Audio process tap.
//!
//! Replaces ScreenCaptureKit as Murmur's system-audio engine. The point is the
//! permission bucket: a tap needs only `kTCCServiceAudioCapture` ("System Audio
//! Recording Only"), where ScreenCaptureKit needs `kTCCServiceScreenCapture`
//! ("Screen & System Audio Recording") and shows Apple's share picker on every
//! start. Murmur captures no video either way; this is about what macOS makes
//! the user agree to. Installed permission behavior still needs macOS QA.
//!
//! The shape is one **aggregate device** containing the default output device,
//! which clocks it, and a global process tap that excludes Murmur's own audio.
//! A single `AudioDeviceIOProc` on that aggregate delivers one input stream:
//! the tap's stereo mix.
//!
//! **The microphone is deliberately not in here.** A5 put it in this same
//! aggregate to get one timeline by construction, and debug-log Bug 9 is what
//! that cost: the tap gates the whole IOProc, so a Mac playing nothing recorded
//! nothing — the microphone included. The microphone now runs as its own source
//! on its own device (`super::source`), and this one is free to be exactly what
//! it is: opportunistic. It delivers while something is playing and says
//! nothing the rest of the time, and that is no longer a failure.
//!
//! Everything downstream — `AudioChunk`, the writers, the recorder, chunking,
//! the manifest — is unchanged. This is a source, not a pipeline.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectSystemObject, kAudioSubDeviceUIDKey, kAudioSubTapUIDKey, kAudioTapPropertyUID,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, CATapDescription, CATapMuteBehavior,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::source::{
    address, check, default_device, device_name, device_uid, input_stream_channels,
    input_stream_format, key, pcm_format, sample_rate_property, string_property,
    validate_stream_format, AudioSource, OwnedObjects, SharedCaptureState, SourceConfiguration,
    TrackSink,
};
use super::{Mt0Error, Mt0ErrorCode};

/// Build the tap and its aggregate device and start delivering system audio.
///
/// Failure here is still fatal to the capture: a missing audio-capture grant or
/// a refused tap is a real problem worth reporting, and is a different thing
/// entirely from a tap that starts fine and then has nothing to hear.
pub(super) fn start_system_audio_source(
    sink: TrackSink,
    shared: SharedCaptureState,
    preferred_output: Option<&str>,
) -> Result<AudioSource, Mt0Error> {
    let (output_device, follows_default) = resolve_output_device(preferred_output)?;
    let output_uid = device_uid(output_device)?;

    let tap = TapGuard(create_process_tap()?);
    let tap_uid = tap_uid(tap.0)?;

    let aggregate = AggregateGuard(create_aggregate_device(&output_uid, &tap_uid)?);

    // The aggregate's input is the tap and nothing else, so this is the
    // authoritative check that we are about to record what we think we are.
    let channels = input_stream_channels(aggregate.0);
    if channels.len() != 1 {
        return Err(Mt0Error::new(
            Mt0ErrorCode::SystemAudioStartFailed,
            format!(
                "system audio device exposes {} input streams, expected 1 (the tap)",
                channels.len()
            ),
        ));
    }

    // **Ask the aggregate, not the tap.** `kAudioTapPropertyFormat` describes
    // the tap's own canonical mix — a flat 48 kHz stereo float — and that is
    // *not* necessarily what the aggregate delivers, because the aggregate is
    // clocked by the output device. Put a Bluetooth headset in call mode and
    // the output device drops to 16 kHz, so the tap advertises 48 kHz while
    // 16 kHz arrives.
    //
    // Under A5's single IOProc that mismatch was survivable-looking: one wrong
    // rate scaled both tracks identically, so the timeline stayed
    // self-consistent and only the WAV headers lied. With independent sources
    // the microphone reads its real rate and the tap does not, the two
    // timelines advance at different speeds, and the capture fails within
    // seconds. Found live 2026-08-03 on a WF-1000XM5 — see the debug log.
    let description = input_stream_format(aggregate.0, "system audio")?;
    let aggregate_virtual_rate = validate_stream_format("system audio", &description)?;
    let output_nominal_rate = sample_rate_property(
        output_device,
        kAudioDevicePropertyNominalSampleRate,
        "read output-device nominal sample rate",
    )?;
    let mut format = pcm_format(&description, channels[0])?;
    // The output device clocks this aggregate. Capture 37 proved that the
    // aggregate can still advertise its canonical 48 kHz virtual format while
    // an earbud route advances callback sample time at 16 kHz. Start from the
    // clock owner's nominal rate; source.rs then validates it against callback
    // sample/host deltas and fails loudly if Core Audio actually converts.
    format.sample_rate = output_nominal_rate;
    let output_name = device_name(output_device).unwrap_or_else(|_| "unavailable".into());
    crate::murmur_diag!(
        "[murmur][audio-validation] source=system_audio output_device_id={output_device} output_device_name={output_name:?} aggregate_device_id={} aggregate_virtual_rate_hz={} output_nominal_rate_hz={} selected_rate_hz={} channels={} bits={} bytes_per_frame={} flags={:#x}",
        aggregate.0,
        aggregate_virtual_rate,
        output_nominal_rate,
        format.sample_rate,
        format.channels,
        format.bits_per_channel,
        format.bytes_per_frame,
        format.flags
    );

    let owned = OwnedObjects {
        aggregate: Some(aggregate.take()),
        tap: Some(tap.take()),
    };
    AudioSource::start(
        "system audio",
        owned.aggregate.expect("aggregate just created"),
        owned,
        SourceConfiguration {
            format,
            advertised_sample_rate: aggregate_virtual_rate,
            health_device_id: output_device,
            health_device_name: output_name,
            default_device_selector: follows_default
                .then_some(kAudioHardwarePropertyDefaultOutputDevice),
        },
        &channels,
        sink,
        shared,
    )
}

fn resolve_output_device(
    preferred_output: Option<&str>,
) -> Result<(AudioObjectID, bool), Mt0Error> {
    let default = default_device(
        kAudioHardwarePropertyDefaultOutputDevice,
        "no default output device is available",
    )?;
    let Some(preferred_name) = preferred_output else {
        return Ok((default, true));
    };

    let mut addr = address(kAudioHardwarePropertyDevices);
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    check(
        status,
        Mt0ErrorCode::SystemAudioStartFailed,
        "list output devices",
    )?;
    let count = size as usize / size_of::<AudioObjectID>();
    let mut devices = vec![0 as AudioObjectID; count];
    if !devices.is_empty() {
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(devices.as_mut_ptr())
                    .expect("non-empty output device storage")
                    .cast::<c_void>(),
            )
        };
        check(
            status,
            Mt0ErrorCode::SystemAudioStartFailed,
            "read output devices",
        )?;
    }

    if let Some(device) = devices
        .into_iter()
        .find(|device| device_name(*device).ok().as_deref() == Some(preferred_name))
    {
        return Ok((device, false));
    }

    crate::murmur_diag!(
        "[murmur][audio-validation] preferred output {preferred_name:?} unavailable; using default"
    );
    Ok((default, true))
}

fn create_process_tap() -> Result<AudioObjectID, Mt0Error> {
    // Exclude Murmur's own audio, the way `setExcludesCurrentProcessAudio(true)`
    // did. This is load-bearing rather than hygiene: the aggregate device
    // belongs to us, so without it the capture would feed on itself.
    //
    // The HAL only mints a process object once a process has touched audio, so
    // a missing object here is normal, not an error — it means Murmur has
    // played nothing yet and there is correspondingly nothing to exclude.
    let excluded: Retained<NSArray<NSNumber>> = match own_process_object() {
        Some(object) => NSArray::from_retained_slice(&[NSNumber::new_u32(object)]),
        None => NSArray::new(),
    };

    // SAFETY: `CATapDescription` is a plain NSObject subclass, the init family
    // consumes the allocation, and `excluded` outlives the call.
    let description = unsafe {
        let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
            CATapDescription::alloc(),
            &excluded,
        );
        description.setName(&NSString::from_str("Murmur meeting capture"));
        // Private: visible only to this process, so it never appears in the
        // user's Sound settings or any other app's device list.
        description.setPrivate(true);
        // Unmuted: the user has to keep hearing their meeting.
        description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        description
    };

    let mut tap: AudioObjectID = 0;
    // SAFETY: `tap` is a live out-pointer for the duration of the call.
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap) };
    check(
        status,
        Mt0ErrorCode::SystemAudioStartFailed,
        "create system audio tap",
    )?;
    if tap == 0 {
        return Err(Mt0Error::new(
            Mt0ErrorCode::SystemAudioStartFailed,
            "system audio tap creation returned no object",
        ));
    }
    Ok(tap)
}

/// The output device plus the tap, and nothing else.
///
/// The output device is here to clock the aggregate, not to be recorded — it
/// contributes no input stream. The microphone used to be a third member; see
/// this module's header for why it is not.
fn create_aggregate_device(output_uid: &str, tap_uid: &str) -> Result<AudioObjectID, Mt0Error> {
    let output_sub_device: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
        &[&*key(kAudioSubDeviceUIDKey)],
        &[&*NSString::from_str(output_uid) as &AnyObject],
    );
    let sub_tap: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
        &[&*key(kAudioSubTapUIDKey)],
        &[&*NSString::from_str(tap_uid) as &AnyObject],
    );

    let aggregate_uid = NSString::from_str(&format!(
        "com.monkeyman.murmur.meeting-capture.{}",
        std::process::id()
    ));
    let name = NSString::from_str("Murmur meeting capture");
    let yes = NSNumber::new_bool(true);
    let no = NSNumber::new_bool(false);
    let main_sub = NSString::from_str(output_uid);
    let sub_devices = NSArray::from_retained_slice(&[output_sub_device]);
    let taps = NSArray::from_retained_slice(&[sub_tap]);

    let keys = [
        key(kAudioAggregateDeviceUIDKey),
        key(kAudioAggregateDeviceNameKey),
        key(kAudioAggregateDeviceIsPrivateKey),
        key(kAudioAggregateDeviceIsStackedKey),
        key(kAudioAggregateDeviceTapAutoStartKey),
        key(kAudioAggregateDeviceMainSubDeviceKey),
        key(kAudioAggregateDeviceSubDeviceListKey),
        key(kAudioAggregateDeviceTapListKey),
    ];
    let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let values: [&AnyObject; 8] = [
        &aggregate_uid,
        &name,
        &yes,
        &no,
        &yes,
        &main_sub,
        &sub_devices,
        &taps,
    ];
    let dict: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&key_refs, &values);

    let mut device: AudioObjectID = 0;
    // SAFETY: NSDictionary is toll-free bridged to CFDictionary, `dict`
    // outlives the call, and `device` is a live out-pointer.
    let status = unsafe {
        let cf_dict = &*Retained::as_ptr(&dict).cast::<CFDictionary>();
        AudioHardwareCreateAggregateDevice(cf_dict, NonNull::from(&mut device))
    };
    check(
        status,
        Mt0ErrorCode::SystemAudioStartFailed,
        "create aggregate capture device",
    )?;
    if device == 0 {
        return Err(Mt0Error::new(
            Mt0ErrorCode::SystemAudioStartFailed,
            "aggregate capture device creation returned no object",
        ));
    }
    Ok(device)
}

fn own_process_object() -> Option<AudioObjectID> {
    let mut addr = address(kAudioHardwarePropertyTranslatePIDToProcessObject);
    let mut size = size_of::<AudioObjectID>() as u32;
    let mut object: AudioObjectID = 0;
    let pid = std::process::id() as i32;
    // SAFETY: the qualifier is a live `pid_t` of the declared size and `object`
    // provides `size` bytes of storage.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut addr),
            size_of::<i32>() as u32,
            std::ptr::from_ref(&pid).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut object).cast::<c_void>(),
        )
    };
    (status == 0 && object != 0).then_some(object)
}

fn tap_uid(tap: AudioObjectID) -> Result<String, Mt0Error> {
    string_property(
        tap,
        kAudioTapPropertyUID,
        "read system audio tap identifier",
    )
}

// ---------------------------------------------------------------------------
// Teardown guards for the construction path
// ---------------------------------------------------------------------------

/// Destroys its object unless `take` claims it. Only used while the source is
/// still being assembled; once `AudioSource` exists, it owns teardown.
struct TapGuard(AudioObjectID);

impl TapGuard {
    fn take(mut self) -> AudioObjectID {
        let id = self.0;
        self.0 = 0;
        id
    }
}

impl Drop for TapGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: created by AudioHardwareCreateProcessTap, destroyed once.
            unsafe { AudioHardwareDestroyProcessTap(self.0) };
        }
    }
}

struct AggregateGuard(AudioObjectID);

impl AggregateGuard {
    fn take(mut self) -> AudioObjectID {
        let id = self.0;
        self.0 = 0;
        id
    }
}

impl Drop for AggregateGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: created by AudioHardwareCreateAggregateDevice, once.
            unsafe { objc2_core_audio::AudioHardwareDestroyAggregateDevice(self.0) };
        }
    }
}
