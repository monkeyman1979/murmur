//! Resumable, serialized meeting transcription processing (MT4 / PR #7).
//!
//! This module deliberately keeps Whisper work out of the recorder and the
//! short-dictation pipeline. SQLite remains the source of truth for queue
//! state; this coordinator is only an in-memory execution hint.

use std::collections::HashMap;
#[cfg(any(test, feature = "meeting-capture-debug"))]
use std::ffi::OsStr;
use std::path::Path;
#[cfg(feature = "meeting-capture-debug")]
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
};
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tokio::sync::{Notify, Semaphore};

use crate::captures::{
    CaptureError, CaptureErrorCode, CaptureRecord, CaptureState, CaptureTrack,
    MeetingAudioRetentionState, NewCaptureSegment, ProcessingProfileRecord, ProcessingUnit,
    SuccessfulAudioRetention, TranscriptStore,
};
use crate::corrections::{apply_correction_rules, CorrectionRule};
use crate::model_download::{ActiveSttEngine, SttEngineSlot};
use crate::stt::{SttOptions, TimedTranscript};
use crate::update_install::{UpdateActivityGuard, UpdateInstallGate};

/// The one application-wide permit for memory-heavy model lifecycle work.
pub type HeavyProcessingGate = Arc<Semaphore>;

#[derive(Debug, Clone, Default)]
pub struct MeetingProcessingSettings {
    pub stt_model_id: String,
    pub vocabulary_prompt: Option<String>,
    pub correction_rules: Vec<CorrectionRule>,
    pub model_hydrated: bool,
    pub vocabulary_hydrated: bool,
    pub corrections_hydrated: bool,
}

/// Rust's atomic mirror of frontend-owned meeting settings. Vocabulary is
/// still hydrated for startup compatibility but is not used by meeting STT.
/// New meetings snapshot Auto-detect; persisted older profiles are read separately.
pub struct MeetingProcessingSettingsState {
    value: Mutex<MeetingProcessingSettings>,
}

impl MeetingProcessingSettingsState {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(MeetingProcessingSettings::default()),
        }
    }

    pub fn set_model(&self, model_id: String) {
        let mut value = self.value.lock();
        value.stt_model_id = model_id;
        value.model_hydrated = true;
    }

    pub fn set_vocabulary_prompt(&self, vocabulary_prompt: Option<String>) {
        let mut value = self.value.lock();
        value.vocabulary_prompt = vocabulary_prompt;
        value.vocabulary_hydrated = true;
    }

    pub fn set_corrections(&self, correction_rules: Vec<CorrectionRule>) {
        let mut value = self.value.lock();
        value.correction_rules = correction_rules;
        value.corrections_hydrated = true;
    }

    pub fn snapshot(&self) -> Result<MeetingProcessingProfile, CaptureError> {
        let value = self.value.lock().clone();
        if !(value.model_hydrated && value.vocabulary_hydrated && value.corrections_hydrated) {
            return Err(CaptureError::new(
                CaptureErrorCode::SettingsNotHydrated,
                "meeting settings are not hydrated",
            ));
        }
        if value.stt_model_id.trim().is_empty() {
            return Err(CaptureError::new(
                CaptureErrorCode::ProcessingProfileInvalid,
                "meeting processing profile is invalid",
            ));
        }
        Ok(MeetingProcessingProfile {
            stt_model_id: value.stt_model_id,
            language: None, // Whisper Auto-detect for newly created meetings only.
            // Dictionary vocabulary is for dictation, not meeting audio. Keep
            // the legacy profile field but do not persist new glossary prompts.
            vocabulary_prompt: None,
            correction_rules: value.correction_rules,
        })
    }
}

impl Default for MeetingProcessingSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeetingProcessingProfile {
    pub stt_model_id: String,
    pub language: Option<String>,
    pub vocabulary_prompt: Option<String>,
    pub correction_rules: Vec<CorrectionRule>,
}

impl From<MeetingProcessingProfile> for ProcessingProfileRecord {
    fn from(value: MeetingProcessingProfile) -> Self {
        Self {
            stt_model_id: value.stt_model_id,
            language: value.language,
            vocabulary_prompt: value.vocabulary_prompt,
            correction_rules: value.correction_rules,
        }
    }
}

fn meeting_stt_options(
    profile: &ProcessingProfileRecord,
    track: CaptureTrack,
    cancellation: Arc<AtomicBool>,
) -> SttOptions {
    SttOptions {
        language: profile.language.clone(),
        speech_gate: match track {
            CaptureTrack::Microphone => crate::stt::SpeechGateMode::MeetingMicrophone,
            CaptureTrack::System => crate::stt::SpeechGateMode::Off,
        },
        // A quiet microphone can cause Whisper to recite its initial prompt.
        // Ignore even legacy captures' persisted glossary on retry; normal
        // dictation continues to use its own vocabulary prompt.
        initial_prompt: None,
        cancellation: Some(cancellation),
        ..SttOptions::default()
    }
}

/// Whisper analyses audio in fixed 30-second windows and zero-pads the last one
/// to fill it, so a segment can legitimately end *after* the audio it was given
/// — by up to one window.
///
/// The old bound here was 2 s, and it survived a long time because every full
/// chunk hides the problem: five minutes is exactly ten windows, so the last
/// window ends precisely at the audio's end and nothing can overshoot. Only the
/// final, arbitrary-length chunk of a capture can, and it needs a stretch of
/// trailing quiet for Whisper to run a segment into the padding at all.
///
/// A 49:21 capture found it live on 2026-08-03: eighteen of twenty units
/// transcribed, the nineteenth overshot its 261,160 ms chunk by ~8.8 s, and the
/// whole meeting was thrown away over an artefact of the window size. See the
/// debug log.
const WHISPER_WINDOW_MS: i64 = 30_000;
#[cfg(feature = "meeting-capture-debug")]
const RETAIN_MEETING_AUDIO_ENV: &str = "MURMUR_RETAIN_MEETING_AUDIO";

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn retention_flag_enabled(value: Option<&OsStr>) -> bool {
    value.and_then(OsStr::to_str).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn retain_meeting_audio_enabled() -> bool {
    #[cfg(feature = "meeting-capture-debug")]
    {
        retention_flag_enabled(std::env::var_os(RETAIN_MEETING_AUDIO_ENV).as_deref())
    }
    #[cfg(not(feature = "meeting-capture-debug"))]
    {
        false
    }
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
#[derive(Debug, PartialEq)]
struct WavSignalMetrics {
    filename: String,
    track: &'static str,
    sample_rate: u32,
    channels: u16,
    frame_count: u64,
    duration_ms: u64,
    peak: f64,
    rms: f64,
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn inferred_track(filename: &str) -> &'static str {
    if filename.starts_with("mic-") || filename == "microphone.wav" {
        "microphone"
    } else if filename.starts_with("system-") || filename == "system.wav" {
        "system_audio"
    } else {
        "unknown"
    }
}

/// Read signal-level diagnostics after capture has stopped. This deliberately
/// runs in the processing path rather than a Core Audio callback, so retaining
/// evidence for a developer test cannot add realtime work or alter delivery.
#[cfg(any(test, feature = "meeting-capture-debug"))]
fn wav_signal_metrics(path: &Path) -> Result<WavSignalMetrics, hound::Error> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample == 0 {
        return Err(hound::Error::Unsupported);
    }
    let full_scale = if spec.bits_per_sample >= 32 {
        i32::MAX as f64
    } else {
        ((1_u64 << (spec.bits_per_sample - 1)) - 1) as f64
    };
    let frame_count = u64::from(reader.duration());
    let mut sample_count = 0_u64;
    let mut peak = 0_f64;
    let mut sum_squares = 0_f64;
    for sample in reader.samples::<i32>() {
        let normalized = f64::from(sample?) / full_scale;
        peak = peak.max(normalized.abs());
        sum_squares += normalized * normalized;
        sample_count = sample_count.saturating_add(1);
    }
    let rms = if sample_count == 0 {
        0.0
    } else {
        (sum_squares / sample_count as f64).sqrt()
    };
    let duration_ms = if spec.sample_rate == 0 {
        0
    } else {
        frame_count.saturating_mul(1_000) / u64::from(spec.sample_rate)
    };
    let filename = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("unknown.wav")
        .to_owned();
    Ok(WavSignalMetrics {
        track: inferred_track(&filename),
        filename,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        frame_count,
        duration_ms,
        peak,
        rms,
    })
}

#[cfg(feature = "meeting-capture-debug")]
fn log_retained_audio_diagnostics(staging: Option<&Path>) {
    let Some(staging) = staging else {
        crate::murmur_diag!("[murmur][audio-validation] retained staging has no directory");
        return;
    };
    let Ok(entries) = std::fs::read_dir(staging) else {
        crate::murmur_diag!("[murmur][audio-validation] retained staging could not be inspected");
        return;
    };
    let mut wavs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(OsStr::to_str) == Some("wav")
                && std::fs::symlink_metadata(path)
                    .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        })
        .collect();
    wavs.sort();
    for path in wavs {
        match wav_signal_metrics(&path) {
            Ok(metrics) => crate::murmur_diag!(
                "[murmur][audio-validation] file={} track={} rate_hz={} channels={} frames={} duration_ms={} peak={:.6} rms={:.6}",
                metrics.filename,
                metrics.track,
                metrics.sample_rate,
                metrics.channels,
                metrics.frame_count,
                metrics.duration_ms,
                metrics.peak,
                metrics.rms
            ),
            Err(_) => crate::murmur_diag!(
                "[murmur][audio-validation] file={} metrics_unavailable",
                path.file_name().and_then(OsStr::to_str).unwrap_or("unknown.wav")
            ),
        }
    }
}

/// Atomically transform the timed engine result into database segment rows.
pub fn map_timed_segments(
    unit: &ProcessingUnit,
    transcript: &TimedTranscript,
    correction_rules: &[CorrectionRule],
) -> Result<Vec<NewCaptureSegment>, CaptureError> {
    let chunk_start = unit.start_ms;
    let chunk_end = unit.end_ms;
    let mut mapped = Vec::with_capacity(transcript.segments.len());
    for segment in &transcript.segments {
        if segment.end_ms < segment.start_ms {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "timed segment range is invalid",
            ));
        }
        let start = chunk_start
            .checked_add(i64::from(segment.start_ms))
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::ChunkInvalid, "timestamp overflow")
            })?;
        let end = chunk_start
            .checked_add(i64::from(segment.end_ms))
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::ChunkInvalid, "timestamp overflow")
            })?;
        let tolerated_end = chunk_end.checked_add(WHISPER_WINDOW_MS).ok_or_else(|| {
            CaptureError::new(CaptureErrorCode::ChunkInvalid, "timestamp overflow")
        })?;
        if end < start || end > tolerated_end {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "timed segment endpoint is outside the chunk tolerance",
            ));
        }
        // Anything beginning at or after the chunk's end describes the padding,
        // not the meeting. Dropping it is the honest answer; keeping it would
        // put transcript text over audio that was never recorded.
        if start >= chunk_end {
            continue;
        }
        // A segment that runs off the end is real speech whose closing
        // timestamp landed in the padded tail. Clamp it rather than discarding
        // the words.
        let end = end.min(chunk_end);
        let text_raw = segment.text.trim().to_owned();
        if text_raw.is_empty() {
            continue;
        }
        let text_final = apply_correction_rules(correction_rules, &text_raw);
        mapped.push(NewCaptureSegment {
            track: unit.track,
            // The processor does not assign capture-global ordinals. The
            // store allocates them inside the commit transaction.
            ordinal: 0,
            start_ms: start,
            end_ms: end,
            text_raw,
            text_final,
            language: transcript.language.clone(),
            confidence: segment.confidence.map(f64::from),
        });
    }
    Ok(mapped)
}

#[derive(Debug, Clone, Serialize)]
pub struct MeetingTranscriptionProgress {
    pub capture_id: i64,
    pub status: CaptureState,
    pub completed_units: i64,
    pub total_units: i64,
    pub fraction: f64,
    pub estimated_remaining_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeetingCaptureWarning {
    pub capture_id: i64,
    pub code: CaptureErrorCode,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeetingCaptureCompleted {
    pub capture_id: i64,
}

struct JobControl {
    cancellation: Arc<AtomicBool>,
    stop_reason: AtomicU8, // 0 = running, 1 = suspended, 2 = explicitly cancelled
    finished: AtomicBool,
    cancel_notify: Notify,
    finished_notify: Notify,
}

impl JobControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancellation: Arc::new(AtomicBool::new(false)),
            stop_reason: AtomicU8::new(0),
            finished: AtomicBool::new(false),
            cancel_notify: Notify::new(),
            finished_notify: Notify::new(),
        })
    }

    fn request_suspend(&self) {
        self.stop_reason.fetch_max(1, Ordering::AcqRel);
        self.cancellation.store(true, Ordering::Release);
        self.cancel_notify.notify_one();
    }

    fn stopped_error(&self) -> Option<CaptureError> {
        if !self.cancellation.load(Ordering::Acquire) {
            return None;
        }
        Some(if self.stop_reason.load(Ordering::Acquire) == 1 {
            CaptureError::new(
                CaptureErrorCode::TranscriptionPaused,
                "meeting transcription paused",
            )
        } else {
            CaptureError::new(
                CaptureErrorCode::TranscriptionCancelled,
                "meeting transcription was stopped by the user",
            )
        })
    }

    fn request_cancel(&self) {
        // Explicit cancellation wins over a simultaneous pause request.
        self.stop_reason.store(2, Ordering::Release);
        self.cancellation.store(true, Ordering::Release);
        // `notify_one` retains a permit if the queued job has not begun
        // awaiting yet, so cancellation cannot be lost before the job gate.
        self.cancel_notify.notify_one();
    }

    fn finish(&self) {
        self.finished.store(true, Ordering::Release);
        self.finished_notify.notify_waiters();
    }

    async fn wait_finished(&self) {
        while !self.finished.load(Ordering::Acquire) {
            let notified = self.finished_notify.notified();
            if self.finished.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

/// Serialized meeting work scheduled from durable, ordered capture rows.
pub struct MeetingTranscriptionCoordinator {
    app: tauri::AppHandle,
    store: Arc<TranscriptStore>,
    engine_slot: SttEngineSlot,
    gate: HeavyProcessingGate,
    jobs: Arc<Semaphore>,
    in_flight: Arc<Mutex<HashMap<i64, Arc<JobControl>>>>,
    audio_retention: Arc<MeetingAudioRetentionState>,
    update_install_gate: Arc<UpdateInstallGate>,
    recording_priority: AtomicBool,
}

/// Keeps heavy transcription suspended until native recording teardown has
/// drained the audio writers. A failed start drops this guard and wakes jobs.
pub struct RecordingPriorityGuard {
    coordinator: Arc<MeetingTranscriptionCoordinator>,
}

impl Drop for RecordingPriorityGuard {
    fn drop(&mut self) {
        self.coordinator
            .recording_priority
            .store(false, Ordering::Release);
        self.coordinator.wake_eligible();
    }
}

impl MeetingTranscriptionCoordinator {
    pub fn new(
        app: tauri::AppHandle,
        store: Arc<TranscriptStore>,
        engine_slot: SttEngineSlot,
        gate: HeavyProcessingGate,
        audio_retention: Arc<MeetingAudioRetentionState>,
        update_install_gate: Arc<UpdateInstallGate>,
    ) -> Arc<Self> {
        Arc::new(Self {
            app,
            store,
            engine_slot,
            gate,
            jobs: Arc::new(Semaphore::new(1)),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            audio_retention,
            update_install_gate,
            recording_priority: AtomicBool::new(false),
        })
    }

    pub fn enqueue(self: &Arc<Self>, capture_id: i64) {
        if self.recording_priority.load(Ordering::Acquire)
            || self.store.transcription_paused(capture_id).unwrap_or(true)
        {
            return;
        }
        let Some(update_activity) = self.update_install_gate.begin_activity() else {
            return;
        };
        self.enqueue_with_activity(capture_id, update_activity);
    }

    pub fn enqueue_with_activity(
        self: &Arc<Self>,
        capture_id: i64,
        update_activity: UpdateActivityGuard,
    ) {
        if self.recording_priority.load(Ordering::Acquire)
            || self.store.transcription_paused(capture_id).unwrap_or(true)
        {
            return;
        }
        let control = JobControl::new();
        {
            let mut jobs = self.in_flight.lock();
            if jobs.contains_key(&capture_id) {
                return;
            }
            // Registration happens before spawn so a cancellation command can
            // never observe durable `transcribing` without seeing its worker.
            jobs.insert(capture_id, Arc::clone(&control));
        }
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let _update_activity = update_activity;
            // Never select cancellation against an executing `process` future:
            // dropping it would release the heavyweight gate before native
            // inference/unload has actually stopped. The worker observes the
            // cooperative stop flag and acknowledges only after process exits.
            let result = match this.wait_for_turn(capture_id, &control).await {
                Ok(_job_permit) => this.process(capture_id, Arc::clone(&control)).await,
                Err(error) => Err(error),
            };
            if let Err(error) = &result {
                #[cfg(feature = "meeting-capture-debug")]
                crate::murmur_diag!(
                    "[murmur][transcription-diag] capture_id={capture_id} exited code={} message={}",
                    error.code.as_str(),
                    error.message
                );
                if !matches!(
                    error.code,
                    CaptureErrorCode::TranscriptionCancelled
                        | CaptureErrorCode::TranscriptionPaused
                ) {
                    this.fail(capture_id, error.code);
                }
            }
            {
                let mut jobs = this.in_flight.lock();
                if jobs
                    .get(&capture_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &control))
                {
                    jobs.remove(&capture_id);
                }
            }
            control.finish();
            // A retry can race the old job's terminal DB transition. If its
            // enqueue hint was deduplicated above, restore it after removing
            // the old registry entry so a row cannot be stranded.
            if this
                .store
                .get_capture(capture_id)
                .is_ok_and(|capture| capture.status == CaptureState::Transcribing)
            {
                this.enqueue(capture_id);
            }
        });
    }

    async fn wait_for_turn(
        &self,
        capture_id: i64,
        control: &JobControl,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, CaptureError> {
        loop {
            if control.cancellation.load(Ordering::Acquire) {
                return self.stopped_result(capture_id, control);
            }
            let permit = tokio::select! {
                biased;
                _ = control.cancel_notify.notified() => {
                    return self.stopped_result(capture_id, control);
                }
                permit = self.jobs.clone().acquire_owned() => permit.map_err(|_| {
                    CaptureError::new(CaptureErrorCode::TranscriptionFailed, "meeting transcription coordinator stopped")
                })?,
            };
            // The semaphore bounds concurrency; the database determines order.
            // Tasks spawned for two recordings can be polled in either order.
            let oldest = self.store.transcribing_capture_ids()?.into_iter().next();
            if oldest == Some(capture_id) {
                return Ok(permit);
            }
            drop(permit);
            tokio::select! {
                _ = control.cancel_notify.notified() => {
                    return self.stopped_result(capture_id, control);
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
        }
    }

    fn wake_eligible(self: &Arc<Self>) {
        if let Ok(ids) = self.store.transcribing_capture_ids() {
            for id in ids {
                self.enqueue(id);
            }
        }
    }

    fn emit_hold_change(&self, capture_id: i64) {
        crate::window_events::emit_main(
            &self.app,
            "murmur://meeting-capture-state",
            serde_json::json!({ "capture_id": capture_id, "status": "transcribing" }),
        );
    }

    pub async fn pause(self: &Arc<Self>, capture_id: i64) -> Result<(), CaptureError> {
        self.store.set_transcription_paused(capture_id, true)?;
        let active = { self.in_flight.lock().get(&capture_id).cloned() };
        if let Some(control) = active {
            control.request_suspend();
            control.wait_finished().await;
        }
        self.emit_hold_change(capture_id);
        Ok(())
    }

    pub fn resume(self: &Arc<Self>, capture_id: i64) -> Result<(), CaptureError> {
        self.store.set_transcription_paused(capture_id, false)?;
        self.emit_hold_change(capture_id);
        self.enqueue(capture_id);
        Ok(())
    }

    pub async fn prioritize_recording(
        self: &Arc<Self>,
    ) -> Result<RecordingPriorityGuard, CaptureError> {
        if self.recording_priority.swap(true, Ordering::AcqRel) {
            return Err(CaptureError::new(
                CaptureErrorCode::CaptureBusy,
                "another meeting is starting",
            ));
        }
        let guard = RecordingPriorityGuard {
            coordinator: Arc::clone(self),
        };
        // The native start path holds its lifecycle lock while requesting the
        // reservation. The processing worker never needs that lock.
        let controls: Vec<_> = self.in_flight.lock().values().cloned().collect();
        for control in &controls {
            control.request_suspend();
        }
        let yielded = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            for control in controls {
                control.wait_finished().await;
            }
        })
        .await;
        if yielded.is_err() {
            return Err(CaptureError::new(
                CaptureErrorCode::RecordingPreparationTimedOut,
                "transcription did not yield in time; try starting the recording again",
            ));
        }
        Ok(guard)
    }

    pub async fn cancel(self: &Arc<Self>, capture_id: i64) -> Result<(), CaptureError> {
        let control = self.in_flight.lock().get(&capture_id).cloned();
        if let Some(control) = control {
            #[cfg(feature = "meeting-capture-debug")]
            crate::murmur_diag!(
                "[murmur][transcription-diag] cancellation requested capture_id={capture_id}"
            );
            control.request_cancel();
            control.wait_finished().await;
        } else {
            // No in-memory reader owns the audio, so an orphaned durable job
            // can be made retryable immediately.
            self.store.cancel_transcription_if_active(capture_id)?;
        }
        let capture = self.store.get_capture(capture_id)?;
        if capture.status == CaptureState::TranscriptionFailed
            && capture.error_code.as_deref()
                == Some(CaptureErrorCode::TranscriptionCancelled.as_str())
        {
            #[cfg(feature = "meeting-capture-debug")]
            crate::murmur_diag!(
                "[murmur][transcription-diag] cancellation acknowledged capture_id={capture_id} status={}",
                capture.status.as_str()
            );
            return Ok(());
        }
        let message = if capture.status == CaptureState::Transcribing {
            "meeting transcription did not stop"
        } else {
            "meeting transcription finished before it could be stopped"
        };
        Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            message,
        ))
    }

    fn check_cancelled(&self, control: &JobControl) -> Result<(), CaptureError> {
        control.stopped_error().map_or(Ok(()), Err)
    }

    fn stopped_result<T>(&self, capture_id: i64, control: &JobControl) -> Result<T, CaptureError> {
        let error = self
            .check_cancelled(control)
            .expect_err("stopped job must have a reason");
        if error.code == CaptureErrorCode::TranscriptionCancelled {
            self.persist_cancellation(capture_id)?;
        }
        Err(error)
    }

    fn persist_cancellation(&self, capture_id: i64) -> Result<(), CaptureError> {
        self.store.cancel_transcription_if_active(capture_id)?;
        crate::window_events::emit_meeting_ui(
            &self.app,
            "murmur://meeting-capture-state",
            MeetingTranscriptionProgress {
                capture_id,
                status: CaptureState::TranscriptionFailed,
                completed_units: 0,
                total_units: 0,
                fraction: 0.0,
                estimated_remaining_ms: None,
            },
        );
        Ok(())
    }

    async fn process(&self, capture_id: i64, control: Arc<JobControl>) -> Result<(), CaptureError> {
        if control.cancellation.load(Ordering::Acquire) {
            return self.stopped_result(capture_id, &control);
        }
        if self.recording_priority.load(Ordering::Acquire)
            || self.store.transcription_paused(capture_id)?
        {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptionPaused,
                "meeting transcription paused",
            ));
        }
        let capture = self.store.get_capture(capture_id)?;
        if capture.status != CaptureState::Transcribing {
            return Ok(());
        }
        let profile = match self.store.get_processing_profile(capture_id) {
            Ok(profile) => profile,
            Err(error) => {
                self.fail(capture_id, CaptureErrorCode::ProcessingProfileMissing);
                return Err(error);
            }
        };
        self.store.normalize_processing_units(capture_id)?;
        let units = self.store.list_processing_units(capture_id)?;
        if let Err(error) = validate_processing_files(&capture, &units, &self.app) {
            self.fail(capture_id, error.code);
            return Err(error);
        }
        self.emit_progress(capture_id, None);
        let started = Instant::now();
        let mut completed_in_process = 0_i64;
        for unit in units {
            if let Err(error) = self.check_cancelled(&control) {
                if error.code == CaptureErrorCode::TranscriptionCancelled {
                    self.persist_cancellation(capture_id)?;
                }
                return Err(error);
            }
            if unit.status == crate::captures::CaptureChunkState::Transcribed {
                self.emit_progress(capture_id, Some((started, completed_in_process)));
                continue;
            }
            if let Err(error) = self
                .process_unit(capture_id, &profile, &unit, Arc::clone(&control))
                .await
            {
                if error.code == CaptureErrorCode::TranscriptionCancelled {
                    self.persist_cancellation(capture_id)?;
                } else if error.code != CaptureErrorCode::TranscriptionPaused {
                    let _ = self
                        .store
                        .mark_processing_unit_failed(capture_id, unit.id, error.code);
                    self.fail(capture_id, error.code);
                }
                return Err(error);
            }
            completed_in_process += 1;
            self.emit_progress(capture_id, Some((started, completed_in_process)));
            tokio::task::yield_now().await;
        }
        if let Err(error) = self.check_cancelled(&control) {
            if error.code == CaptureErrorCode::TranscriptionCancelled {
                self.persist_cancellation(capture_id)?;
            }
            return Err(error);
        }
        // Everything past the unit loop used to propagate straight out of this
        // function with `?`, and `enqueue` discards the result — so a failure
        // here left the capture in `transcribing` for ever: no terminal state,
        // no error recorded, and because `transcribing` exposed no row actions,
        // no way for the user to retry or delete it either. Two captures whose
        // audio was silent enough to yield zero segments hit exactly that on
        // 2026-08-03 and became permanently stuck at "2 of 2".
        //
        // Every exit from here on must leave a terminal state behind.
        let finalized = match self.finalize(capture_id) {
            Ok(finalized) => finalized,
            Err(error) => {
                if error.code != CaptureErrorCode::TranscriptionPaused {
                    self.fail(capture_id, error.code);
                }
                return Err(error);
            }
        };
        // Debug audio validation keeps recordings for the longest window.
        let retention = if retain_meeting_audio_enabled() {
            SuccessfulAudioRetention::After7Days
        } else {
            self.audio_retention.snapshot()
        };
        #[cfg(feature = "meeting-capture-debug")]
        if retain_meeting_audio_enabled() {
            log_retained_audio_diagnostics(finalized.staging_path.as_deref());
            crate::murmur_diag!(
                "[murmur][audio-validation] retained staging for capture_id={capture_id}"
            );
        }
        let cleanup =
            crate::captures::capture_audio_bytes(&self.app, &finalized).and_then(|bytes| {
                let retained = self.store.set_completed_audio_retention(
                    capture_id,
                    retention,
                    bytes,
                    crate::history::now_millis(),
                )?;
                if retention == SuccessfulAudioRetention::DeleteImmediately {
                    crate::captures::remove_validated_staging(&self.app, &retained)?;
                    self.store.clear_completed_staging_path(
                        capture_id,
                        retained.staging_path.as_deref(),
                    )?;
                }
                Ok(())
            });
        // Announce completion only once staging cleanup has settled. The
        // library derives "temporary audio remains" from `staging_path`, and a
        // successful cleanup emits nothing of its own, so announcing first
        // refreshed the row at the one instant it was both complete and still
        // holding staging — leaving a cleanup action on a capture with nothing
        // left to clean up until the next full reload.
        crate::window_events::emit_main(
            &self.app,
            "murmur://meeting-capture-completed",
            MeetingCaptureCompleted {
                capture_id: finalized.id,
            },
        );
        crate::tray::refresh_copy_availability(&self.app, &self.store);
        if let Err(error) = cleanup {
            crate::window_events::emit_main(
                &self.app,
                "murmur://meeting-capture-warning",
                MeetingCaptureWarning {
                    capture_id,
                    code: error.code,
                },
            );
        }
        Ok(())
    }

    /// Move a fully-transcribed capture to `finalizing` and write its Markdown.
    ///
    /// Split out so the whole tail of `process` has exactly one error path, and
    /// that path is obliged to record a terminal state.
    fn finalize(&self, capture_id: i64) -> Result<crate::captures::CaptureRecord, CaptureError> {
        let progress = self.store.enter_finalizing_if_complete(capture_id)?;
        self.emit_progress_record(progress, None);
        let (documents, app_data) = crate::captures::resolve_output_roots(&self.app)?;
        crate::captures::finalize_markdown_from_settings(
            &self.store,
            capture_id,
            &documents,
            &app_data,
        )
    }

    async fn process_unit(
        &self,
        capture_id: i64,
        profile: &ProcessingProfileRecord,
        unit: &ProcessingUnit,
        control: Arc<JobControl>,
    ) -> Result<(), CaptureError> {
        let current = self.store.get_capture(capture_id)?;
        if current.status != CaptureState::Transcribing {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptionFailed,
                "capture is no longer transcribing",
            ));
        }
        self.check_cancelled(&control)?;
        let _permit = tokio::select! {
            biased;
            _ = control.cancel_notify.notified() => return self.check_cancelled(&control),
            permit = self.gate.clone().acquire_owned() => permit.map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::TranscriptionFailed,
                    "processing gate stopped",
                )
            })?,
        };
        self.check_cancelled(&control)?;
        let ActiveSttEngine { model_id, engine } = self.engine_slot.read().clone();
        if model_id != profile.stt_model_id {
            return Err(CaptureError::new(
                CaptureErrorCode::SttModelMissing,
                "the persisted meeting model is not active",
            ));
        }
        let options = meeting_stt_options(profile, unit.track, Arc::clone(&control.cancellation));
        let result = engine
            .transcribe_timed(Path::new(&unit.path), &options)
            .await;
        if !matches!(result, Err(crate::error::Error::ModelNotFound(_))) {
            engine.unload().await;
        }
        if let Err(error) = self.check_cancelled(&control) {
            drop(engine);
            drop(_permit);
            return Err(error);
        }
        let timed = result.map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::TranscriptionFailed,
                "timed transcription failed",
            )
        })?;
        let segments = map_timed_segments(unit, &timed, &profile.correction_rules)?;
        self.check_cancelled(&control)?;
        drop(engine);
        drop(_permit);
        match self
            .store
            .commit_processed_unit(capture_id, unit.id, &segments)
        {
            Ok(()) => Ok(()),
            Err(error) => {
                // A local SQLite error can be ambiguous. Never replay segment
                // inserts blindly; a proven transcribed checkpoint is enough
                // to treat the unit as committed.
                let committed = self
                    .store
                    .list_processing_units(capture_id)
                    .ok()
                    .and_then(|units| units.into_iter().find(|candidate| candidate.id == unit.id))
                    .is_some_and(|candidate| {
                        candidate.status == crate::captures::CaptureChunkState::Transcribed
                    });
                if committed {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn fail(&self, capture_id: i64, code: CaptureErrorCode) {
        let _ = self.store.fail_transcription_if_active(capture_id, code);
    }

    fn emit_progress(&self, capture_id: i64, timing: Option<(Instant, i64)>) {
        if let Ok(progress) = self.store.processing_progress(capture_id) {
            self.emit_progress_record(progress, timing);
        }
    }

    fn emit_progress_record(
        &self,
        progress: crate::captures::ProcessingProgress,
        timing: Option<(Instant, i64)>,
    ) {
        let remaining = timing.and_then(|(started, completed_in_process)| {
            if completed_in_process == 0 {
                return None;
            }
            let elapsed = started.elapsed().as_millis() as u64;
            let remaining_units = progress
                .total_units
                .saturating_sub(progress.completed_units) as u64;
            Some(elapsed.saturating_mul(remaining_units) / completed_in_process as u64)
        });
        crate::window_events::emit_main(
            &self.app,
            "murmur://meeting-transcription-progress",
            MeetingTranscriptionProgress {
                capture_id: progress.capture_id,
                status: progress.status,
                completed_units: progress.completed_units,
                total_units: progress.total_units,
                fraction: progress.fraction,
                estimated_remaining_ms: remaining,
            },
        );
    }
}

fn validate_processing_files(
    capture: &CaptureRecord,
    units: &[ProcessingUnit],
    app: &tauri::AppHandle,
) -> Result<(), CaptureError> {
    let staging = capture.staging_path.as_ref().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "capture staging path is missing",
        )
    })?;
    let root = app
        .path()
        .app_data_dir()
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "capture staging root is unavailable",
            )
        })?
        .join("captures")
        .join("staging");
    crate::captures::recovery::validate_processing_manifest(staging, capture.id, &root, units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captures::CaptureTrack;
    use crate::stt::TimedSegment;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_wav(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "murmur-processing-{name}-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn unit() -> ProcessingUnit {
        ProcessingUnit {
            id: 1,
            capture_id: 2,
            ordinal: 0,
            track: CaptureTrack::Microphone,
            start_ms: 10_000,
            end_ms: 20_000,
            path: PathBuf::from("/tmp/mic-00000.wav"),
            byte_length: 1,
            status: crate::captures::CaptureChunkState::Ready,
        }
    }

    #[test]
    fn suspend_aborts_inference_without_becoming_user_cancellation() {
        let control = JobControl::new();
        assert!(control.stopped_error().is_none());
        control.request_suspend();
        assert!(control.cancellation.load(Ordering::Acquire));
        assert_eq!(
            control.stopped_error().unwrap().code,
            CaptureErrorCode::TranscriptionPaused
        );
        control.request_cancel();
        control.request_suspend();
        assert_eq!(
            control.stopped_error().unwrap().code,
            CaptureErrorCode::TranscriptionCancelled
        );
    }

    #[test]
    fn retention_flag_accepts_only_explicit_truthy_values() {
        for value in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(retention_flag_enabled(Some(OsStr::new(value))), "{value}");
        }
        for value in ["", "0", "false", "no", "enabled"] {
            assert!(!retention_flag_enabled(Some(OsStr::new(value))), "{value}");
        }
        assert!(!retention_flag_enabled(None));
    }

    #[test]
    fn retained_wav_metrics_report_signal_without_audio_content() {
        let path = temp_wav("mic-00000");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 1_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for sample in [0_i16, i16::MAX, -i16::MAX, 0] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();

        let metrics = wav_signal_metrics(&path).unwrap();
        assert_eq!(metrics.track, "unknown");
        assert_eq!(metrics.sample_rate, 1_000);
        assert_eq!(metrics.channels, 1);
        assert_eq!(metrics.frame_count, 4);
        assert_eq!(metrics.duration_ms, 4);
        assert!((metrics.peak - 1.0).abs() < f64::EPSILON);
        assert!((metrics.rms - (0.5_f64).sqrt()).abs() < 0.000_001);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn staging_filenames_map_to_track_provenance() {
        assert_eq!(inferred_track("mic-00000.wav"), "microphone");
        assert_eq!(inferred_track("microphone.wav"), "microphone");
        assert_eq!(inferred_track("system-00003.wav"), "system_audio");
        assert_eq!(inferred_track("system.wav"), "system_audio");
        assert_eq!(inferred_track("other.wav"), "unknown");
    }

    #[test]
    fn new_meeting_settings_require_three_hydration_bits_and_snapshot_auto_detect() {
        let settings = MeetingProcessingSettingsState::new();
        assert_eq!(
            settings.snapshot().unwrap_err().code,
            CaptureErrorCode::SettingsNotHydrated
        );
        settings.set_model("model-a".into());
        settings.set_vocabulary_prompt(Some("Glossary: fictional-term.".into()));
        settings.set_corrections(Vec::new());
        let profile = settings.snapshot().unwrap();
        assert_eq!(profile.stt_model_id, "model-a");
        assert!(profile.language.is_none());
        assert!(profile.vocabulary_prompt.is_none());
        assert!(profile.correction_rules.is_empty());
    }

    #[test]
    fn meeting_retry_ignores_persisted_glossary_but_preserves_language_and_cancellation() {
        let profile = ProcessingProfileRecord {
            stt_model_id: "whisper-small-multilingual".into(),
            language: Some("es".into()),
            vocabulary_prompt: Some("Glossary: fictional-term.".into()),
            correction_rules: Vec::new(),
        };
        let cancellation = Arc::new(AtomicBool::new(false));
        let options = meeting_stt_options(
            &profile,
            CaptureTrack::Microphone,
            Arc::clone(&cancellation),
        );
        let system = meeting_stt_options(&profile, CaptureTrack::System, Arc::clone(&cancellation));
        assert_eq!(
            options.speech_gate,
            crate::stt::SpeechGateMode::MeetingMicrophone
        );
        assert_eq!(system.speech_gate, crate::stt::SpeechGateMode::Off);
        assert_eq!(options.language.as_deref(), Some("es"));
        assert_eq!(system.language.as_deref(), Some("es"));
        assert!(options.initial_prompt.is_none());
        assert!(system.initial_prompt.is_none());
        assert!(!options.translate);
        assert!(Arc::ptr_eq(
            options.cancellation.as_ref().unwrap(),
            &cancellation
        ));
    }

    #[test]
    fn mapping_preserves_overlap_and_applies_per_segment_corrections() {
        let transcript = TimedTranscript {
            text: "unused".into(),
            language: Some("en".into()),
            duration_ms: 10_000,
            segments: vec![
                TimedSegment {
                    start_ms: 0,
                    end_ms: 0,
                    text: " foo ".into(),
                    confidence: None,
                },
                TimedSegment {
                    start_ms: 0,
                    end_ms: 2_000,
                    text: "bar".into(),
                    confidence: Some(0.5),
                },
            ],
        };
        let rules = vec![CorrectionRule {
            find: "bar".into(),
            replace: "baz".into(),
            case_sensitive: true,
            whole_word: false,
        }];
        let mapped = map_timed_segments(&unit(), &transcript, &rules).unwrap();
        assert_eq!(mapped[0].start_ms, 10_000);
        assert_eq!(mapped[0].end_ms, 10_000);
        assert_eq!(mapped[1].text_raw, "bar");
        assert_eq!(mapped[1].text_final, "baz");
    }

    /// A capture's final chunk is whatever length the meeting happened to end
    /// on, and Whisper pads its last 30-second window with zeros. Speech that
    /// runs into that padding must keep its words and lose only the overshoot.
    ///
    /// Regression for the live 2026-08-03 failure: a 49:21 meeting whose last
    /// chunk was 261,160 ms had a segment end ~8.8 s past it, and the 2 s
    /// tolerance in force at the time threw the entire capture away.
    #[test]
    fn a_final_chunk_segment_running_into_whisper_padding_is_clamped_not_rejected() {
        let mut unit = unit();
        // The real shape: a partial final chunk, not a round five minutes.
        unit.start_ms = 2_700_000;
        unit.end_ms = 2_961_160;
        let transcript = TimedTranscript {
            text: "unused".into(),
            language: Some("en".into()),
            duration_ms: 261_160,
            segments: vec![TimedSegment {
                // Starts inside the audio, ends 8.84 s into the padded tail —
                // Whisper's last window runs 240 s to 270 s.
                start_ms: 258_000,
                end_ms: 270_000,
                text: "thanks everyone".into(),
                confidence: None,
            }],
        };
        let mapped = map_timed_segments(&unit, &transcript, &[]).unwrap();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].text_raw, "thanks everyone");
        assert_eq!(mapped[0].start_ms, 2_958_000);
        assert_eq!(
            mapped[0].end_ms, 2_961_160,
            "the overshoot is clamped to the chunk, not carried into the next one"
        );
    }

    /// Whisper talking about the zero-padding itself is not transcript. Keeping
    /// it would put words over audio that was never recorded.
    #[test]
    fn a_segment_lying_entirely_in_the_padding_is_dropped() {
        let mut unit = unit();
        unit.start_ms = 2_700_000;
        unit.end_ms = 2_961_160;
        let transcript = TimedTranscript {
            text: "unused".into(),
            language: Some("en".into()),
            duration_ms: 261_160,
            segments: vec![TimedSegment {
                start_ms: 262_000,
                end_ms: 268_000,
                text: "Thank you.".into(),
                confidence: None,
            }],
        };
        assert!(map_timed_segments(&unit, &transcript, &[])
            .unwrap()
            .is_empty());
    }

    /// Widening the bound to one window is not the same as removing it. A
    /// timestamp further out than Whisper's padding can explain is still a
    /// malformed response and still fails the chunk.
    #[test]
    fn a_segment_beyond_one_whisper_window_still_fails_the_chunk() {
        let mut unit = unit();
        unit.start_ms = 2_700_000;
        unit.end_ms = 2_961_160;
        let transcript = TimedTranscript {
            text: "unused".into(),
            language: Some("en".into()),
            duration_ms: 261_160,
            segments: vec![TimedSegment {
                start_ms: 258_000,
                end_ms: 261_160 + 30_001,
                text: "impossible".into(),
                confidence: None,
            }],
        };
        assert_eq!(
            map_timed_segments(&unit, &transcript, &[])
                .unwrap_err()
                .code,
            CaptureErrorCode::ChunkInvalid
        );
    }
}
