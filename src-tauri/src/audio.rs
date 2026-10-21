//! Microphone capture via `cpal`.
//!
//! The audio backend lives on a dedicated OS thread because `cpal::Stream`
//! is `!Send`. Other threads drive the recorder through a command channel.
//!
//! While recording, each cpal callback computes the RMS level of the block
//! and emits it as `murmur://level` at ~30 Hz so the floating indicator can
//! show a real VU meter instead of a fake pulse animation.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use rand_core::{OsRng, RngCore};
use std::sync::Arc;
use tauri::AppHandle;

use crate::audio_devices;
use crate::error::{Error, Result};
use crate::meeting_capture::{AudioCaptureArbiter, AudioCaptureLease, AudioCaptureOwner};
use crate::notify;

/// Minimum gap between `murmur://level` emits. At 48 kHz with typical cpal
/// block sizes (~256–1024 samples) callbacks fire every 5–20 ms; without
/// throttling we'd flood the event bus.
const LEVEL_EMIT_INTERVAL: Duration = Duration::from_millis(33);

/// Multiplier applied to raw RMS before clamping to [0, 1]. Typical speech
/// RMS is ~0.05–0.3, so 4× gives a meter that actually reaches the top on
/// a normal "hello testing" and doesn't look dead on quiet speech.
const LEVEL_GAIN: f32 = 4.0;

/// Hard cap for a single recording buffer. This prevents accidental
/// toggle-lock sessions (or a stuck key) from appending samples forever.
pub const MAX_RECORDING_DURATION: Duration = Duration::from_secs(10 * 60);

/// Stale crash-resilience WAVs at least this old are eligible for removal on
/// app boot. An insertion transaction with a posted event and no detected
/// transaction/restore error removes its WAV earlier; failed or crashed files
/// can persist past this age until Murmur is launched again.
pub const TEMP_WAV_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// A cryptographic nonce makes collisions practically impossible, but creation
/// still retries because `create_new` is the authority when a path exists.
const TEMP_WAV_CREATE_ATTEMPTS: usize = 16;

/// RMS at or below this value is treated as effectively digital silence.
/// `1e-5` is -100 dBFS: deliberately far below ordinary quiet speech and
/// typical microphone noise, while still catching all-zero/dead routes and
/// insignificant floating-point residue without loading Whisper.
pub const NO_SIGNAL_RMS_THRESHOLD: f32 = 1.0e-5;

/// Maximum microphone-name length exposed in UI and native notifications.
/// CoreAudio names are user-controlled, so remove control characters and cap
/// the result before it crosses the capture boundary.
pub const MAX_EFFECTIVE_DEVICE_NAME_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSignal {
    NoFrames,
    NearDigitalSilence,
    Signal,
}

/// A finished capture buffer in the device's native format.
/// Samples are interleaved when `channels > 1`. `effective_device_name` is
/// snapshotted from the exact `cpal::Device` whose stream was opened, so later
/// route or preference changes cannot make failure feedback name the wrong mic.
#[derive(Debug, Clone)]
pub struct RecordedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub effective_device_name: Option<String>,
}

impl RecordedAudio {
    /// Downmix interleaved channels to a single mono track at the original
    /// sample rate. Whisper wants 16 kHz mono; resampling happens separately.
    pub fn to_mono(&self) -> Vec<f32> {
        if self.channels <= 1 {
            return self.samples.clone();
        }
        let ch = self.channels as usize;
        self.samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    }

    pub fn duration_ms(&self) -> u32 {
        if self.sample_rate == 0 || self.channels == 0 {
            return 0;
        }
        let frames = self.samples.len() as u64 / self.channels as u64;
        ((frames * 1000) / self.sample_rate as u64) as u32
    }

    pub fn signal(&self) -> CaptureSignal {
        classify_capture_signal(&self.samples)
    }
}

/// Classify only near-digital silence here. Anything above the conservative
/// floor proceeds to Whisper, which remains responsible for deciding whether
/// audible energy contained usable speech. Non-finite samples do not count as
/// evidence of a live signal.
pub fn classify_capture_signal(samples: &[f32]) -> CaptureSignal {
    if samples.is_empty() {
        return CaptureSignal::NoFrames;
    }

    let mut finite_sum_squares = 0.0_f64;
    for sample in samples.iter().copied().filter(|sample| sample.is_finite()) {
        let sample = f64::from(sample);
        finite_sum_squares += sample * sample;
    }
    let rms = (finite_sum_squares / samples.len() as f64).sqrt() as f32;
    if rms <= NO_SIGNAL_RMS_THRESHOLD {
        CaptureSignal::NearDigitalSilence
    } else {
        CaptureSignal::Signal
    }
}

pub fn sanitize_effective_device_name(name: &str) -> Option<String> {
    let sanitized: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_EFFECTIVE_DEVICE_NAME_CHARS)
        .collect();
    let sanitized = sanitized.trim();
    (!sanitized.is_empty()).then(|| sanitized.to_string())
}

/// Commands the audio thread accepts. Dropping the sender ends the thread.
enum Cmd {
    Start(mpsc::Sender<std::result::Result<CaptureReadiness, String>>),
    Stop(mpsc::Sender<Option<RecordedAudio>>),
    /// Start a lightweight level-only input stream for onboarding's
    /// microphone meter. This never buffers audio and emits on the
    /// separate `murmur://mic-monitor-level` channel so it can't be
    /// confused with a real dictation capture.
    StartMonitor(mpsc::Sender<std::result::Result<(), String>>),
    /// Stop the level-only onboarding monitor and acknowledge stream teardown.
    StopMonitor(mpsc::Sender<()>),
    /// Stop the active capture and discard its buffer. The acknowledgement
    /// keeps the shared audio-owner lease held until the CPAL stream is gone.
    Cancel(mpsc::Sender<()>),
}

/// Per-capture signal: `Stream::play` does not mean a Bluetooth microphone
/// has delivered its first block of audio yet.
#[derive(Clone, Debug)]
pub struct CaptureReadiness {
    active: Arc<AtomicBool>,
    first_callback: Arc<OnceLock<Instant>>,
    first_signal: Arc<OnceLock<Instant>>,
}

impl CaptureReadiness {
    pub(crate) fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
            first_callback: Arc::new(OnceLock::new()),
            first_signal: Arc::new(OnceLock::new()),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn since_first_callback(&self) -> Option<Duration> {
        self.first_callback.get().map(Instant::elapsed)
    }

    /// Track both first frames and first non-near-silent block. The threshold
    /// is far below normal speech: background mic noise may satisfy it before
    /// anyone talks, while noise suppression may keep it below threshold.
    /// Keep this diagnostic-only until device timing shows which event is a
    /// reliable cue trigger.
    pub(crate) fn note_callback(&self, samples: usize, rms: f32) {
        if samples > 0 {
            if self.first_callback.get().is_none() {
                let _ = self.first_callback.set(Instant::now());
            }
            if rms.is_finite() && rms > NO_SIGNAL_RMS_THRESHOLD && self.first_signal.get().is_none()
            {
                let _ = self.first_signal.set(Instant::now());
            }
        }
    }

    pub(crate) fn close(&self) {
        self.active.store(false, Ordering::Release);
    }
}

/// Handle to the audio thread. Cheap to clone.
///
/// Carries the user's preferred input-device name alongside the command
/// channel so Tauri commands can update the selection from any thread
/// without having to own a dedicated message type.
#[derive(Clone)]
pub struct AudioHandle {
    tx: mpsc::Sender<Cmd>,
    preferred_device: Arc<Mutex<Option<String>>>,
    capture_arbiter: Arc<AudioCaptureArbiter>,
    lifecycle: Arc<Mutex<()>>,
    owner: Arc<Mutex<HandleAudioOwner>>,
}

enum HandleAudioOwner {
    Idle,
    Monitor(AudioCaptureLease),
    Capture(AudioCaptureLease),
}

#[derive(Debug, PartialEq, Eq)]
pub enum AudioStartError {
    CaptureBusy,
    Unavailable(String),
}

impl std::fmt::Display for AudioStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CaptureBusy => formatter.write_str("meeting capture active"),
            Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AudioStartError {}

impl AudioHandle {
    pub fn meeting_capture_active(&self) -> bool {
        self.capture_arbiter.meeting_capture_active()
    }

    pub fn start(&self) -> std::result::Result<CaptureReadiness, AudioStartError> {
        let _lifecycle = self.lifecycle.lock();
        let previous = std::mem::replace(&mut *self.owner.lock(), HandleAudioOwner::Idle);
        let lease = match previous {
            HandleAudioOwner::Capture(lease) | HandleAudioOwner::Monitor(lease) => lease,
            HandleAudioOwner::Idle => self
                .capture_arbiter
                .reserve(AudioCaptureOwner::Dictation)
                .map_err(|_| AudioStartError::CaptureBusy)?,
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        let result = self
            .tx
            .send(Cmd::Start(reply_tx))
            .map_err(|_| AudioStartError::Unavailable("audio thread unavailable".to_string()))
            .and_then(|()| {
                reply_rx
                    .recv()
                    .map_err(|_| {
                        AudioStartError::Unavailable(
                            "audio thread closed before capture started".to_string(),
                        )
                    })?
                    .map_err(AudioStartError::Unavailable)
            });
        if result.is_ok() {
            *self.owner.lock() = HandleAudioOwner::Capture(lease);
        }
        result
    }

    pub fn stop(&self) -> Option<RecordedAudio> {
        let _lifecycle = self.lifecycle.lock();
        // `Stop` only tears down dictation capture. Preserve a live monitor
        // and its lease rather than admitting a meeting beside its CPAL stream.
        if matches!(*self.owner.lock(), HandleAudioOwner::Monitor(_)) {
            return None;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let result = if self.tx.send(Cmd::Stop(reply_tx)).is_err() {
            None
        } else {
            reply_rx.recv().ok().flatten()
        };
        *self.owner.lock() = HandleAudioOwner::Idle;
        result
    }

    /// Drop any in-progress capture before releasing shared audio ownership.
    pub fn cancel(&self) {
        let _lifecycle = self.lifecycle.lock();
        // `Cancel` only tears down dictation capture; monitoring has its own
        // acknowledged stop command and must retain shared audio ownership.
        if matches!(*self.owner.lock(), HandleAudioOwner::Monitor(_)) {
            return;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        if self.tx.send(Cmd::Cancel(reply_tx)).is_ok() {
            let _ = reply_rx.recv();
        }
        *self.owner.lock() = HandleAudioOwner::Idle;
    }

    /// Start the onboarding microphone meter. It shares the same process-wide
    /// audio lease as dictation so a meeting cannot open a second mic stream.
    pub fn start_monitor(&self) -> std::result::Result<(), String> {
        let _lifecycle = self.lifecycle.lock();
        if matches!(*self.owner.lock(), HandleAudioOwner::Capture(_)) {
            return Err("recording already active".to_string());
        }
        if matches!(*self.owner.lock(), HandleAudioOwner::Monitor(_)) {
            return Ok(());
        }
        let lease = self
            .capture_arbiter
            .reserve(AudioCaptureOwner::Dictation)
            .map_err(|_| "meeting capture active".to_string())?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let result = self
            .tx
            .send(Cmd::StartMonitor(reply_tx))
            .map_err(|_| "audio thread unavailable".to_string())
            .and_then(|()| {
                reply_rx
                    .recv()
                    .map_err(|_| "audio thread closed before monitor started".to_string())?
            });
        if result.is_ok() {
            *self.owner.lock() = HandleAudioOwner::Monitor(lease);
        }
        result
    }

    /// Stop the onboarding microphone meter before releasing audio ownership.
    pub fn stop_monitor(&self) {
        let _lifecycle = self.lifecycle.lock();
        if !matches!(*self.owner.lock(), HandleAudioOwner::Monitor(_)) {
            return;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        if self.tx.send(Cmd::StopMonitor(reply_tx)).is_ok() {
            let _ = reply_rx.recv();
        }
        *self.owner.lock() = HandleAudioOwner::Idle;
    }

    /// Set the user's preferred input device by cpal name. `None`
    /// reverts to the system default on the next capture.
    pub fn set_preferred_device(&self, name: Option<String>) {
        *self.preferred_device.lock() = name;
    }

    /// Read the current preferred-device name. Cheap clone.
    pub fn preferred_device(&self) -> Option<String> {
        self.preferred_device.lock().clone()
    }
}

/// Spawn the audio thread and return a handle. The thread exits when all
/// handles are dropped. The `AppHandle` is cloned into each new capture so
/// the cpal callback can emit `murmur://level` without cross-thread fuss.
pub fn spawn(app: AppHandle, capture_arbiter: Arc<AudioCaptureArbiter>) -> AudioHandle {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let preferred_device: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let preferred_for_thread = Arc::clone(&preferred_device);
    thread::Builder::new()
        .name("murmur-audio".into())
        .spawn(move || audio_loop(rx, app, preferred_for_thread))
        .expect("spawn audio thread");
    AudioHandle {
        tx,
        preferred_device,
        capture_arbiter,
        lifecycle: Arc::new(Mutex::new(())),
        owner: Arc::new(Mutex::new(HandleAudioOwner::Idle)),
    }
}

#[tauri::command]
pub fn start_mic_monitor(audio: tauri::State<'_, AudioHandle>) -> std::result::Result<(), String> {
    audio.start_monitor()
}

#[tauri::command]
pub fn stop_mic_monitor(audio: tauri::State<'_, AudioHandle>) {
    audio.stop_monitor();
}

fn audio_loop(
    rx: mpsc::Receiver<Cmd>,
    app: AppHandle,
    preferred_device: Arc<Mutex<Option<String>>>,
) {
    let mut capture: Option<CaptureState> = None;
    let mut monitor: Option<LevelMonitor> = None;
    loop {
        let cmd = match capture.as_ref() {
            Some(CaptureState::Active(active)) => {
                let elapsed = active.started_at.elapsed();
                if elapsed >= MAX_RECORDING_DURATION {
                    auto_stop_for_duration_limit(&mut capture, &app);
                    continue;
                }
                match rx.recv_timeout(MAX_RECORDING_DURATION - elapsed) {
                    Ok(cmd) => cmd,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        auto_stop_for_duration_limit(&mut capture, &app);
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            _ => match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            },
        };

        match cmd {
            Cmd::Start(reply) => {
                if let Some(CaptureState::Active(active)) = capture.as_ref() {
                    let _ = reply.send(Ok(active.readiness.clone()));
                    continue;
                }
                // A prior release may have been lost after auto-stop. Do not
                // leave its bounded buffer blocking every subsequent press.
                if discard_auto_stopped(&mut capture) {
                    crate::murmur_dev_log!("[murmur] discarded unclaimed auto-stopped capture");
                }
                // A real dictation capture owns the input meter while it
                // runs. Drop any onboarding monitor first so we don't keep
                // two cpal streams open against the same device.
                if monitor.take().is_some() {
                    crate::window_events::emit_main(&app, "murmur://mic-monitor-level", 0.0_f32);
                }
                let device_name = preferred_device.lock().clone();
                match ActiveCapture::start(app.clone(), device_name.as_deref()) {
                    Ok(c) => {
                        let readiness = c.readiness.clone();
                        capture = Some(CaptureState::Active(c));
                        let _ = reply.send(Ok(readiness));
                    }
                    Err(e) => {
                        let message = e.to_string();
                        crate::murmur_dev_log!("[murmur] audio start failed: {message}");
                        let _ = reply.send(Err(message));
                    }
                }
            }
            Cmd::Stop(reply) => {
                let audio = capture.take().map(CaptureState::finish);
                // Snap the meter to zero once recording ends — the window is
                // hidden anyway, but the level would otherwise stay pinned at
                // whatever the final block measured for the next capture.
                crate::window_events::emit_dictation_ui(&app, "murmur://level", 0.0_f32);
                let _ = reply.send(audio);
            }
            Cmd::StartMonitor(reply) => {
                if capture.is_some() {
                    let _ = reply.send(Err("recording already active".to_string()));
                    continue;
                }
                if monitor.is_some() {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                let device_name = preferred_device.lock().clone();
                match LevelMonitor::start(app.clone(), device_name.as_deref()) {
                    Ok(m) => {
                        monitor = Some(m);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let message = e.to_string();
                        crate::murmur_dev_log!("[murmur] mic monitor start failed: {message}");
                        let _ = reply.send(Err(message));
                    }
                }
            }
            Cmd::StopMonitor(reply) => {
                monitor.take();
                crate::window_events::emit_main(&app, "murmur://mic-monitor-level", 0.0_f32);
                let _ = reply.send(());
            }
            Cmd::Cancel(reply) => {
                // Drop the stream + buffer before releasing the process-wide
                // audio owner in `AudioHandle::cancel`.
                capture.take();
                crate::window_events::emit_dictation_ui(&app, "murmur://level", 0.0_f32);
                let _ = reply.send(());
            }
        }
    }
}

/// Audio-loop state. `AutoStopped` keeps a bounded buffer after the
/// duration cap fires so releasing the hotkey still transcribes what
/// was captured up to the limit, without continuing to grow memory.
enum CaptureState {
    Active(ActiveCapture),
    AutoStopped(RecordedAudio),
}

fn discard_auto_stopped(capture: &mut Option<CaptureState>) -> bool {
    if matches!(capture.as_ref(), Some(CaptureState::AutoStopped(_))) {
        *capture = None;
        true
    } else {
        false
    }
}

impl CaptureState {
    fn finish(self) -> RecordedAudio {
        match self {
            CaptureState::Active(c) => c.finish(),
            CaptureState::AutoStopped(audio) => audio,
        }
    }
}

fn auto_stop_for_duration_limit(capture: &mut Option<CaptureState>, app: &AppHandle) {
    let Some(CaptureState::Active(active)) = capture.take() else {
        return;
    };
    let audio = active.finish();
    *capture = Some(CaptureState::AutoStopped(audio));
    crate::window_events::emit_dictation_ui(app, "murmur://level", 0.0_f32);
    crate::window_events::emit_main(app, "murmur://recording", false);
    crate::window_events::emit_dictation_ui(app, "murmur://indicator-state", "recording-limit");
    notify::error(
        app,
        "Recording stopped",
        "Murmur stopped recording after 10 minutes to keep memory bounded. Release the hotkey to transcribe what was captured.",
    );
}

struct LevelMonitor {
    stream: cpal::Stream,
}

impl LevelMonitor {
    fn start(app: AppHandle, preferred_device: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = resolve_input_device(&host, preferred_device)
            .ok_or_else(|| Error::Backend("no input device available".into()))?;
        if let Ok(name) = device.name() {
            crate::murmur_dev_log!("[murmur] monitoring input level from device: {name}");
        }
        let config = device
            .default_input_config()
            .map_err(|e| Error::Backend(format!("default input config: {e}")))?;
        let err_fn = move |e| {
            crate::murmur_dev_log!("[murmur] input monitor stream error: {e}");
        };
        let initial_last_emit = Instant::now()
            .checked_sub(LEVEL_EMIT_INTERVAL)
            .unwrap_or_else(Instant::now);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let app_cb = app.clone();
                let mut last_emit = initial_last_emit;
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        maybe_emit_monitor_level(&app_cb, &mut last_emit, rms_f32(data));
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let app_cb = app.clone();
                let mut last_emit = initial_last_emit;
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _| {
                        maybe_emit_monitor_level(&app_cb, &mut last_emit, rms_i16(data));
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let app_cb = app.clone();
                let mut last_emit = initial_last_emit;
                device.build_input_stream(
                    &config.into(),
                    move |data: &[u16], _| {
                        maybe_emit_monitor_level(&app_cb, &mut last_emit, rms_u16(data));
                    },
                    err_fn,
                    None,
                )
            }
            fmt => {
                return Err(Error::Backend(format!("unsupported sample format {fmt:?}")));
            }
        }
        .map_err(|e| Error::Backend(format!("build_input_stream: {e}")))?;

        stream
            .play()
            .map_err(|e| Error::Backend(format!("stream play: {e}")))?;

        Ok(Self { stream })
    }
}

impl Drop for LevelMonitor {
    fn drop(&mut self) {
        pause_input_stream(&self.stream, "microphone monitor");
    }
}

struct ActiveCapture {
    buffer: Arc<Mutex<Vec<f32>>>,
    stream: Option<cpal::Stream>,
    sample_rate: u32,
    channels: u16,
    effective_device_name: Option<String>,
    started_at: Instant,
    readiness: CaptureReadiness,
}

impl ActiveCapture {
    fn start(app: AppHandle, preferred_device: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = resolve_input_device(&host, preferred_device)
            .ok_or_else(|| Error::Backend("no input device available".into()))?;
        let effective_device_name = device
            .name()
            .ok()
            .and_then(|name| sanitize_effective_device_name(&name));
        if let Some(name) = effective_device_name.as_deref() {
            crate::murmur_dev_log!("[murmur] capturing from input device: {name}");
        }
        let config = device
            .default_input_config()
            .map_err(|e| Error::Backend(format!("default input config: {e}")))?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let readiness = CaptureReadiness::new();
        let err_app = app.clone();
        let notified_stream_error = Arc::new(AtomicBool::new(false));
        let error_readiness = readiness.clone();
        let err_fn = move |e| {
            error_readiness.close();
            crate::murmur_dev_log!("[murmur] input stream error: {e}");
            if !notified_stream_error.swap(true, Ordering::Relaxed) {
                notify::error(
                    &err_app,
                    "Microphone disconnected",
                    "Murmur lost access to the current microphone. Check your input device and try again.",
                );
            }
        };
        // Start one interval in the past so the first callback actually emits.
        let initial_last_emit = Instant::now()
            .checked_sub(LEVEL_EMIT_INTERVAL)
            .unwrap_or_else(Instant::now);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let buf = Arc::clone(&buffer);
                let ready = readiness.clone();
                let app_cb = app.clone();
                let mut last_emit = initial_last_emit;
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        let rms = rms_f32(data);
                        ready.note_callback(data.len(), rms);
                        buf.lock().extend_from_slice(data);
                        maybe_emit_level(&app_cb, &mut last_emit, rms);
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let buf = Arc::clone(&buffer);
                let ready = readiness.clone();
                let app_cb = app.clone();
                let mut last_emit = initial_last_emit;
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _| {
                        let rms = rms_i16(data);
                        ready.note_callback(data.len(), rms);
                        let mut g = buf.lock();
                        g.extend(data.iter().map(|&s| s as f32 / i16::MAX as f32));
                        drop(g);
                        maybe_emit_level(&app_cb, &mut last_emit, rms);
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let buf = Arc::clone(&buffer);
                let ready = readiness.clone();
                let app_cb = app.clone();
                let mut last_emit = initial_last_emit;
                device.build_input_stream(
                    &config.into(),
                    move |data: &[u16], _| {
                        let rms = rms_u16(data);
                        ready.note_callback(data.len(), rms);
                        let mut g = buf.lock();
                        g.extend(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0));
                        drop(g);
                        maybe_emit_level(&app_cb, &mut last_emit, rms);
                    },
                    err_fn,
                    None,
                )
            }
            fmt => {
                return Err(Error::Backend(format!("unsupported sample format {fmt:?}")));
            }
        }
        .map_err(|e| Error::Backend(format!("build_input_stream: {e}")))?;

        let started_at = Instant::now();
        stream
            .play()
            .map_err(|e| Error::Backend(format!("stream play: {e}")))?;

        Ok(Self {
            buffer,
            stream: Some(stream),
            sample_rate,
            channels,
            effective_device_name,
            started_at,
            readiness,
        })
    }

    fn finish(mut self) -> RecordedAudio {
        // Close the stream before draining: callbacks could previously append
        // after `mem::take`, discarding the final block on key release. Drop
        // also tears it down if pause fails.
        self.readiness.close();
        let stream = self.stream.take().expect("active capture has a stream");
        let samples = take_samples_after_stop(&self.buffer, || {
            pause_input_stream(&stream, "dictation capture");
            drop(stream);
        });
        let frames = samples.len() / usize::from(self.channels.max(1));
        let captured_ms = (frames as u64 * 1000) / u64::from(self.sample_rate.max(1));
        crate::murmur_dev_log!(
            "[murmur] mic capture: wall_ms={} captured_ms={} first_callback_ms={:?} first_signal_ms={:?}",
            self.started_at.elapsed().as_millis(),
            captured_ms,
            self.readiness
                .first_callback
                .get()
                .map(|at| at.duration_since(self.started_at).as_millis()),
            self.readiness
                .first_signal
                .get()
                .map(|at| at.duration_since(self.started_at).as_millis())
        );
        RecordedAudio {
            samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
            effective_device_name: self.effective_device_name.clone(),
        }
    }
}

impl Drop for ActiveCapture {
    fn drop(&mut self) {
        self.readiness.close();
        if let Some(stream) = self.stream.take() {
            pause_input_stream(&stream, "dictation capture");
        }
    }
}

/// Teardown must finish before the final buffer swap. A callback that races
/// with teardown should still be in the returned recording, not left behind.
fn take_samples_after_stop(buffer: &Arc<Mutex<Vec<f32>>>, stop: impl FnOnce()) -> Vec<f32> {
    stop();
    std::mem::take(&mut *buffer.lock())
}

fn pause_input_stream(stream: &cpal::Stream, label: &str) {
    if let Err(e) = stream.pause() {
        crate::murmur_dev_log!("[murmur] failed to pause {label} input stream before drop: {e}");
    }
}

/// A crash-resilience WAV plus an open descriptor for the exact file created.
///
/// On Unix, Whisper reads through `/dev/fd` so a pathname replacement after
/// creation cannot substitute different audio. Cleanup makes a best-effort
/// device/inode check before unlinking; POSIX metadata-check + unlink is not
/// atomic against a malicious same-account process.
pub struct TempWav {
    path: PathBuf,
    file: File,
    transcription_path: PathBuf,
}

impl TempWav {
    fn new(path: PathBuf, file: File) -> Self {
        #[cfg(unix)]
        let transcription_path = {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()))
        };
        #[cfg(not(unix))]
        let transcription_path = path.clone();

        Self {
            path,
            file,
            transcription_path,
        }
    }

    pub fn transcription_path(&self) -> &Path {
        &self.transcription_path
    }

    /// Best-effort removal that checks whether the path still names the created
    /// descriptor before unlinking. This rejects an already-replaced entry, but
    /// a same-account process can still race the separate check and unlink.
    pub fn try_remove_if_same(&self) -> io::Result<bool> {
        let path_metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
            return Ok(false);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let open_metadata = self.file.metadata()?;
            if open_metadata.dev() != path_metadata.dev()
                || open_metadata.ino() != path_metadata.ino()
            {
                return Ok(false);
            }
        }

        std::fs::remove_file(&self.path)?;
        Ok(true)
    }
}

/// Persist raw capture audio for crash resilience using an unpredictable,
/// exclusively-created file in the OS temporary directory. On Unix, the mode
/// is restricted to the current user from the initial create syscall.
pub fn write_temp_wav(audio: &RecordedAudio) -> Result<TempWav> {
    write_temp_wav_in(&std::env::temp_dir(), audio)
}

fn write_temp_wav_in(directory: &Path, audio: &RecordedAudio) -> Result<TempWav> {
    for _ in 0..TEMP_WAV_CREATE_ATTEMPTS {
        let mut nonce = [0_u8; 16];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|error| Error::Backend(format!("temp wav randomness: {error}")))?;
        let nonce = u128::from_le_bytes(nonce);
        let path = directory.join(format!("murmur-{}-{nonce:032x}.wav", std::process::id()));

        match create_restrictive_new_file(&path) {
            Ok(file) => {
                let file = write_wav_file(&path, file, audio)?;
                return Ok(TempWav::new(path, file));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Error::Backend(format!("wav create: {error}"))),
        }
    }

    Err(Error::Backend(
        "wav create: exhausted collision-safe temporary names".to_string(),
    ))
}

fn create_restrictive_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_wav_file(path: &Path, file: File, audio: &RecordedAudio) -> Result<File> {
    let result = (|| {
        let mut identity = file
            .try_clone()
            .map_err(|error| Error::Backend(format!("wav descriptor clone: {error}")))?;
        let spec = hound::WavSpec {
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        // Unlike `create`, Hound's `new` does not buffer its writer.
        let mut writer = hound::WavWriter::new(BufWriter::new(file), spec)
            .map_err(|error| Error::Backend(format!("wav create: {error}")))?;
        for &sample in &audio.samples {
            let clamped = sample.clamp(-1.0, 1.0);
            let value = (clamped * i16::MAX as f32) as i16;
            writer
                .write_sample(value)
                .map_err(|error| Error::Backend(format!("wav write: {error}")))?;
        }
        writer
            .finalize()
            .map_err(|error| Error::Backend(format!("wav finalize: {error}")))?;
        identity
            .seek(SeekFrom::Start(0))
            .map_err(|error| Error::Backend(format!("wav rewind: {error}")))?;
        Ok(identity)
    })();

    remove_partial_on_error(path, result)
}

fn remove_partial_on_error<T>(path: &Path, result: Result<T>) -> Result<T> {
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Remove stale Murmur temp WAVs left by older builds, crashes, failed
/// insertions, or force-quits. Only files older than
/// [`TEMP_WAV_STALE_AFTER`] are touched so an in-flight recording from
/// this process is never removed under the pipeline.
pub fn cleanup_stale_temp_wavs() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("murmur-") || !name.ends_with(".wav") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|age| age >= TEMP_WAV_STALE_AFTER)
            .unwrap_or(false);
        if stale {
            let len = meta.len();
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
                bytes += len;
            }
        }
    }
    if removed > 0 {
        crate::murmur_dev_log!(
            "[murmur] temp wav janitor: removed {removed} stale file(s), {:.1} MB",
            bytes as f64 / 1024.0 / 1024.0
        );
    }
}

/// Resolve the input device to use for a capture. If the user has
/// picked a device by name in Settings and that device is
/// still present, return it. Otherwise Automatic mode prefers USB
/// inputs for Murmur only, then falls back to whatever macOS reports
/// as the current default — safer than erroring when headphones or an
/// external mic get unplugged between captures.
fn resolve_input_device(host: &cpal::Host, preferred: Option<&str>) -> Option<cpal::Device> {
    if let Some(name) = preferred {
        if let Ok(mut devices) = host.input_devices() {
            if let Some(d) = devices.find(|d| d.name().as_deref().ok() == Some(name)) {
                return Some(d);
            }
        }
        crate::murmur_dev_log!(
            "[murmur] preferred input device '{name}' not found; falling back to default"
        );
    } else if let Ok(mut devices) = host.input_devices() {
        if let Some(d) = devices.find(|d| {
            d.name()
                .map(|name| audio_devices::is_preferred_automatic_input(&name))
                .unwrap_or(false)
        }) {
            return Some(d);
        }
    }
    host.default_input_device()
}

/// Emit `murmur://level` at most once per [`LEVEL_EMIT_INTERVAL`]. Applies
/// [`LEVEL_GAIN`] and clamps to `[0, 1]` so the frontend can use it as a
/// transform scale input without extra math.
fn maybe_emit_level(app: &AppHandle, last_emit: &mut Instant, rms: f32) {
    maybe_emit_level_on(app, "murmur://level", last_emit, rms);
}

fn maybe_emit_monitor_level(app: &AppHandle, last_emit: &mut Instant, rms: f32) {
    maybe_emit_level_on(app, "murmur://mic-monitor-level", last_emit, rms);
}

fn maybe_emit_level_on(app: &AppHandle, event: &str, last_emit: &mut Instant, rms: f32) {
    let now = Instant::now();
    if now.duration_since(*last_emit) < LEVEL_EMIT_INTERVAL {
        return;
    }
    *last_emit = now;
    let level = (rms * LEVEL_GAIN).clamp(0.0, 1.0);
    if event == "murmur://level" {
        crate::window_events::emit_dictation_ui(app, event, level);
    } else {
        crate::window_events::emit_main(app, event, level);
    }
}

fn rms_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

fn rms_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let f = s as f64;
            f * f
        })
        .sum();
    let rms = (sum_sq / samples.len() as f64).sqrt();
    (rms / i16::MAX as f64) as f32
}

fn rms_u16(samples: &[u16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let f = s as f64 - 32768.0;
            f * f
        })
        .sum();
    let rms = (sum_sq / samples.len() as f64).sqrt();
    (rms / 32768.0) as f32
}

/// Linear resample a mono f32 track from `src_rate` to `dst_rate`. Not audiophile
/// quality but adequate for Whisper at 16 kHz. Kept in-crate to avoid pulling
/// `rubato` for a single use site.
pub fn resample_linear(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = dst_rate as f64 / src_rate as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = samples[idx.min(samples.len() - 1)];
        let b = samples[(idx + 1).min(samples.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "murmur-audio-test-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock after epoch")
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("create isolated audio test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_audio() -> RecordedAudio {
        RecordedAudio {
            samples: vec![0.0, 0.25, -0.25],
            sample_rate: 16_000,
            channels: 1,
            effective_device_name: None,
        }
    }

    // Frozen test-only copy of the unbuffered writer immediately before this fix.
    fn write_wav_file_unbuffered(path: &Path, file: File, audio: &RecordedAudio) -> Result<File> {
        let result = (|| {
            let mut identity = file
                .try_clone()
                .map_err(|error| Error::Backend(format!("wav descriptor clone: {error}")))?;
            let spec = hound::WavSpec {
                channels: audio.channels,
                sample_rate: audio.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(file, spec)
                .map_err(|error| Error::Backend(format!("wav create: {error}")))?;
            for &sample in &audio.samples {
                let clamped = sample.clamp(-1.0, 1.0);
                let value = (clamped * i16::MAX as f32) as i16;
                writer
                    .write_sample(value)
                    .map_err(|error| Error::Backend(format!("wav write: {error}")))?;
            }
            writer
                .finalize()
                .map_err(|error| Error::Backend(format!("wav finalize: {error}")))?;
            identity
                .seek(SeekFrom::Start(0))
                .map_err(|error| Error::Backend(format!("wav rewind: {error}")))?;
            Ok(identity)
        })();
        remove_partial_on_error(path, result)
    }

    fn fixture(rate: u32, channels: u16, frames: usize) -> RecordedAudio {
        let levels = [-1.25, -1.0, -0.7, -0.25, 0.0, 0.25, 0.7, 1.0, 1.25];
        RecordedAudio {
            samples: (0..frames * channels as usize)
                .map(|i| levels[i % levels.len()])
                .collect(),
            sample_rate: rate,
            channels,
            effective_device_name: None,
        }
    }

    fn assert_wav_samples(path: &Path, audio: &RecordedAudio) {
        let mut reader = hound::WavReader::open(path).expect("open finalized WAV");
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, audio.sample_rate);
        assert_eq!(spec.channels, audio.channels);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
        assert_eq!(
            reader.duration() as usize,
            audio.samples.len() / audio.channels as usize
        );
        let actual: Vec<i16> = reader
            .samples::<i16>()
            .map(|s| s.expect("PCM16 sample"))
            .collect();
        let expected: Vec<i16> = audio
            .samples
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.last(), expected.last());
    }

    #[test]
    fn buffered_wav_tiny_tail_formats_and_multiple_buffers_match_old_bytes() {
        let directory = TestDirectory::new("buffered-equivalence");
        for (rate, channels, frames) in [
            (16_000, 1, 9),
            (48_000, 1, 9),
            (48_000, 2, 9),
            (48_000, 2, 12_345),
        ] {
            let audio = fixture(rate, channels, frames);
            let old_path = directory
                .0
                .join(format!("old-{rate}-{channels}-{frames}.wav"));
            let new_path = directory
                .0
                .join(format!("new-{rate}-{channels}-{frames}.wav"));
            let old_file = write_wav_file_unbuffered(
                &old_path,
                create_restrictive_new_file(&old_path).unwrap(),
                &audio,
            )
            .unwrap();
            let new_file = write_wav_file(
                &new_path,
                create_restrictive_new_file(&new_path).unwrap(),
                &audio,
            )
            .unwrap();
            let temp = TempWav::new(new_path.clone(), new_file);
            assert_wav_samples(temp.transcription_path(), &audio);
            assert_eq!(
                std::fs::read(&old_path).unwrap(),
                std::fs::read(&new_path).unwrap()
            );
            assert!(temp.try_remove_if_same().unwrap());
            drop(old_file);
        }
    }

    #[cfg(unix)]
    #[test]
    fn buffered_write_error_removes_partial_file() {
        let directory = TestDirectory::new("buffered-error");
        let path = directory.0.join("readonly.wav");
        drop(create_restrictive_new_file(&path).unwrap());
        let readonly = File::open(&path).unwrap();
        assert!(write_wav_file(&path, readonly, &sample_audio()).is_err());
        assert!(!path.exists());
    }

    // Nearest-rank percentile: ceil(p * n) - 1 (zero-based).
    fn percentile_ms(values: &[f64], percent: f64) -> f64 {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[((percent * sorted.len() as f64).ceil() as usize).saturating_sub(1)]
    }

    #[test]
    #[ignore = "synthetic disk-write benchmark; run explicitly in an isolated temp directory"]
    fn benchmark_dictation_wav_buffering() {
        let directory = TestDirectory::new("buffer-benchmark");
        let deadline = Instant::now() + Duration::from_secs(600);
        for (rate, seconds) in [(48_000, 3), (16_000, 3), (48_000, 30), (16_000, 30)] {
            let count = rate as usize * seconds;
            let audio = RecordedAudio {
                samples: (0..count)
                    .map(|i| ((i % 997) as f32 - 498.5) / 500.0)
                    .collect(),
                sample_rate: rate,
                channels: 1,
                effective_device_name: None,
            };
            let mut old_ms = Vec::new();
            let mut new_ms = Vec::new();
            let mut equality_checked = false;
            for pair in 0..22 {
                if Instant::now() >= deadline {
                    break;
                }
                let mut paths = [PathBuf::new(), PathBuf::new()];
                let mut times = [0.0; 2];
                let order = if pair % 2 == 0 { [0, 1] } else { [1, 0] };
                for arm in order {
                    let path = directory
                        .0
                        .join(format!("{rate}-{seconds}-{pair}-{arm}.wav"));
                    let start = Instant::now();
                    let file = create_restrictive_new_file(&path).expect("secure create");
                    let descriptor = if arm == 0 {
                        write_wav_file_unbuffered(&path, file, &audio)
                    } else {
                        write_wav_file(&path, file, &audio)
                    }
                    .expect("complete WAV");
                    times[arm] = start.elapsed().as_secs_f64() * 1_000.0;
                    drop(descriptor);
                    paths[arm] = path;
                }
                if !equality_checked {
                    assert_eq!(
                        std::fs::read(&paths[0]).unwrap(),
                        std::fs::read(&paths[1]).unwrap()
                    );
                    equality_checked = true;
                }
                for path in paths {
                    std::fs::remove_file(path).expect("remove owned fixture");
                }
                if pair >= 2 {
                    old_ms.push(times[0]);
                    new_ms.push(times[1]);
                }
            }
            if old_ms.is_empty() {
                println!(
                    "rate={rate} channels=1 seconds={seconds} samples={count} n=0 deadline reached"
                );
                continue;
            }
            let old50 = percentile_ms(&old_ms, 0.50);
            let new50 = percentile_ms(&new_ms, 0.50);
            println!("rate={rate} channels=1 seconds={seconds} samples={count} n={} old_p50_ms={old50:.3} new_p50_ms={new50:.3} old_p95_ms={:.3} new_p95_ms={:.3} reduction_ms={:.3} reduction_pct={:.1}",
                old_ms.len(), percentile_ms(&old_ms, 0.95), percentile_ms(&new_ms, 0.95),
                old50 - new50, (old50 - new50) / old50 * 100.0);
        }
    }

    #[test]
    fn temp_wav_is_unique_valid_and_restrictive() {
        let directory = TestDirectory::new("private");
        let first = write_temp_wav_in(&directory.0, &sample_audio()).expect("write first WAV");
        let second = write_temp_wav_in(&directory.0, &sample_audio()).expect("write second WAV");

        assert_ne!(first.path, second.path);
        assert_eq!(first.path.parent(), Some(directory.0.as_path()));
        assert!(first
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("murmur-") && name.ends_with(".wav")));
        let reader =
            hound::WavReader::open(first.transcription_path()).expect("open completed WAV");
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.duration(), 3);
        drop(reader);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first.path)
                .expect("read WAV metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        assert!(first
            .try_remove_if_same()
            .expect("remove matching secure WAV"));
        assert!(!first.path.exists());
    }

    #[test]
    fn wav_write_error_removes_the_partial_file() {
        let directory = TestDirectory::new("write-error");
        let path = directory.0.join("murmur-partial.wav");
        let file = create_restrictive_new_file(&path).expect("create partial WAV");
        drop(file);

        let result: Result<()> = remove_partial_on_error(
            &path,
            Err(Error::Backend("injected write failure".to_string())),
        );

        assert!(result.is_err());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn open_descriptor_survives_post_create_replacement_and_cleanup_refuses_substitute() {
        let directory = TestDirectory::new("post-create-replacement");
        let temp = write_temp_wav_in(&directory.0, &sample_audio()).expect("write secure WAV");
        let moved_original = directory.0.join("moved-original.wav");
        std::fs::rename(&temp.path, &moved_original).expect("move original directory entry");
        std::fs::write(&temp.path, b"substitute").expect("place substitute at original path");

        let reader = hound::WavReader::open(temp.transcription_path())
            .expect("descriptor path must still open original WAV");
        assert_eq!(reader.duration(), 3);
        assert!(!temp
            .try_remove_if_same()
            .expect("identity-checked cleanup must not error"));
        assert_eq!(
            std::fs::read(&temp.path).expect("substitute remains"),
            b"substitute"
        );
        assert!(moved_original.exists());
    }

    #[test]
    fn restrictive_create_never_replaces_an_existing_file() {
        let directory = TestDirectory::new("collision");
        let path = directory.0.join("murmur-existing.wav");
        std::fs::write(&path, b"keep me").expect("seed existing file");

        let error = create_restrictive_new_file(&path).expect_err("existing path must fail closed");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&path).expect("read existing file"),
            b"keep me"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restrictive_create_refuses_an_existing_symlink() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new("symlink");
        let target = directory.0.join("target");
        let link = directory.0.join("murmur-link.wav");
        std::fs::write(&target, b"keep me").expect("seed symlink target");
        symlink(&target, &link).expect("create symlink");

        let error = create_restrictive_new_file(&link).expect_err("symlink path must fail closed");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).expect("read target"), b"keep me");
    }

    #[test]
    fn meeting_capture_rejects_dictation_before_audio_thread_start() {
        let arbiter = Arc::new(AudioCaptureArbiter::new());
        let meeting_lease = arbiter
            .reserve(AudioCaptureOwner::Meeting)
            .expect("meeting should reserve idle audio");
        let (tx, rx) = mpsc::channel();
        let audio = AudioHandle {
            tx,
            preferred_device: Arc::new(Mutex::new(None)),
            capture_arbiter: Arc::clone(&arbiter),
            lifecycle: Arc::new(Mutex::new(())),
            owner: Arc::new(Mutex::new(HandleAudioOwner::Idle)),
        };

        assert!(audio.meeting_capture_active());
        assert!(matches!(audio.start(), Err(AudioStartError::CaptureBusy)));
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(arbiter.owner(), Some(AudioCaptureOwner::Meeting));

        drop(meeting_lease);
        assert_eq!(arbiter.owner(), None);
    }

    #[test]
    fn capture_readiness_tracks_first_nonempty_callback() {
        let ready = CaptureReadiness::new();
        assert!(ready.is_active());
        assert!(ready.since_first_callback().is_none());
        ready.note_callback(0, 0.0);
        assert!(ready.since_first_callback().is_none());
        ready.note_callback(128, 0.0);
        assert!(ready.since_first_callback().is_some());
        assert!(ready.first_signal.get().is_none());
        ready.note_callback(128, 0.05);
        assert!(ready.first_signal.get().is_some());
        ready.close();
        assert!(!ready.is_active());
    }

    #[test]
    fn stop_collects_frames_added_during_teardown() {
        let buffer = Arc::new(Mutex::new(vec![1.0]));
        let callback_buffer = Arc::clone(&buffer);
        let samples = take_samples_after_stop(&buffer, || callback_buffer.lock().push(2.0));
        assert_eq!(samples, vec![1.0, 2.0]);
        assert!(buffer.lock().is_empty());
    }

    #[test]
    fn stale_auto_stopped_capture_is_discarded_before_next_start() {
        let mut capture = Some(CaptureState::AutoStopped(sample_audio()));
        assert!(discard_auto_stopped(&mut capture));
        assert!(capture.is_none());
        assert!(!discard_auto_stopped(&mut capture));
    }

    #[test]
    fn mono_passthrough() {
        let audio = RecordedAudio {
            samples: vec![0.1, 0.2, 0.3],
            sample_rate: 16_000,
            channels: 1,
            effective_device_name: None,
        };
        assert_eq!(audio.to_mono(), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn stereo_downmix() {
        let audio = RecordedAudio {
            samples: vec![0.0, 1.0, 0.5, 0.5],
            sample_rate: 48_000,
            channels: 2,
            effective_device_name: None,
        };
        assert_eq!(audio.to_mono(), vec![0.5, 0.5]);
    }

    #[test]
    fn resample_noop_when_rates_match() {
        let s = vec![0.0, 0.5, 1.0];
        assert_eq!(resample_linear(&s, 16_000, 16_000), s);
    }

    #[test]
    fn resample_halves_length_when_rate_halves() {
        let s: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_linear(&s, 32_000, 16_000);
        assert!((out.len() as i32 - 50).abs() <= 1);
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms_f32(&[0.0; 128]), 0.0);
        assert_eq!(rms_i16(&[0; 128]), 0.0);
        // U16 silence is the midpoint, not 0.
        assert_eq!(rms_u16(&[32768; 128]), 0.0);
    }

    #[test]
    fn rms_of_empty_slice_is_zero() {
        assert_eq!(rms_f32(&[]), 0.0);
        assert_eq!(rms_i16(&[]), 0.0);
        assert_eq!(rms_u16(&[]), 0.0);
    }

    #[test]
    fn rms_of_full_scale_f32_is_one() {
        let samples = [1.0_f32; 64];
        // constant 1.0 → RMS is 1.0 exactly.
        assert!((rms_f32(&samples) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rms_normalizes_i16_full_scale() {
        // i16::MAX constant across the block → normalized RMS ≈ 1.0.
        let samples = [i16::MAX; 64];
        assert!((rms_i16(&samples) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn duration_ms_for_one_second_of_mono_16k() {
        let audio = RecordedAudio {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
            channels: 1,
            effective_device_name: None,
        };
        assert_eq!(audio.duration_ms(), 1_000);
    }

    #[test]
    fn signal_classifier_separates_missing_silent_and_meaningful_audio() {
        assert_eq!(classify_capture_signal(&[]), CaptureSignal::NoFrames);
        assert_eq!(
            classify_capture_signal(&[0.0; 256]),
            CaptureSignal::NearDigitalSilence
        );
        assert_eq!(
            classify_capture_signal(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY]),
            CaptureSignal::NearDigitalSilence
        );

        // A single numerical speck in a realistic callback block is not
        // sufficient evidence that the microphone route is live.
        let mut residue = [0.0; 256];
        residue[0] = NO_SIGNAL_RMS_THRESHOLD;
        assert_eq!(
            classify_capture_signal(&residue),
            CaptureSignal::NearDigitalSilence
        );

        // The exact RMS boundary is deliberately classified as silence.
        assert_eq!(
            classify_capture_signal(&[NO_SIGNAL_RMS_THRESHOLD; 32]),
            CaptureSignal::NearDigitalSilence
        );
        // Even extremely quiet finite audio immediately above -100 dBFS must
        // continue to Whisper rather than produce a false no-audio warning.
        assert_eq!(
            classify_capture_signal(&[NO_SIGNAL_RMS_THRESHOLD * 1.01; 32]),
            CaptureSignal::Signal
        );
        assert_eq!(
            classify_capture_signal(&[0.003, -0.004, 0.002, -0.001]),
            CaptureSignal::Signal
        );
    }

    #[test]
    fn effective_device_names_are_sanitized_and_bounded() {
        assert_eq!(
            sanitize_effective_device_name("  Galaxy\nBuds\0  "),
            Some("GalaxyBuds".to_string())
        );
        assert_eq!(sanitize_effective_device_name("\n\t"), None);
        let long_name = "m".repeat(MAX_EFFECTIVE_DEVICE_NAME_CHARS + 20);
        assert_eq!(
            sanitize_effective_device_name(&long_name)
                .expect("sanitized name")
                .chars()
                .count(),
            MAX_EFFECTIVE_DEVICE_NAME_CHARS
        );
    }

    #[test]
    fn recorded_audio_keeps_effective_device_provenance() {
        let audio = RecordedAudio {
            samples: vec![0.1],
            sample_rate: 48_000,
            channels: 1,
            effective_device_name: Some("Galaxy Buds".to_string()),
        };
        let auto_stopped = CaptureState::AutoStopped(audio);
        assert_eq!(
            auto_stopped.finish().effective_device_name.as_deref(),
            Some("Galaxy Buds")
        );
    }
}
