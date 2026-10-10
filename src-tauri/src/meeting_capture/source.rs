//! Independent capture sources on one media timeline — Phase A2.
//!
//! A **source** is one CoreAudio device, one IOProc, one anchor, and one track
//! sink. A capture is a set of sources sharing a clock, a pause gate and a
//! timeline origin. Adding a source is adding a source; it is not a
//! restructuring.
//!
//! This replaces A5's single aggregate device carrying both the microphone and
//! the system-audio tap. That design bought "one timeline by construction" and
//! cost debug-log Bug 9: **the tap gates the whole IOProc**, so a Mac with
//! nothing playing recorded nothing at all, microphone included. The two
//! sources now behave differently on purpose:
//!
//! | Source | Device | Behaviour |
//! |---|---|---|
//! | Microphone | the default input device directly | always delivers |
//! | System audio | an aggregate of the output device + a process tap | only while something is playing |
//!
//! Two devices means two device clocks, and cross-source rebasing is exactly
//! what A5 was right to avoid. It is also unnecessary: both report
//! `mHostTime`, which is `mach_absolute_time` and system-wide. Each source
//! therefore anchors **once**, on its first callback, and derives every later
//! timestamp in the frame domain. All of that arithmetic lives in
//! [`super::timeline`], which has no CoreAudio in it and is tested
//! exhaustively without a device.
//!
//! What a source deliberately does *not* do is notice that it has produced
//! nothing. A tap that never fires simply never anchors. Keeping the two
//! tracks aligned when that happens is the recorder's job — see
//! `TrackInput::pad_to`.

use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use objc2::rc::Retained;
use objc2_core_audio::{
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyStreams,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectSystemObject, kAudioStreamPropertyVirtualFormat,
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags,
};
use objc2_foundation::NSString;
use parking_lot::Mutex;

use crate::captures::recovery::CaptureSourceHealth;

use super::clock::CaptureClock;
use super::macos::AudioDestination;
use super::timeline::{frames_to_ns, AtomicSourceAnchor, TimelineOriginCell};
use super::writer::{AudioChunk, PcmFormat, TrackStats};
use super::{Mt0Error, Mt0ErrorCode, MAX_CALLBACK_BYTES};

/// `kAudioFormatFlagIsFloat` and friends, not re-exported by
/// `objc2-core-audio-types`.
pub(super) const FLAG_IS_FLOAT: u32 = 1 << 0;
pub(super) const FLAG_IS_PACKED: u32 = 1 << 3;
pub(super) const FLAG_IS_NON_INTERLEAVED: u32 = 1 << 5;

/// Everything a capture shares across its sources. All of it is owned by
/// `ActiveCapture` and merely borrowed by the IOProcs.
#[derive(Clone)]
pub(super) struct SharedCaptureState {
    pub fatal: Arc<Mutex<Option<Mt0Error>>>,
    pub clock: Arc<Mutex<CaptureClock>>,
    pub paused: Arc<AtomicBool>,
    pub hard_limit_reached: Arc<AtomicBool>,
    /// Claimed by whichever source produces the first callback. Deliberately
    /// not claimed at start: a source that is running but silent must not
    /// define where zero is.
    pub origin: Arc<TimelineOriginCell>,
}

/// Where one source's audio goes, and the counters the controller reads.
pub(super) struct TrackSink {
    pub destination: AudioDestination,
    pub stats: Arc<TrackStats>,
}

/// Everything the IOProc needs. Boxed and handed to CoreAudio as client data;
/// freed only after the IOProc has been destroyed.
struct SourceIoState {
    label: &'static str,
    shared: SharedCaptureState,
    sink: TrackSink,
    anchor: AtomicSourceAnchor,
    health: Arc<SourceHealthState>,
    format: PcmFormat,
    /// How the device lays its input out: one buffer of interleaved channels,
    /// or one single-channel buffer per channel. Fixed at start and asserted on
    /// every callback, because a change means the device reconfigured under us.
    layout: BufferLayout,
}

/// Health state survives IOProc teardown so the final durable snapshot is
/// taken only after Core Audio has proven no callback remains in flight.
struct SourceHealthState {
    rate_validator: Mutex<SampleRateValidator>,
    device_id: AudioObjectID,
    device_name: String,
    advertised_sample_rate: u32,
    default_device_selector: Option<u32>,
    layout_channels: Vec<u32>,
}

/// Validate the device's declared sample rate against the clock that callbacks
/// actually deliver. Core Audio can expose a stale aggregate virtual rate while
/// a headset is clocking the aggregate at a call-mode rate (capture 37 was
/// declared 48 kHz but delivered 16 kHz). The timeline watchdog can keep such a
/// stream monotonic by inserting silence, so this check must fail before silent
/// corruption reaches transcription.
#[derive(Debug, Default)]
struct SampleRateValidator {
    last_sample_time: Option<i64>,
    last_host_ns: Option<i64>,
    accumulated_frames: i64,
    accumulated_host_ns: i64,
    measured_rate: Option<f64>,
    first_callback_host_ns: Option<i64>,
    last_callback_host_ns: Option<i64>,
    callback_count: u64,
    gap_count: u64,
    format_change_count: u64,
}

const RATE_VALIDATION_WINDOW_NS: i64 = 250_000_000;
const RATE_VALIDATION_MAX_GAP_NS: i64 = 500_000_000;
const RATE_VALIDATION_TOLERANCE: f64 = 0.08;

impl SampleRateValidator {
    fn reset(&mut self) {
        self.last_sample_time = None;
        self.last_host_ns = None;
        self.accumulated_frames = 0;
        self.accumulated_host_ns = 0;
        // A resumed source or one returning after a long gap must prove its
        // current clock again. Keeping the old measurement would allow a
        // short post-transition tail to inherit a stale valid verdict.
        self.measured_rate = None;
    }

    fn note_format_change(&mut self) {
        self.format_change_count = self.format_change_count.saturating_add(1);
    }

    fn note_callback(&mut self, host_ns: i64) {
        self.first_callback_host_ns.get_or_insert(host_ns);
        self.last_callback_host_ns = Some(host_ns);
        self.callback_count = self.callback_count.saturating_add(1);
    }

    fn health_verdict(&self, track: &str) -> (bool, Option<&'static str>) {
        if self.format_change_count > 0 {
            return (false, Some("sample_rate_or_route_changed"));
        }
        if track == "system" && self.callback_count == 0 {
            return (true, None);
        }
        if self.callback_count == 0 {
            return (false, Some("microphone_no_callbacks"));
        }
        if self.measured_rate.is_none() {
            return (false, Some("media_clock_unproven"));
        }
        (true, None)
    }

    fn observe(&mut self, sample_time: i64, host_ns: i64, expected_rate: u32) -> Option<f64> {
        let (Some(previous_sample), Some(previous_host)) =
            (self.last_sample_time, self.last_host_ns)
        else {
            self.last_sample_time = Some(sample_time);
            self.last_host_ns = Some(host_ns);
            return None;
        };
        let frame_delta = sample_time.saturating_sub(previous_sample);
        let host_delta = host_ns.saturating_sub(previous_host);
        self.last_sample_time = Some(sample_time);
        self.last_host_ns = Some(host_ns);

        // A process tap is intentionally opportunistic and may stop calling us
        // during silence. Start a fresh observation after that legitimate gap.
        if frame_delta <= 0 || host_delta <= 0 || host_delta > RATE_VALIDATION_MAX_GAP_NS {
            if host_delta > RATE_VALIDATION_MAX_GAP_NS {
                self.gap_count = self.gap_count.saturating_add(1);
            }
            self.reset();
            // Keep this callback as the beginning of the new observation
            // window; reset intentionally cleared the previous epoch.
            self.last_sample_time = Some(sample_time);
            self.last_host_ns = Some(host_ns);
            return None;
        }
        self.accumulated_frames = self.accumulated_frames.saturating_add(frame_delta);
        self.accumulated_host_ns = self.accumulated_host_ns.saturating_add(host_delta);
        if self.accumulated_host_ns < RATE_VALIDATION_WINDOW_NS {
            return None;
        }

        let observed =
            self.accumulated_frames as f64 * 1_000_000_000.0 / self.accumulated_host_ns as f64;
        self.accumulated_frames = 0;
        self.accumulated_host_ns = 0;
        self.measured_rate = Some(observed);
        let expected = f64::from(expected_rate);
        if expected <= 0.0 || ((observed - expected).abs() / expected) > RATE_VALIDATION_TOLERANCE {
            self.format_change_count = self.format_change_count.saturating_add(1);
            Some(observed)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
struct BufferLayout {
    buffers: usize,
    bytes_per_frame_per_buffer: usize,
    planar: bool,
}

/// CoreAudio objects a source created and must destroy after its IOProc, in
/// this order: the aggregate device references the tap, so it goes first.
#[derive(Default, Clone, Copy)]
pub(super) struct OwnedObjects {
    pub aggregate: Option<AudioObjectID>,
    pub tap: Option<AudioObjectID>,
}

pub(super) struct SourceConfiguration {
    pub format: PcmFormat,
    pub advertised_sample_rate: u32,
    pub health_device_id: AudioObjectID,
    pub health_device_name: String,
    pub default_device_selector: Option<u32>,
}

/// One device delivering one track.
///
/// Owns its teardown and performs it exactly once, in the only safe order:
/// stop IO, destroy the IOProc, destroy the aggregate, destroy the tap, free
/// the state allocation. Destroying the IOProc is what *proves* no callback is
/// still running — CoreAudio will not return from it while one is in flight —
/// so nothing may free the state before that returns.
pub(super) struct AudioSource {
    label: &'static str,
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    state: *mut SourceIoState,
    health: Arc<SourceHealthState>,
    owned: OwnedObjects,
    format: PcmFormat,
    layout: BufferLayout,
    running: bool,
    torn_down: bool,
}

// SAFETY: the contained ids are plain integers and the `SourceIoState` pointer
// is only dereferenced by the IOProc, which CoreAudio serialises against the
// start/stop calls this type makes. The controller thread owns the source.
unsafe impl Send for AudioSource {}

impl AudioSource {
    /// Install an IOProc on `device` and begin delivering audio.
    ///
    /// The clock is *not* started here and no device clock is read: the first
    /// callback anchors the source, so nothing is lost to a start-up race and
    /// nothing can lose the race Bug 8 was about.
    ///
    /// `owned` names the objects this source created on its way to `device` and
    /// is responsible for destroying — for a plain input device, nothing.
    pub(super) fn start(
        label: &'static str,
        device: AudioObjectID,
        owned: OwnedObjects,
        configuration: SourceConfiguration,
        layout_channels: &[u32],
        sink: TrackSink,
        shared: SharedCaptureState,
    ) -> Result<Self, Mt0Error> {
        let guard = SourceObjectGuard(owned);
        let SourceConfiguration {
            format,
            advertised_sample_rate,
            health_device_id,
            health_device_name,
            default_device_selector,
        } = configuration;
        let layout = buffer_layout(label, format, layout_channels)?;
        let health = Arc::new(SourceHealthState {
            rate_validator: Mutex::new(SampleRateValidator::default()),
            device_id: health_device_id,
            device_name: health_device_name,
            advertised_sample_rate,
            default_device_selector,
            layout_channels: layout_channels.to_vec(),
        });
        // Populate the mach timebase before any callback needs it: resolving it
        // lazily would put a one-time lock on the audio thread.
        let _ = host_time_to_ns(0);

        let state = Box::into_raw(Box::new(SourceIoState {
            label,
            shared,
            sink,
            anchor: AtomicSourceAnchor::new(format.sample_rate),
            health: Arc::clone(&health),
            format,
            layout,
        }));

        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `io_proc` matches the AudioDeviceIOProc ABI, `state` stays
        // alive until `teardown` destroys the IOProc, and `proc_id` is a live
        // out-pointer.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                Some(io_proc),
                state.cast::<c_void>(),
                NonNull::from(&mut proc_id),
            )
        };
        if let Err(error) = check(
            status,
            Mt0ErrorCode::SystemAudioStartFailed,
            &format!("install {label} IOProc"),
        ) {
            // SAFETY: no IOProc was created, so nothing else holds this.
            drop(unsafe { Box::from_raw(state) });
            return Err(error);
        }

        // SAFETY: `proc_id` was just created against this device.
        let status = unsafe { AudioDeviceStart(device, proc_id) };
        if let Err(error) = check(
            status,
            Mt0ErrorCode::SystemAudioStartFailed,
            &format!("start {label} capture device"),
        ) {
            // SAFETY: created but never started, so no callback can be running.
            unsafe {
                AudioDeviceDestroyIOProcID(device, proc_id);
                drop(Box::from_raw(state));
            }
            return Err(error);
        }

        guard.disarm();
        Ok(Self {
            label,
            device,
            proc_id,
            state,
            health,
            owned,
            format,
            layout,
            running: true,
            torn_down: false,
        })
    }

    pub(super) fn format(&self) -> PcmFormat {
        self.format
    }

    pub(super) fn health_snapshot(&self, track: &'static str) -> CaptureSourceHealth {
        let validator = self.health.rate_validator.lock();
        let measured = validator.measured_rate.map(|rate| rate.round() as u32);
        let (transcribable, diagnosis) = validator.health_verdict(track);
        CaptureSourceHealth {
            track: track.into(),
            device_id: self.health.device_id,
            device_name: self.health.device_name.clone(),
            advertised_sample_rate: self.health.advertised_sample_rate,
            selected_sample_rate: self.format.sample_rate,
            measured_sample_rate: measured,
            channels: self.format.channels,
            layout: if self.layout.planar {
                "non_interleaved".into()
            } else {
                "interleaved".into()
            },
            first_callback_host_ns: validator.first_callback_host_ns,
            last_callback_host_ns: validator.last_callback_host_ns,
            callback_count: validator.callback_count,
            gap_count: validator.gap_count,
            format_change_count: validator.format_change_count,
            peak: 0.0,
            rms: 0.0,
            transcribable,
            diagnosis: diagnosis.map(str::to_owned),
        }
    }

    /// Re-read route and format properties on the controller thread. Apple’s
    /// AudioHardware headers define these properties as observable, but a
    /// bounded poll avoids placing property queries on the realtime IOProc.
    pub(super) fn verify_format_invariant(&self) -> Result<(), Mt0Error> {
        let fail = |detail: &str| {
            self.health.rate_validator.lock().note_format_change();
            Mt0Error::new(
                Mt0ErrorCode::AudioFormatChanged,
                format!("{} capture format changed: {detail}", self.label),
            )
        };
        if let Some(selector) = self.health.default_device_selector {
            let current_default = default_device(selector, "default audio route disappeared")
                .map_err(|_| fail("default device route became unavailable"))?;
            if current_default != self.health.device_id {
                return Err(fail("default device route changed"));
            }
        }
        let description = input_stream_format(self.device, self.label)
            .map_err(|_| fail("stream format became unavailable"))?;
        let advertised = validate_stream_format(self.label, &description)
            .map_err(|_| fail("stream format became unsupported"))?;
        if advertised != self.health.advertised_sample_rate {
            return Err(fail("advertised sample rate changed"));
        }
        let channels = input_stream_channels(self.device);
        if channels != self.health.layout_channels {
            return Err(fail("channel layout changed"));
        }
        let current_layout = buffer_layout(self.label, self.format, &channels)
            .map_err(|_| fail("channel layout became unsupported"))?;
        if current_layout.buffers != self.layout.buffers
            || current_layout.bytes_per_frame_per_buffer != self.layout.bytes_per_frame_per_buffer
            || current_layout.planar != self.layout.planar
        {
            return Err(fail("channel layout changed"));
        }
        if self.label == "system audio" {
            let nominal = sample_rate_property(
                self.health.device_id,
                kAudioDevicePropertyNominalSampleRate,
                "read output-device nominal sample rate",
            )
            .map_err(|_| fail("output clock rate became unavailable"))?;
            if nominal != self.format.sample_rate {
                return Err(fail("output clock rate changed"));
            }
        }
        Ok(())
    }

    /// Stop delivering audio without tearing the device down.
    ///
    /// This genuinely stops IO rather than gating the callback, so the macOS
    /// microphone indicator goes out while a meeting is paused.
    pub(super) fn pause(&mut self) -> Result<(), Mt0Error> {
        if !self.running {
            return Ok(());
        }
        // SAFETY: both ids are live for the lifetime of `self`.
        let status = unsafe { AudioDeviceStop(self.device, self.proc_id) };
        check(
            status,
            Mt0ErrorCode::TeardownFailed,
            &format!("pause {} capture device", self.label),
        )?;
        self.running = false;
        Ok(())
    }

    /// Re-arm the anchor at `resume_at_ns` and restart IO.
    ///
    /// **Do not reintroduce a device-clock read here.** This used to call
    /// `AudioDeviceGetCurrentTime` immediately after `AudioDeviceStart`, on the
    /// reasoning that the clock is only readable once the device is running.
    /// The premise is right and the conclusion is wrong: `AudioDeviceStart`
    /// *returns* before the HAL has the device's IO thread going, so the read
    /// raced the start and lost, answering `kAudioHardwareNotRunningError`
    /// (`'stop'`) — and because that mapped to `SystemAudioStartFailed`, a
    /// perfectly healthy meeting was terminalised on the user's first press of
    /// resume. See debug-log Bug 8.
    ///
    /// Every source in the capture is armed with the *same* `resume_at_ns`,
    /// which is what keeps them on one timeline across a pause. The caller
    /// keeps the shared `paused` gate closed across the whole transaction, so
    /// no callback can map a sample against the stale anchor in between.
    pub(super) fn resume(&mut self, resume_at_ns: i64) -> Result<(), Mt0Error> {
        // SAFETY: `state` is alive until `teardown`, and the IOProc only reads
        // this through atomics.
        let state = unsafe { &*self.state };
        state.anchor.arm_resume(resume_at_ns);
        state.health.rate_validator.lock().reset();

        if !self.running {
            // SAFETY: both ids are live for the lifetime of `self`.
            let status = unsafe { AudioDeviceStart(self.device, self.proc_id) };
            check(
                status,
                Mt0ErrorCode::SystemAudioStartFailed,
                &format!("resume {} capture device", self.label),
            )?;
            self.running = true;
        }
        Ok(())
    }

    /// Stop IO and destroy everything, exactly once. Safe to call repeatedly.
    pub(super) fn stop(&mut self) -> Result<(), Mt0Error> {
        self.teardown()
    }

    fn teardown(&mut self) -> Result<(), Mt0Error> {
        if self.torn_down {
            return Ok(());
        }
        self.torn_down = true;

        let mut first_error = None;
        let mut note = |status: i32, what: String| {
            if let Err(error) = check(status, Mt0ErrorCode::TeardownFailed, &what) {
                first_error.get_or_insert(error);
            }
        };

        if self.running {
            // SAFETY: `proc_id` was created against this device and is live.
            note(
                unsafe { AudioDeviceStop(self.device, self.proc_id) },
                format!("stop {} capture device", self.label),
            );
            self.running = false;
        }
        // SAFETY: as above; destroying after stopping is the documented order.
        // This is also the barrier that guarantees no callback is in flight.
        note(
            unsafe { AudioDeviceDestroyIOProcID(self.device, self.proc_id) },
            format!("destroy {} IOProc", self.label),
        );
        if let Some(aggregate) = self.owned.aggregate {
            // SAFETY: created by AudioHardwareCreateAggregateDevice, once.
            note(
                unsafe { AudioHardwareDestroyAggregateDevice(aggregate) },
                format!("destroy {} aggregate device", self.label),
            );
        }
        if let Some(tap) = self.owned.tap {
            // SAFETY: created by AudioHardwareCreateProcessTap, once.
            note(
                unsafe { AudioHardwareDestroyProcessTap(tap) },
                format!("destroy {} process tap", self.label),
            );
        }

        // SAFETY: the IOProc is stopped and destroyed, so nothing else holds
        // this pointer.
        drop(unsafe { Box::from_raw(self.state) });

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for AudioSource {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

/// Destroys the objects a half-built source claimed, unless it got as far as
/// owning them itself.
struct SourceObjectGuard(OwnedObjects);

impl SourceObjectGuard {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for SourceObjectGuard {
    fn drop(&mut self) {
        if let Some(aggregate) = self.0.aggregate {
            // SAFETY: created by AudioHardwareCreateAggregateDevice, once.
            unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
        }
        if let Some(tap) = self.0.tap {
            // SAFETY: created by AudioHardwareCreateProcessTap, once.
            unsafe { AudioHardwareDestroyProcessTap(tap) };
        }
    }
}

// ---------------------------------------------------------------------------
// The two sources a meeting capture runs
// ---------------------------------------------------------------------------

/// Start capturing the default input device directly.
///
/// No aggregate device: it is one device with one input stream, and wrapping it
/// in an aggregate would only re-couple it to something else's clock. That does
/// mean `kAudioSubDeviceDriftCompensationKey` no longer applies to the
/// microphone — it is not in an aggregate to be compensated *within*. Frame-
/// domain anchoring keeps each source exact in its own clock domain; genuine
/// hardware drift between two crystals is a separate question, and §9's
/// external-microphone test is what answers it.
pub(super) fn start_microphone_source(
    sink: TrackSink,
    shared: SharedCaptureState,
) -> Result<AudioSource, Mt0Error> {
    let device = default_device(
        objc2_core_audio::kAudioHardwarePropertyDefaultInputDevice,
        "no default input device is available",
    )?;
    let description = input_stream_format(device, "microphone")?;
    let advertised_sample_rate = validate_stream_format("microphone", &description)?;
    let channels = input_stream_channels(device);
    let total_channels = channels.iter().copied().sum::<u32>();
    let format = pcm_format(&description, total_channels)?;
    let name = device_name(device).unwrap_or_else(|_| "unavailable".into());
    #[cfg(feature = "meeting-capture-debug")]
    {
        crate::murmur_diag!(
            "[murmur][audio-validation] source=microphone device_id={device} device_name={name:?} rate_hz={} channels={} bits={} bytes_per_frame={} flags={:#x}",
            format.sample_rate,
            format.channels,
            format.bits_per_channel,
            format.bytes_per_frame,
            format.flags
        );
    }

    AudioSource::start(
        "microphone",
        device,
        OwnedObjects::default(),
        SourceConfiguration {
            format,
            advertised_sample_rate,
            health_device_id: device,
            health_device_name: name,
            default_device_selector: Some(
                objc2_core_audio::kAudioHardwarePropertyDefaultInputDevice,
            ),
        },
        &channels,
        sink,
        shared,
    )
}

/// The sources one capture is running, and the operations that apply to all of
/// them at once.
///
/// Pause, resume and teardown are capture-level rather than device-level: every
/// source is gated by the same `paused` flag and re-armed at the same point on
/// the timeline, which is what keeps them together across a pause.
pub(super) struct CaptureSources {
    pub microphone: AudioSource,
    pub system: AudioSource,
}

impl CaptureSources {
    pub(super) fn health_snapshot(&self) -> Vec<CaptureSourceHealth> {
        vec![
            self.microphone.health_snapshot("microphone"),
            self.system.health_snapshot("system"),
        ]
    }

    pub(super) fn verify_format_invariant(&self) -> Result<(), Mt0Error> {
        self.microphone.verify_format_invariant()?;
        self.system.verify_format_invariant()
    }

    fn all_mut(&mut self) -> [&mut AudioSource; 2] {
        [&mut self.microphone, &mut self.system]
    }

    /// Stop device IO on every source, keeping the devices themselves alive so
    /// resume stays cheap.
    pub(super) fn pause(&mut self) -> Result<(), Mt0Error> {
        let mut first_error = None;
        for source in self.all_mut() {
            if let Err(error) = source.pause() {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Re-arm every source at `resume_at_ns` and restart its IO.
    pub(super) fn resume(&mut self, resume_at_ns: i64) -> Result<(), Mt0Error> {
        let mut first_error = None;
        for source in self.all_mut() {
            if let Err(error) = source.resume(resume_at_ns) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Tear every source down, exactly once each. Reports the first failure but
    /// never skips the remaining teardowns because of it — a leaked aggregate
    /// device or tap outlives the capture.
    pub(super) fn stop(&mut self) -> Result<(), Mt0Error> {
        let mut first_error = None;
        for source in self.all_mut() {
            if let Err(error) = source.stop() {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

// ---------------------------------------------------------------------------
// The IOProc
// ---------------------------------------------------------------------------

/// Runs on a CoreAudio realtime thread.
///
/// It allocates (building the `AudioChunk` plane) and takes the clock lock,
/// which is the same work the ScreenCaptureKit callback did — but that ran on a
/// dispatch queue, and this does not. The buffers are small (512 frames)
/// against a ~10.6 ms deadline and the clock lock is held only by 1 Hz status
/// polling, so this is fine in practice and was clean across the Phase A runs.
/// If long-duration testing ever shows dropouts, the fix is a preallocated ring
/// buffer drained by a worker thread, not shaving work off here.
///
/// There are now two of these running concurrently on different threads. The
/// only state they share is the timeline origin (one compare-exchange, on the
/// first callback only) and the clock mutex.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input_data: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    _output_data: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data: *mut c_void,
) -> i32 {
    // SAFETY: `client_data` is the `SourceIoState` handed to
    // AudioDeviceCreateIOProcID, alive until AudioDeviceDestroyIOProcID.
    let Some(state) = (unsafe { client_data.cast::<SourceIoState>().as_ref() }) else {
        return 0;
    };
    if state.shared.paused.load(Ordering::Acquire) {
        return 0;
    }

    // SAFETY: CoreAudio guarantees the buffer list is valid for this call.
    let list = unsafe { input_data.as_ref() };
    let buffer_count = list.mNumberBuffers as usize;
    if buffer_count != state.layout.buffers {
        set_fatal(
            &state.shared.fatal,
            Mt0Error::new(
                Mt0ErrorCode::UnsupportedAudioFormat,
                format!(
                    "capture device delivered {buffer_count} input streams, expected {}",
                    state.layout.buffers
                ),
            ),
        );
        return 0;
    }
    // SAFETY: `mBuffers` is a C flexible array member of `mNumberBuffers`
    // entries laid out immediately after the count.
    let buffers = unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), buffer_count) };

    let bytes_per_frame = state.layout.bytes_per_frame_per_buffer;
    let mut frames = 0usize;
    for buffer in buffers {
        let size = buffer.mDataByteSize as usize;
        // A partial frame means the stream is not the format we validated at
        // start — most likely a device reconfiguration. Truncating would
        // silently misalign every later frame on this track, so stop instead.
        if !size.is_multiple_of(bytes_per_frame) {
            set_fatal(
                &state.shared.fatal,
                Mt0Error::new(
                    Mt0ErrorCode::AudioFormatChanged,
                    format!(
                        "capture stream delivered {size} bytes, not a whole number of \
                         {bytes_per_frame}-byte frames"
                    ),
                ),
            );
            return 0;
        }
        frames = frames.max(size / bytes_per_frame);
    }
    if frames == 0 {
        return 0;
    }

    // SAFETY: CoreAudio guarantees the timestamp is valid for this call.
    let time = unsafe { input_time.as_ref() };
    let sample_time = time.mSampleTime as i64;
    // `mHostTime` is what places this source relative to the others, and it is
    // read once per anchor rather than per callback. A device that declines to
    // report it is rare enough that reading the clock ourselves is a better
    // answer than failing the capture — the anchor only needs to know roughly
    // when this callback happened, to within the jitter it already tolerates.
    let host_ns = if time.mFlags.contains(AudioTimeStampFlags::HostTimeValid) {
        host_time_to_ns(time.mHostTime)
    } else {
        // SAFETY: `mach_absolute_time` takes no arguments and cannot fail.
        host_time_to_ns(unsafe { mach_absolute_time() })
    };
    {
        let mut validator = state.health.rate_validator.lock();
        validator.note_callback(host_ns);
        if time
            .mFlags
            .contains(AudioTimeStampFlags::SampleHostTimeValid)
        {
            if let Some(observed_rate) =
                validator.observe(sample_time, host_ns, state.format.sample_rate)
            {
                crate::murmur_diag!(
                "[murmur][audio-validation] source={} observed_rate_hz={:.0} expected_rate_hz={} terminal=sample_rate_mismatch",
                state.label,
                observed_rate,
                state.format.sample_rate
            );
                set_fatal(
                    &state.shared.fatal,
                    Mt0Error::new(
                        Mt0ErrorCode::AudioFormatChanged,
                        format!(
                            "{} capture clock delivered approximately {:.0} Hz, expected {} Hz",
                            state.label, observed_rate, state.format.sample_rate
                        ),
                    ),
                );
                return 0;
            }
        }
    }

    let pts_ns = state
        .anchor
        .map_frames(&state.shared.origin, host_ns, sample_time, frames);

    let mapping = {
        let mut clock = state.shared.clock.lock();
        clock.map_sample(
            pts_ns,
            frames_to_ns(frames as i64, state.format.sample_rate),
            frames,
            state.format.sample_rate,
        )
    };
    let Some(mapping) = mapping else {
        return 0;
    };
    if mapping.hard_limit_reached {
        state
            .shared
            .hard_limit_reached
            .store(true, Ordering::Release);
    }
    if mapping.frames == 0 {
        return 0;
    }

    // The clock may have clipped this cycle at the hard limit; honour that
    // rather than the buffer's own length.
    let wanted = mapping.frames.saturating_mul(bytes_per_frame);
    if wanted.saturating_mul(buffer_count) > MAX_CALLBACK_BYTES {
        // Carried over from the ScreenCaptureKit path: a buffer this size
        // means the device reconfigured under us, not a real cycle.
        set_fatal(
            &state.shared.fatal,
            Mt0Error::new(
                Mt0ErrorCode::CallbackBufferTooLarge,
                format!(
                    "capture callback delivered {} bytes in one cycle",
                    wanted.saturating_mul(buffer_count)
                ),
            ),
        );
        return 0;
    }

    let mut planes = Vec::with_capacity(buffer_count);
    for buffer in buffers {
        let available = buffer.mDataByteSize as usize;
        let take = wanted.min(available);
        let mut plane = vec![0u8; wanted];
        if !buffer.mData.is_null() && take > 0 {
            // SAFETY: `mData` holds at least `mDataByteSize` bytes, and `take`
            // never exceeds it.
            let source = unsafe { std::slice::from_raw_parts(buffer.mData.cast::<u8>(), take) };
            plane[..take].copy_from_slice(source);
        }
        // A stream with no data still reports the byte count it would have
        // produced, so the zeroed tail keeps the track's frame count honest.
        planes.push(plane);
    }
    let plane_channels = if state.layout.planar {
        vec![1u32; buffer_count]
    } else {
        vec![u32::from(state.format.channels)]
    };

    let chunk = AudioChunk {
        format: state.format,
        frames: mapping.frames,
        plane_channels,
        planes,
        pts_ns: mapping.mapped_pts_ns,
        origin_ns: 0,
    };

    let sink = &state.sink;
    let chunk_frames = chunk.frames as u64;
    let chunk_bytes = chunk.planes.iter().map(Vec::len).sum::<usize>() as u64;
    let chunk_pts = chunk.pts_ns;
    if sink.destination.is_recorder() {
        match sink.destination.try_send(chunk) {
            Ok(()) => {
                sink.stats.frames.fetch_add(chunk_frames, Ordering::Relaxed);
                sink.stats
                    .input_bytes
                    .fetch_add(chunk_bytes, Ordering::Relaxed);
                sink.stats.note_pts(chunk_pts);
            }
            Err(error) => set_fatal(&state.shared.fatal, error),
        }
    } else {
        // Increment before publishing so a fast writer can never decrement a
        // zero queue counter.
        let queued = sink.stats.note_enqueued();
        match sink.destination.try_send(chunk) {
            Ok(()) => sink.stats.note_enqueued_success(queued),
            Err(error) => {
                sink.stats.note_dequeued();
                set_fatal(&state.shared.fatal, error);
            }
        }
    }
    0
}

fn set_fatal(slot: &Mutex<Option<Mt0Error>>, error: Mt0Error) {
    let mut slot = slot.lock();
    if slot.is_none() {
        *slot = Some(error);
    }
}

/// `mach_time.h`. Declared here rather than pulled in with a crate: two symbols
/// from libSystem, which is linked into every macOS binary already, is not
/// worth a runtime dependency (`libc` is a dev-dependency in this project).
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    fn mach_absolute_time() -> u64;
}

/// `mach_absolute_time` ticks → nanoseconds.
///
/// The ratio is 1:1 on Apple Silicon, which is the only architecture Murmur
/// ships (see the plan's A6), but reading the timebase is cheap and assuming it
/// is not.
fn host_time_to_ns(host_time: u64) -> i64 {
    static TIMEBASE: OnceLock<(u128, u128)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: `info` is a live out-pointer of the declared type.
        if unsafe { mach_timebase_info(&mut info) } != 0 || info.denom == 0 {
            (1, 1)
        } else {
            (u128::from(info.numer), u128::from(info.denom))
        }
    });
    i64::try_from(u128::from(host_time).saturating_mul(numer) / denom).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// Device format and layout
// ---------------------------------------------------------------------------

/// How the IOProc's input buffer list will be laid out, derived from the
/// device's own stream configuration rather than assumed.
fn buffer_layout(
    label: &str,
    format: PcmFormat,
    channels_per_buffer: &[u32],
) -> Result<BufferLayout, Mt0Error> {
    let planar = format.flags & FLAG_IS_NON_INTERLEAVED != 0;
    let unsupported = |detail: String| {
        Err(Mt0Error::new(
            Mt0ErrorCode::UnsupportedAudioFormat,
            format!("{label} capture device: {detail}"),
        ))
    };
    if channels_per_buffer.is_empty() {
        return unsupported("device exposes no input streams".into());
    }
    if planar {
        // One single-channel buffer per channel is the only planar layout the
        // recorder's frame writer understands, and the only one CoreAudio
        // produces for a non-interleaved stream.
        if channels_per_buffer.iter().any(|count| *count != 1)
            || channels_per_buffer.len() != usize::from(format.channels)
        {
            return unsupported(format!(
                "non-interleaved layout {channels_per_buffer:?} does not match \
                 {} single-channel streams",
                format.channels
            ));
        }
    } else if channels_per_buffer.len() != 1 {
        return unsupported(format!(
            "interleaved audio arrived as {} separate streams",
            channels_per_buffer.len()
        ));
    }
    let bytes_per_frame_per_buffer = usize::try_from(format.bytes_per_frame)
        .ok()
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| {
            Mt0Error::new(
                Mt0ErrorCode::UnsupportedAudioFormat,
                format!("{label} capture device reported a zero-length frame"),
            )
        })?;
    Ok(BufferLayout {
        buffers: channels_per_buffer.len(),
        bytes_per_frame_per_buffer,
        planar,
    })
}

/// Build a `PcmFormat` from a device's stream description.
///
/// `channels` comes from the aggregate's or device's own stream layout rather
/// than from the description, because the two can disagree — the tap describes
/// a stereo mix while a microphone sub-device is mono.
pub(super) fn pcm_format(
    format: &AudioStreamBasicDescription,
    channels: u32,
) -> Result<PcmFormat, Mt0Error> {
    let channels = u16::try_from(channels)
        .ok()
        .filter(|count| *count > 0)
        .ok_or_else(|| {
            Mt0Error::new(
                Mt0ErrorCode::UnsupportedAudioFormat,
                format!("capture stream reported an unusable channel count ({channels})"),
            )
        })?;
    let bytes_per_sample = format.mBitsPerChannel / 8;
    let bytes_per_frame = if format.mFormatFlags & FLAG_IS_NON_INTERLEAVED != 0 {
        bytes_per_sample
    } else {
        u32::from(channels) * bytes_per_sample
    };
    Ok(PcmFormat {
        sample_rate: format.mSampleRate.round() as u32,
        channels,
        format_id: format.mFormatID,
        flags: format.mFormatFlags,
        bits_per_channel: format.mBitsPerChannel,
        bytes_per_frame,
    })
}

/// Reject anything the writers cannot turn into 16-bit PCM before a single
/// frame is captured, rather than failing per callback.
pub(super) fn validate_stream_format(
    label: &str,
    format: &AudioStreamBasicDescription,
) -> Result<u32, Mt0Error> {
    let unsupported = |detail: String| {
        Mt0Error::new(
            Mt0ErrorCode::UnsupportedAudioFormat,
            format!("{label} capture device {detail}"),
        )
    };
    let rate = format.mSampleRate;
    if !rate.is_finite() || rate <= 0.0 || rate > f64::from(u32::MAX) {
        return Err(unsupported(format!(
            "reported an unusable sample rate ({rate})"
        )));
    }
    if format.mFormatFlags & FLAG_IS_FLOAT == 0 || format.mBitsPerChannel != 32 {
        return Err(unsupported(format!(
            "is not 32-bit float ({} bits, flags {:#x})",
            format.mBitsPerChannel, format.mFormatFlags
        )));
    }
    if format.mFormatFlags & FLAG_IS_PACKED == 0 {
        return Err(unsupported(format!(
            "delivered unpacked audio (flags {:#x})",
            format.mFormatFlags
        )));
    }
    Ok(rate.round() as u32)
}

/// The virtual format of a device's first input stream.
///
/// The tap's format comes from the tap object itself; every other device has to
/// be asked what it is actually going to deliver, which is what this is for.
pub(super) fn input_stream_format(
    device: AudioObjectID,
    label: &str,
) -> Result<AudioStreamBasicDescription, Mt0Error> {
    let streams = input_streams(device);
    let stream = *streams.first().ok_or_else(|| {
        Mt0Error::new(
            Mt0ErrorCode::SourceUnavailable,
            format!("{label} capture device exposes no input streams"),
        )
    })?;

    let mut addr = address(kAudioStreamPropertyVirtualFormat);
    let mut size = size_of::<AudioStreamBasicDescription>() as u32;
    // SAFETY: ASBD is a `repr(C)` plain-data struct that CoreAudio overwrites.
    let mut format: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
    // SAFETY: `size` matches the storage `format` provides.
    let status = unsafe {
        AudioObjectGetPropertyData(
            stream,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut format).cast::<c_void>(),
        )
    };
    check(
        status,
        Mt0ErrorCode::UnsupportedAudioFormat,
        &format!("read {label} capture format"),
    )?;
    Ok(format)
}

fn input_streams(device: AudioObjectID) -> Vec<AudioObjectID> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreams,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = 0u32;
    // SAFETY: `size` is a live out-pointer; this call only measures.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let count = size as usize / size_of::<AudioObjectID>();
    let mut streams = vec![0 as AudioObjectID; count];
    // SAFETY: `streams` provides exactly `size` bytes of storage.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(streams.as_mut_ptr())
                .expect("non-empty storage")
                .cast::<c_void>(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    streams
}

/// Channels per input stream, in buffer-list order — i.e. exactly how the
/// IOProc's buffers will be laid out.
pub(super) fn input_stream_channels(device: AudioObjectID) -> Vec<u32> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = 0u32;
    // SAFETY: `size` is a live out-pointer; this call only measures.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let words = (size as usize).div_ceil(size_of::<usize>());
    let mut storage = vec![0usize; words];
    let list = storage.as_mut_ptr().cast::<AudioBufferList>();
    // SAFETY: `storage` provides at least `size` bytes and `list` points at it.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(list)
                .expect("non-empty storage")
                .cast::<c_void>(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    // SAFETY: CoreAudio filled `list` with a valid buffer list.
    unsafe {
        let count = (*list).mNumberBuffers as usize;
        std::slice::from_raw_parts((*list).mBuffers.as_ptr(), count)
            .iter()
            .map(|buffer| buffer.mNumberChannels)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Small CoreAudio helpers, shared by every source
// ---------------------------------------------------------------------------

pub(super) fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

pub(super) fn default_device(selector: u32, missing: &str) -> Result<AudioObjectID, Mt0Error> {
    let mut addr = address(selector);
    let mut size = size_of::<AudioObjectID>() as u32;
    let mut device: AudioObjectID = 0;
    // SAFETY: `device` provides `size` bytes and every pointer is live.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut device).cast::<c_void>(),
        )
    };
    check(
        status,
        Mt0ErrorCode::SystemAudioStartFailed,
        "read default audio device",
    )?;
    if device == 0 {
        return Err(Mt0Error::new(
            Mt0ErrorCode::SourceUnavailable,
            missing.to_string(),
        ));
    }
    Ok(device)
}

pub(super) fn sample_rate_property(
    device: AudioObjectID,
    selector: u32,
    what: &str,
) -> Result<u32, Mt0Error> {
    let mut addr = address(selector);
    let mut size = size_of::<f64>() as u32;
    let mut rate = 0.0_f64;
    // SAFETY: `rate` provides exactly `size` bytes for this synchronous read.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut rate).cast::<c_void>(),
        )
    };
    check(status, Mt0ErrorCode::UnsupportedAudioFormat, what)?;
    if !rate.is_finite() || rate <= 0.0 || rate > f64::from(u32::MAX) {
        return Err(Mt0Error::new(
            Mt0ErrorCode::UnsupportedAudioFormat,
            format!("{what}: unusable sample rate ({rate})"),
        ));
    }
    Ok(rate.round() as u32)
}

pub(super) fn device_uid(device: AudioObjectID) -> Result<String, Mt0Error> {
    string_property(
        device,
        kAudioDevicePropertyDeviceUID,
        "read audio device identifier",
    )
}

pub(super) fn device_name(device: AudioObjectID) -> Result<String, Mt0Error> {
    string_property(device, kAudioObjectPropertyName, "read audio device name")
}

/// CoreAudio returns `CFStringRef` properties +1, and NSString is toll-free
/// bridged to CFString, so `Retained::from_raw` adopts the reference exactly
/// once. Anything else leaks.
pub(super) fn string_property(
    object: AudioObjectID,
    selector: u32,
    what: &str,
) -> Result<String, Mt0Error> {
    let mut addr = address(selector);
    let mut size = size_of::<*mut NSString>() as u32;
    let mut raw: *mut NSString = std::ptr::null_mut();
    // SAFETY: `raw` provides `size` bytes and every pointer is live.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut raw).cast::<c_void>(),
        )
    };
    check(status, Mt0ErrorCode::SystemAudioStartFailed, what)?;
    // SAFETY: CoreAudio handed back a +1 CFString, toll-free bridged to
    // NSString, so this adopts the existing reference exactly once.
    let string = unsafe { Retained::from_raw(raw) }.ok_or_else(|| {
        Mt0Error::new(
            Mt0ErrorCode::SystemAudioStartFailed,
            format!("{what}: CoreAudio returned no value"),
        )
    })?;
    Ok(string.to_string())
}

pub(super) fn key(raw: &CStr) -> Retained<NSString> {
    NSString::from_str(raw.to_str().expect("CoreAudio dictionary keys are ASCII"))
}

pub(super) fn check(status: i32, code: Mt0ErrorCode, what: &str) -> Result<(), Mt0Error> {
    if status == 0 {
        return Ok(());
    }
    // CoreAudio OSStatus values are usually FourCC. 'nope' is the one worth
    // naming: it means the audio-capture TCC grant is missing or was denied.
    let fourcc = status.to_be_bytes();
    let detail = if fourcc.iter().all(|b| b.is_ascii_graphic()) {
        format!("'{}'", String::from_utf8_lossy(&fourcc))
    } else {
        status.to_string()
    };
    let code = if &fourcc == b"nope" {
        Mt0ErrorCode::SystemAudioPermissionDenied
    } else {
        code
    };
    Err(Mt0Error::new(
        code,
        format!("could not {what} (CoreAudio {detail})"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asbd(bits: u32, flags: u32) -> AudioStreamBasicDescription {
        // SAFETY: ASBD is a `repr(C)` plain-data struct.
        let mut format: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
        format.mSampleRate = 48_000.0;
        format.mBitsPerChannel = bits;
        format.mFormatFlags = flags;
        format
    }

    const FLOAT: u32 = FLAG_IS_FLOAT | FLAG_IS_PACKED;

    #[test]
    fn stream_format_accepts_packed_float_and_rejects_the_rest() {
        assert_eq!(
            validate_stream_format("mic", &asbd(32, FLOAT)).unwrap(),
            48_000
        );
        assert!(validate_stream_format("mic", &asbd(16, FLAG_IS_PACKED)).is_err());
        assert!(validate_stream_format("mic", &asbd(32, FLAG_IS_FLOAT)).is_err());
    }

    /// Non-interleaved audio is now reachable: the microphone runs on its own
    /// device rather than through an aggregate that normalised the layout.
    #[test]
    fn pcm_format_sizes_a_frame_by_layout_not_by_channel_count_alone() {
        let interleaved = pcm_format(&asbd(32, FLOAT), 2).unwrap();
        assert_eq!(interleaved.bytes_per_frame, 8);
        let planar = pcm_format(&asbd(32, FLOAT | FLAG_IS_NON_INTERLEAVED), 2).unwrap();
        // One buffer per channel, so a "frame" in each buffer is one sample.
        assert_eq!(planar.bytes_per_frame, 4);
    }

    #[test]
    fn buffer_layout_matches_the_devices_own_stream_configuration() {
        let interleaved = pcm_format(&asbd(32, FLOAT), 2).unwrap();
        let layout = buffer_layout("system", interleaved, &[2]).unwrap();
        assert_eq!(layout.buffers, 1);
        assert_eq!(layout.bytes_per_frame_per_buffer, 8);
        assert!(!layout.planar);

        let planar = pcm_format(&asbd(32, FLOAT | FLAG_IS_NON_INTERLEAVED), 2).unwrap();
        let layout = buffer_layout("mic", planar, &[1, 1]).unwrap();
        assert_eq!(layout.buffers, 2);
        assert_eq!(layout.bytes_per_frame_per_buffer, 4);
        assert!(layout.planar);
    }

    /// A layout the frame writer cannot represent has to be refused at start,
    /// not discovered one packet at a time on the audio thread.
    #[test]
    fn buffer_layout_refuses_layouts_the_writer_cannot_represent() {
        let interleaved = pcm_format(&asbd(32, FLOAT), 2).unwrap();
        assert!(buffer_layout("mic", interleaved, &[]).is_err());
        assert!(buffer_layout("mic", interleaved, &[1, 1]).is_err());

        let planar = pcm_format(&asbd(32, FLOAT | FLAG_IS_NON_INTERLEAVED), 2).unwrap();
        assert!(buffer_layout("mic", planar, &[2]).is_err());
        assert!(buffer_layout("mic", planar, &[1]).is_err());
    }

    #[test]
    fn delivered_rate_validation_accepts_the_configured_clock() {
        let mut validator = SampleRateValidator::default();
        assert_eq!(validator.observe(0, 0, 48_000), None);
        // Thirty 10 ms callbacks cross the 250 ms validation window.
        for cycle in 1..30 {
            assert_eq!(
                validator.observe(cycle * 480, cycle * 10_000_000, 48_000),
                None
            );
        }
    }

    #[test]
    fn quiet_system_is_valid_but_a_microphone_without_callbacks_is_not() {
        let validator = SampleRateValidator::default();
        assert_eq!(validator.health_verdict("system"), (true, None));
        assert_eq!(
            validator.health_verdict("microphone"),
            (false, Some("microphone_no_callbacks"))
        );
    }

    #[test]
    fn microphone_frames_without_a_proven_clock_are_rejected() {
        let mut validator = SampleRateValidator::default();
        validator.note_callback(1);
        assert_eq!(
            validator.health_verdict("microphone"),
            (false, Some("media_clock_unproven"))
        );
    }

    #[test]
    fn delivered_rate_validation_accepts_normal_16k_and_44k1_routes() {
        for (rate, frames_per_10ms) in [(16_000, 160), (44_100, 441)] {
            let mut validator = SampleRateValidator::default();
            assert_eq!(validator.observe(0, 0, rate), None);
            for cycle in 1..30 {
                assert_eq!(
                    validator.observe(cycle * frames_per_10ms, cycle * 10_000_000, rate),
                    None
                );
            }
            assert_eq!(
                validator.measured_rate.map(|value| value.round() as u32),
                Some(rate)
            );
        }
    }

    #[test]
    fn delivered_rate_validation_rejects_capture_37s_three_to_one_mismatch() {
        let mut validator = SampleRateValidator::default();
        assert_eq!(validator.observe(0, 0, 48_000), None);
        let mut mismatch = None;
        for cycle in 1..30 {
            mismatch = validator.observe(cycle * 160, cycle * 10_000_000, 48_000);
            if mismatch.is_some() {
                break;
            }
        }
        let observed = mismatch.expect("16 kHz delivery must not pass as 48 kHz");
        assert!((observed - 16_000.0).abs() < 1.0);
    }

    #[test]
    fn a_route_or_layout_transition_invalidates_the_source_epoch() {
        let mut validator = SampleRateValidator::default();
        validator.note_format_change();
        assert_eq!(validator.format_change_count, 1);
        assert_eq!(
            validator.health_verdict("system"),
            (false, Some("sample_rate_or_route_changed"))
        );
    }

    #[test]
    fn delivered_rate_validation_resets_after_an_opportunistic_gap() {
        let mut validator = SampleRateValidator::default();
        assert_eq!(validator.observe(0, 0, 48_000), None);
        for cycle in 1..30 {
            assert_eq!(
                validator.observe(cycle * 480, cycle * 10_000_000, 48_000),
                None
            );
        }
        assert!(validator.measured_rate.is_some());

        // A silent process tap may disappear for seconds. Its next callback is
        // a new observation window, not evidence of a rate mismatch, and the
        // pre-gap proof must not authorize a short post-gap format epoch.
        assert_eq!(validator.observe(14_400, 2_000_000_000, 48_000), None);
        assert_eq!(validator.accumulated_frames, 0);
        assert_eq!(validator.accumulated_host_ns, 0);
        assert!(validator.measured_rate.is_none());
    }

    /// Apple Silicon reports a 1:1 timebase, but the conversion must not assume
    /// it — and must never hand the timeline a negative anchor.
    #[test]
    fn host_time_conversion_is_monotonic_and_non_negative() {
        assert_eq!(host_time_to_ns(0), 0);
        assert!(host_time_to_ns(1_000_000) > 0);
        assert!(host_time_to_ns(2_000_000) > host_time_to_ns(1_000_000));
        assert!(host_time_to_ns(u64::MAX) > 0);
    }
}
