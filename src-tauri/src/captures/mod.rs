//! Durable meeting-capture persistence domain (Meeting Transcription MT1).
//!
//! This module intentionally contains no recorder, transcription pipeline, or
//! frontend behavior. SQLite operations are short and synchronous; command
//! wrappers move them off the async runtime. Recovery and Markdown filesystem
//! work happen only after the SQLite connection guard is released.

mod audio_export;
mod markdown;
pub(crate) mod processing;
pub(crate) mod recovery;

pub use markdown::CaptureOutputDirectory;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::history::{now_millis, StoreError, StoreHealth, TranscriptStore};

const MAX_TITLE_CHARS: usize = 200;
const DEFAULT_PAGE_LIMIT: i64 = 100;
const SUMMARY_PAGE_LIMIT: i64 = 25;
const MAX_PAGE_LIMIT: i64 = 500;
const HOUR_MS: i64 = 60 * 60 * 1_000;
static AUDIO_FILES_LOCK: Mutex<()> = Mutex::new(());
static MARKDOWN_RENAME_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Draft,
    SelectingSource,
    Starting,
    Recording,
    Pausing,
    Paused,
    Resuming,
    Stopping,
    Transcribing,
    Finalizing,
    Completed,
    Interrupted,
    CaptureFailed,
    TranscriptionFailed,
    FinalizationFailed,
    Deleting,
    CleanupFailed,
    Cancelled,
}

impl CaptureState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::SelectingSource => "selecting_source",
            Self::Starting => "starting",
            Self::Recording => "recording",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Resuming => "resuming",
            Self::Stopping => "stopping",
            Self::Transcribing => "transcribing",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::CaptureFailed => "capture_failed",
            Self::TranscriptionFailed => "transcription_failed",
            Self::FinalizationFailed => "finalization_failed",
            Self::Deleting => "deleting",
            Self::CleanupFailed => "cleanup_failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, CaptureError> {
        match value {
            "draft" => Ok(Self::Draft),
            "selecting_source" => Ok(Self::SelectingSource),
            "starting" => Ok(Self::Starting),
            "recording" => Ok(Self::Recording),
            "pausing" => Ok(Self::Pausing),
            "paused" => Ok(Self::Paused),
            "resuming" => Ok(Self::Resuming),
            "stopping" => Ok(Self::Stopping),
            "transcribing" => Ok(Self::Transcribing),
            "finalizing" => Ok(Self::Finalizing),
            "completed" => Ok(Self::Completed),
            "interrupted" => Ok(Self::Interrupted),
            "capture_failed" => Ok(Self::CaptureFailed),
            "transcription_failed" => Ok(Self::TranscriptionFailed),
            "finalization_failed" => Ok(Self::FinalizationFailed),
            "deleting" => Ok(Self::Deleting),
            "cleanup_failed" => Ok(Self::CleanupFailed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(CaptureError::new(
                CaptureErrorCode::InvalidPersistedState,
                "capture has an unknown persisted state",
            )),
        }
    }

    fn is_failure(self) -> bool {
        matches!(
            self,
            Self::CaptureFailed
                | Self::TranscriptionFailed
                | Self::FinalizationFailed
                | Self::CleanupFailed
        )
    }

    fn can_delete_metadata(self) -> bool {
        matches!(
            self,
            Self::Draft
                | Self::Completed
                | Self::Interrupted
                | Self::CaptureFailed
                | Self::TranscriptionFailed
                | Self::FinalizationFailed
                | Self::CleanupFailed
                | Self::Cancelled
        )
    }
}

pub fn is_legal_transition(from: CaptureState, to: CaptureState) -> bool {
    use CaptureState::*;
    match from {
        Draft => matches!(to, SelectingSource | Deleting),
        SelectingSource => matches!(to, Starting | Deleting),
        Starting => matches!(to, Recording | Interrupted | CaptureFailed | Deleting),
        Recording => matches!(
            to,
            Pausing | Stopping | Interrupted | CaptureFailed | Deleting
        ),
        Pausing => matches!(to, Paused | Interrupted | CaptureFailed | Deleting),
        Paused => matches!(
            to,
            Resuming | Stopping | Interrupted | CaptureFailed | Deleting
        ),
        Resuming => matches!(to, Recording | Interrupted | CaptureFailed | Deleting),
        Stopping => matches!(to, Transcribing | Interrupted | CaptureFailed | Deleting),
        Transcribing => matches!(to, Finalizing | TranscriptionFailed | Deleting),
        Finalizing => matches!(to, Completed | FinalizationFailed | Deleting),
        Interrupted | CaptureFailed | TranscriptionFailed => matches!(to, Transcribing | Deleting),
        FinalizationFailed => matches!(to, Finalizing | Deleting),
        Deleting => matches!(to, Cancelled | CleanupFailed),
        CleanupFailed => matches!(to, Deleting),
        Completed => matches!(to, Deleting),
        Cancelled => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureErrorCode {
    FeatureDisabled,
    InvalidRequest,
    CaptureNotFound,
    CaptureConflict,
    InvalidStateTransition,
    InvalidPersistedState,
    CleanupRequired,
    RecoveryDiscoveryFailed,
    UnsupportedMacos,
    MicrophonePermissionDenied,
    /// The audio-capture TCC grant ("System Audio Recording Only") is missing
    /// or was denied.
    SystemAudioPermissionDenied,
    SourceUnavailable,
    // Legacy — no longer produced. Meeting capture moved from ScreenCaptureKit
    // to Core Audio process taps, which need neither the screen-recording grant
    // nor Apple's share picker. Kept so captures recorded before that migration
    // still deserialise; do not reuse.
    ScreenPermissionDenied,
    PermissionRequiresRelaunch,
    SourceSelectionCancelled,
    CaptureBusy,
    RecordingPreparationTimedOut,
    SttModelMissing,
    PersistenceUnavailable,
    OutputDirectoryUnwritable,
    OutputFileExists,
    TranscriptUnavailable,
    TranscriptFileMissing,
    InsufficientDiskSpace,
    SystemAudioStartFailed,
    MicrophoneStartFailed,
    WriterQueueOverflow,
    ChunkWriteFailed,
    SourceEnded,
    MicrophoneDisconnected,
    CaptureInterrupted,
    AudioFormatChanged,
    ChunkInvalid,
    TranscriptionFailed,
    NoSpeechRecognized,
    TranscriptionCancelled,
    TranscriptionPaused,
    MarkdownWriteFailed,
    CleanupFailed,
    SettingsNotHydrated,
    ProcessingProfileMissing,
    ProcessingProfileInvalid,
    AudioUnavailable,
    AudioExportFailed,
}

impl CaptureErrorCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FeatureDisabled => "feature_disabled",
            Self::InvalidRequest => "invalid_request",
            Self::CaptureNotFound => "capture_not_found",
            Self::CaptureConflict => "capture_conflict",
            Self::InvalidStateTransition => "invalid_state_transition",
            Self::InvalidPersistedState => "invalid_persisted_state",
            Self::CleanupRequired => "cleanup_required",
            Self::RecoveryDiscoveryFailed => "recovery_discovery_failed",
            Self::UnsupportedMacos => "unsupported_macos",
            Self::MicrophonePermissionDenied => "microphone_permission_denied",
            Self::SystemAudioPermissionDenied => "system_audio_permission_denied",
            Self::SourceUnavailable => "source_unavailable",
            Self::ScreenPermissionDenied => "screen_permission_denied",
            Self::PermissionRequiresRelaunch => "permission_requires_relaunch",
            Self::SourceSelectionCancelled => "source_selection_cancelled",
            Self::CaptureBusy => "capture_busy",
            Self::RecordingPreparationTimedOut => "recording_preparation_timed_out",
            Self::SttModelMissing => "stt_model_missing",
            Self::PersistenceUnavailable => "persistence_unavailable",
            Self::OutputDirectoryUnwritable => "output_directory_unwritable",
            Self::OutputFileExists => "output_file_exists",
            Self::TranscriptUnavailable => "transcript_unavailable",
            Self::TranscriptFileMissing => "transcript_file_missing",
            Self::InsufficientDiskSpace => "insufficient_disk_space",
            Self::SystemAudioStartFailed => "system_audio_start_failed",
            Self::MicrophoneStartFailed => "microphone_start_failed",
            Self::WriterQueueOverflow => "writer_queue_overflow",
            Self::ChunkWriteFailed => "chunk_write_failed",
            Self::SourceEnded => "source_ended",
            Self::MicrophoneDisconnected => "microphone_disconnected",
            Self::CaptureInterrupted => "capture_interrupted",
            Self::AudioFormatChanged => "audio_format_changed",
            Self::ChunkInvalid => "chunk_invalid",
            Self::TranscriptionFailed => "transcription_failed",
            Self::NoSpeechRecognized => "no_speech_recognized",
            Self::TranscriptionCancelled => "transcription_cancelled",
            Self::TranscriptionPaused => "transcription_paused",
            Self::MarkdownWriteFailed => "markdown_write_failed",
            Self::CleanupFailed => "cleanup_failed",
            Self::SettingsNotHydrated => "settings_not_hydrated",
            Self::ProcessingProfileMissing => "processing_profile_missing",
            Self::ProcessingProfileInvalid => "processing_profile_invalid",
            Self::AudioUnavailable => "audio_unavailable",
            Self::AudioExportFailed => "audio_export_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct CaptureError {
    pub code: CaptureErrorCode,
    pub message: String,
}

impl CaptureError {
    pub(crate) fn new(code: CaptureErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn persistence() -> Self {
        Self::new(
            CaptureErrorCode::PersistenceUnavailable,
            "durable meeting-capture persistence is unavailable",
        )
    }
}

impl From<StoreError> for CaptureError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::Sqlite(error) => error.into(),
            StoreError::Storage(_)
            | StoreError::Poisoned
            | StoreError::UnrecognizedSchema
            | StoreError::UnsupportedSchema { .. } => Self::persistence(),
        }
    }
}

impl From<rusqlite::Error> for CaptureError {
    fn from(value: rusqlite::Error) -> Self {
        if matches!(
            value,
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::ConstraintViolation,
                    ..
                },
                _
            )
        ) {
            Self::new(
                CaptureErrorCode::CaptureConflict,
                "capture data conflicts with an existing record",
            )
        } else {
            Self::persistence()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub id: i64,
    pub kind: String,
    pub status: CaptureState,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
    pub source_kind: Option<String>,
    pub source_name: Option<String>,
    pub source_bundle: Option<String>,
    pub language: Option<String>,
    pub word_count: i64,
    pub transcript_text: Option<String>,
    pub markdown_path: Option<PathBuf>,
    pub staging_path: Option<PathBuf>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub completed_chunk_count: i64,
    pub audio_retention: MeetingAudioRetention,
    pub audio_delete_at: Option<i64>,
    pub audio_bytes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingAudioRetention {
    FailurePreserved,
    DeleteImmediately,
    After24Hours,
    After7Days,
}

impl MeetingAudioRetention {
    fn as_str(self) -> &'static str {
        match self {
            Self::FailurePreserved => "failure_preserved",
            Self::DeleteImmediately => "delete_immediately",
            Self::After24Hours => "after_24_hours",
            Self::After7Days => "after_7_days",
        }
    }

    fn parse(value: &str) -> Result<Self, CaptureError> {
        match value {
            "failure_preserved" => Ok(Self::FailurePreserved),
            "delete_immediately" => Ok(Self::DeleteImmediately),
            "after_24_hours" => Ok(Self::After24Hours),
            "after_7_days" => Ok(Self::After7Days),
            // v0.1.1 could keep audio indefinitely. Retention now tops out
            // at 7 days; `retire_kept_audio` rewrites these rows on cleanup.
            LEGACY_KEPT_AUDIO => Ok(Self::After7Days),
            _ => Err(CaptureError::new(
                CaptureErrorCode::InvalidPersistedState,
                "capture has an unknown audio retention state",
            )),
        }
    }

    fn delete_at(self, completed_at: i64) -> Option<i64> {
        match self {
            Self::DeleteImmediately => Some(completed_at),
            Self::After24Hours => completed_at.checked_add(24 * HOUR_MS),
            Self::After7Days => completed_at.checked_add(7 * 24 * HOUR_MS),
            Self::FailurePreserved => None,
        }
    }
}

/// Stored `audio_retention` for audio v0.1.1 kept until manually deleted.
const LEGACY_KEPT_AUDIO: &str = "keep";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessfulAudioRetention {
    DeleteImmediately,
    #[default]
    After24Hours,
    // A v0.1.1 frontend may still persist "keep"; it now means the longest
    // window. To keep a recording longer, users save a copy.
    #[serde(alias = "keep")]
    After7Days,
}

impl SuccessfulAudioRetention {
    fn capture_retention(self) -> MeetingAudioRetention {
        match self {
            Self::DeleteImmediately => MeetingAudioRetention::DeleteImmediately,
            Self::After24Hours => MeetingAudioRetention::After24Hours,
            Self::After7Days => MeetingAudioRetention::After7Days,
        }
    }
}

#[derive(Debug, Default)]
pub struct MeetingAudioRetentionState(Mutex<SuccessfulAudioRetention>);

impl MeetingAudioRetentionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn snapshot(&self) -> SuccessfulAudioRetention {
        self.0.lock().map(|value| *value).unwrap_or_default()
    }

    fn set(&self, value: SuccessfulAudioRetention) -> Result<(), CaptureError> {
        *self.0.lock().map_err(|_| CaptureError::persistence())? = value;
        Ok(())
    }
}

/// Retention window for completed meeting transcripts. Zero keeps completed
/// meetings forever; failed, interrupted, and in-flight captures are never
/// removed automatically because they may still contain recoverable audio.
#[derive(Debug, Default)]
pub struct MeetingTranscriptRetentionState(AtomicU32);

impl MeetingTranscriptRetentionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_days(&self, days: u32) {
        self.0.store(days, Ordering::Relaxed);
    }

    pub fn days(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    fn cutoff_millis(&self) -> Option<i64> {
        let days = self.days();
        if days == 0 {
            return None;
        }
        Some(now_millis().saturating_sub((days as i64) * 24 * HOUR_MS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMarkdownStatus {
    NotApplicable,
    Available,
    Missing,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureProcessingSnapshot {
    pub completed_units: i64,
    pub total_units: i64,
    pub fraction: f64,
    pub estimated_remaining_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureSummary {
    pub id: i64,
    pub status: CaptureState,
    pub title: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
    pub source_name: Option<String>,
    pub language: Option<String>,
    pub word_count: i64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub markdown_status: CaptureMarkdownStatus,
    pub processing: Option<CaptureProcessingSnapshot>,
    pub transcription_paused: bool,
    pub transcription_queued: bool,
    pub has_recovery_audio: bool,
    pub can_retry_transcription: bool,
    pub retry_unavailable_reason: Option<CaptureErrorCode>,
    pub audio_retention: Option<MeetingAudioRetention>,
    pub audio_delete_at: Option<i64>,
    pub audio_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureStats {
    pub total_completed: i64,
    pub total_deletable: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MeetingAiHandoffContext {
    pub title: String,
    pub transcript: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureDetail {
    pub summary: CaptureSummary,
    pub source_kind: Option<String>,
    pub source_bundle: Option<String>,
    pub transcript_text: Option<String>,
    pub markdown_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureSummaryRow {
    id: i64,
    status: CaptureState,
    title: String,
    created_at: i64,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    active_duration_ms: i64,
    paused_duration_ms: i64,
    source_name: Option<String>,
    language: Option<String>,
    word_count: i64,
    error_code: Option<String>,
    error_message: Option<String>,
    markdown_path: Option<PathBuf>,
    has_recovery_audio: bool,
    transcription_paused: bool,
    transcription_queued: bool,
    can_retry_transcription: bool,
    retry_unavailable_reason: Option<CaptureErrorCode>,
    audio_retention: MeetingAudioRetention,
    audio_delete_at: Option<i64>,
    audio_bytes: i64,
    completed_units: i64,
    total_units: i64,
}

#[derive(Debug, Clone, Copy)]
struct ProcessingCounts {
    completed_units: i64,
    total_units: i64,
    audio_bytes: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapturePatch {
    pub title: Option<String>,
    pub transcript_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTrack {
    Microphone,
    System,
}

impl CaptureTrack {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::System => "system",
        }
    }

    fn parse(value: &str) -> Result<Self, CaptureError> {
        match value {
            "microphone" => Ok(Self::Microphone),
            "system" => Ok(Self::System),
            _ => Err(CaptureError::new(
                CaptureErrorCode::InvalidPersistedState,
                "capture segment has an unknown persisted track",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureChunkState {
    Open,
    Ready,
    Transcribing,
    Transcribed,
    Invalid,
    Deleted,
}

impl CaptureChunkState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Ready => "ready",
            Self::Transcribing => "transcribing",
            Self::Transcribed => "transcribed",
            Self::Invalid => "invalid",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCaptureChunk {
    pub capture_id: i64,
    pub ordinal: i64,
    pub track: CaptureTrack,
    pub start_ms: i64,
    pub end_ms: i64,
    pub path: PathBuf,
    pub byte_length: i64,
    pub status: CaptureChunkState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCaptureSegment {
    pub track: CaptureTrack,
    pub ordinal: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text_raw: String,
    pub text_final: String,
    pub language: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProcessingProfileRecord {
    pub stt_model_id: String,
    pub language: Option<String>,
    pub vocabulary_prompt: Option<String>,
    pub correction_rules: Vec<crate::corrections::CorrectionRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProcessingUnit {
    pub id: i64,
    pub capture_id: i64,
    pub ordinal: i64,
    pub track: CaptureTrack,
    pub start_ms: i64,
    pub end_ms: i64,
    pub path: PathBuf,
    pub byte_length: i64,
    pub status: CaptureChunkState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct ProcessingProgress {
    pub capture_id: i64,
    pub status: CaptureState,
    pub completed_units: i64,
    pub total_units: i64,
    pub fraction: f64,
}

impl TranscriptStore {
    fn ensure_capture_durable(&self) -> Result<(), CaptureError> {
        if self.is_durable() {
            Ok(())
        } else {
            Err(CaptureError::persistence())
        }
    }

    pub(crate) fn create_capture_with_processing_profile(
        &self,
        title: &str,
        staging_root: &Path,
        profile: &ProcessingProfileRecord,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        let title = validate_title(title)?;
        if !staging_root.is_absolute() || profile.stt_model_id.trim().is_empty() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "capture creation metadata is invalid",
            ));
        }
        let corrections = serde_json::to_string(&profile.correction_rules).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ProcessingProfileInvalid,
                "meeting processing profile is invalid",
            )
        })?;
        let now = now_millis();
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO captures
                (kind, status, title, created_at, updated_at, language)
             VALUES ('meeting', 'draft', ?1, ?2, ?2, ?3)",
            params![title, now, profile.language],
        )?;
        let id = tx.last_insert_rowid();
        let staging_path = staging_root.join(id.to_string());
        tx.execute(
            "UPDATE captures SET staging_path = ?2 WHERE id = ?1",
            params![id, staging_path.to_string_lossy()],
        )?;
        tx.execute(
            "INSERT INTO capture_processing_profiles
                (capture_id, stt_model_id, language, vocabulary_prompt,
                 corrections_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                profile.stt_model_id,
                profile.language,
                profile.vocabulary_prompt,
                corrections,
                now
            ],
        )?;
        let record = get_capture_in_transaction(&tx, id)?;
        tx.commit()?;
        Ok(record)
    }

    /// Legacy fixture helper. Production capture creation always supplies the
    /// hydrated profile through `create_capture_with_processing_profile`.
    #[cfg(test)]
    pub fn create_capture(
        &self,
        title: &str,
        staging_root: &Path,
    ) -> Result<CaptureRecord, CaptureError> {
        let profile = ProcessingProfileRecord {
            stt_model_id: "test-model".into(),
            language: None,
            vocabulary_prompt: None,
            correction_rules: Vec::new(),
        };
        self.create_capture_with_processing_profile(title, staging_root, &profile)
    }

    pub fn list_captures(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CaptureRecord>, CaptureError> {
        validate_page(limit, offset)?;
        let conn = self.connection()?;
        let mut stmt = conn.prepare(&format!(
            "{CAPTURE_SELECT}
             ORDER BY created_at DESC, id DESC
             LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt
            .query_map(params![limit, offset], capture_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    pub fn get_capture(&self, id: i64) -> Result<CaptureRecord, CaptureError> {
        validate_id(id)?;
        let conn = self.connection()?;
        get_capture_in_connection(&conn, id)
    }

    pub fn capture_stats(&self) -> Result<CaptureStats, CaptureError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT
                 SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN status IN (
                     'draft', 'completed', 'interrupted', 'capture_failed',
                     'transcription_failed', 'finalization_failed',
                     'cleanup_failed', 'cancelled'
                 ) THEN 1 ELSE 0 END)
             FROM captures",
            [],
            |row| {
                Ok(CaptureStats {
                    total_completed: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    total_deletable: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                })
            },
        )
        .map_err(Into::into)
    }

    pub(crate) fn completed_capture_ids_older_than(
        &self,
        cutoff_millis: i64,
    ) -> Result<Vec<i64>, CaptureError> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id FROM captures
             WHERE status = 'completed' AND created_at < ?1
             ORDER BY created_at ASC, id ASC",
        )?;
        let ids = statement
            .query_map(params![cutoff_millis], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub(crate) fn deletable_capture_ids(&self) -> Result<Vec<i64>, CaptureError> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id FROM captures
             WHERE status IN (
                 'draft', 'completed', 'interrupted', 'capture_failed',
                 'transcription_failed', 'finalization_failed',
                 'cleanup_failed', 'cancelled'
             )
             ORDER BY created_at ASC, id ASC",
        )?;
        let ids = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub fn search_captures(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<CaptureRecord>, CaptureError> {
        if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "capture search limit must be between 1 and 500",
            ));
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.list_captures(limit, 0);
        }
        let match_expr = fts_prefix_expr(trimmed);
        let conn = self.connection()?;
        let mut stmt = conn.prepare(&format!(
            "{CAPTURE_SELECT_JOIN_FTS}
             WHERE captures_fts MATCH ?1
               AND COALESCE(TRIM(c.transcript_text), '') <> ''
             ORDER BY c.created_at DESC, c.id DESC
             LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(params![match_expr, limit], capture_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    pub(crate) fn list_capture_summaries(
        &self,
        limit: i64,
        offset: i64,
        expected_staging_root: &Path,
    ) -> Result<Vec<CaptureSummaryRow>, CaptureError> {
        validate_page(limit, offset)?;
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id, status, title, created_at, started_at, ended_at,
                    active_duration_ms, paused_duration_ms, source_name,
                    language, word_count, error_code, error_message,
                    markdown_path,
                    CASE WHEN staging_path IS NOT NULL THEN 1 ELSE 0 END,
                    audio_retention, audio_delete_at, audio_bytes
             FROM captures
             ORDER BY created_at DESC, id DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement
            .query_map(params![limit, offset], summary_row_from_row)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(conn);
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            summaries.push(self.complete_summary_row(row, expected_staging_root)?);
        }
        Ok(summaries)
    }

    pub(crate) fn search_capture_summaries(
        &self,
        query: &str,
        limit: i64,
        expected_staging_root: &Path,
    ) -> Result<Vec<CaptureSummaryRow>, CaptureError> {
        if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "capture search limit must be between 1 and 500",
            ));
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.list_capture_summaries(limit, 0, expected_staging_root);
        }
        let match_expr = fts_prefix_expr(trimmed);
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT c.id, c.status, c.title, c.created_at, c.started_at, c.ended_at,
                    c.active_duration_ms, c.paused_duration_ms, c.source_name,
                    c.language, c.word_count, c.error_code, c.error_message,
                    c.markdown_path,
                    CASE WHEN c.staging_path IS NOT NULL THEN 1 ELSE 0 END,
                    c.audio_retention, c.audio_delete_at, c.audio_bytes
             FROM captures c
             JOIN captures_fts f ON f.rowid = c.id
             WHERE captures_fts MATCH ?1 AND c.status = 'completed'
             ORDER BY c.created_at DESC, c.id DESC
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![match_expr, limit], summary_row_from_row)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(conn);
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            summaries.push(self.complete_summary_row(row, expected_staging_root)?);
        }
        Ok(summaries)
    }

    pub(crate) fn get_capture_summary(
        &self,
        id: i64,
        expected_staging_root: &Path,
    ) -> Result<CaptureSummaryRow, CaptureError> {
        validate_id(id)?;
        let conn = self.connection()?;
        let row = conn
            .query_row(
                "SELECT id, status, title, created_at, started_at, ended_at,
                        active_duration_ms, paused_duration_ms, source_name,
                        language, word_count, error_code, error_message,
                        markdown_path,
                        CASE WHEN staging_path IS NOT NULL THEN 1 ELSE 0 END,
                        audio_retention, audio_delete_at, audio_bytes
                 FROM captures WHERE id = ?1",
                params![id],
                summary_row_from_row,
            )
            .optional()?
            .transpose()?
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found")
            })?;
        drop(conn);
        self.complete_summary_row(row, expected_staging_root)
    }

    fn complete_summary_row(
        &self,
        mut row: CaptureSummaryRow,
        expected_staging_root: &Path,
    ) -> Result<CaptureSummaryRow, CaptureError> {
        let counts = self.processing_counts(row.id)?;
        if row.status == CaptureState::Transcribing {
            row.transcription_paused = self.transcription_paused(row.id)?;
            if !row.transcription_paused {
                row.transcription_queued = self.transcription_queued(row.id, row.created_at)?;
            }
        }
        row.completed_units = counts.completed_units;
        row.total_units = counts.total_units;
        // Older builds saved silent recordings as generic transcription failures.
        // Infer the more precise display code only when every chunk finished
        // and no words were produced; never mutate the user's persisted data.
        if row.status == CaptureState::TranscriptionFailed
            && row.error_code.as_deref() == Some("transcription_failed")
            && counts.total_units > 0
            && counts.completed_units == counts.total_units
        {
            let conn = self.connection()?;
            let has_words: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM capture_segments WHERE capture_id = ?1 AND TRIM(text_final) <> '')",
                params![row.id],
                |result| result.get(0),
            )?;
            if !has_words {
                row.error_code = Some(CaptureErrorCode::NoSpeechRecognized.as_str().into());
            }
        }
        if row.has_recovery_audio {
            row.audio_bytes = row.audio_bytes.max(counts.audio_bytes);
        }
        if matches!(
            row.status,
            CaptureState::Interrupted
                | CaptureState::CaptureFailed
                | CaptureState::TranscriptionFailed
        ) {
            let capture = self.get_capture(row.id)?;
            let units = self.list_processing_units(row.id)?;
            let has_audio = recovery::has_recoverable_audio(
                row.id,
                capture.staging_path.as_deref(),
                expected_staging_root,
                &units,
            );
            row.retry_unavailable_reason = if !has_audio {
                Some(CaptureErrorCode::AudioUnavailable)
            } else {
                self.get_processing_profile(row.id)
                    .err()
                    .map(|error| error.code)
            };
            row.can_retry_transcription = row.retry_unavailable_reason.is_none();
        }
        Ok(row)
    }

    fn processing_counts(&self, capture_id: i64) -> Result<ProcessingCounts, CaptureError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'transcribed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status <> 'deleted' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status <> 'deleted' THEN byte_length ELSE 0 END), 0)
             FROM capture_chunks WHERE capture_id = ?1",
            params![capture_id],
            |row| {
                Ok(ProcessingCounts {
                    completed_units: row.get(0)?,
                    total_units: row.get(1)?,
                    audio_bytes: row.get(2)?,
                })
            },
        )
        .map_err(Into::into)
    }

    pub(crate) fn get_capture_detail(&self, id: i64) -> Result<CaptureRecord, CaptureError> {
        self.get_capture(id)
    }

    pub(crate) fn summary_from_row(row: CaptureSummaryRow) -> CaptureSummary {
        let markdown_status = markdown_status(row.status, row.markdown_path.as_deref());
        let processing = if matches!(
            row.status,
            CaptureState::Transcribing
                | CaptureState::Finalizing
                | CaptureState::Interrupted
                | CaptureState::CaptureFailed
                | CaptureState::TranscriptionFailed
        ) && row.total_units > 0
        {
            Some(CaptureProcessingSnapshot {
                completed_units: row.completed_units,
                total_units: row.total_units,
                fraction: (row.completed_units as f64 / row.total_units as f64).clamp(0.0, 1.0),
                estimated_remaining_ms: None,
            })
        } else {
            None
        };
        CaptureSummary {
            id: row.id,
            status: row.status,
            title: row.title,
            created_at: row.created_at,
            started_at: row.started_at,
            ended_at: row.ended_at,
            active_duration_ms: row.active_duration_ms,
            paused_duration_ms: row.paused_duration_ms,
            source_name: row.source_name,
            language: row.language,
            word_count: row.word_count,
            error_code: row.error_code,
            error_message: row.error_message,
            markdown_status,
            processing,
            transcription_paused: row.transcription_paused,
            transcription_queued: row.transcription_queued,
            has_recovery_audio: row.has_recovery_audio,
            can_retry_transcription: row.can_retry_transcription,
            retry_unavailable_reason: row.retry_unavailable_reason,
            audio_retention: row.has_recovery_audio.then_some(row.audio_retention),
            audio_delete_at: row.audio_delete_at,
            audio_bytes: row.audio_bytes,
        }
    }

    pub fn update_capture(
        &self,
        id: i64,
        patch: CapturePatch,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        if patch.title.is_none() && patch.transcript_text.is_none() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "capture update must change at least one field",
            ));
        }
        let title = patch.title.as_deref().map(validate_title).transpose()?;
        let transcript = patch.transcript_text;
        let word_count = transcript.as_ref().map(|text| count_words(text));
        let now = now_millis();
        let conn = self.connection()?;
        let current = get_capture_in_connection(&conn, id)?;
        if matches!(
            current.status,
            CaptureState::Cancelled | CaptureState::Deleting | CaptureState::Finalizing
        ) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "cancelled, deleting, or finalizing captures cannot be updated",
            ));
        }
        if transcript.is_some()
            && !matches!(
                current.status,
                CaptureState::Transcribing | CaptureState::TranscriptionFailed
            )
        {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "transcript text cannot be updated in the current capture state",
            ));
        }
        conn.execute(
            "UPDATE captures
             SET title = COALESCE(?2, title),
                 transcript_text = COALESCE(?3, transcript_text),
                 word_count = COALESCE(?4, word_count),
                 updated_at = ?5
             WHERE id = ?1",
            params![id, title, transcript, word_count, now],
        )?;
        get_capture_in_connection(&conn, id)
    }

    pub fn transition_capture(
        &self,
        id: i64,
        target: CaptureState,
        error_code: Option<CaptureErrorCode>,
        error_message: Option<String>,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        let requires_error = target.is_failure() || target == CaptureState::Interrupted;
        if requires_error && error_code.is_none() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "failure and interruption transitions require a typed error code",
            ));
        }
        if !requires_error && (error_code.is_some() || error_message.is_some()) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "successful transitions cannot include failure metadata",
            ));
        }
        let conn = self.connection()?;
        let current = get_capture_in_connection(&conn, id)?;
        if !is_legal_transition(current.status, target) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                format!(
                    "capture cannot transition from {} to {}",
                    current.status.as_str(),
                    target.as_str()
                ),
            ));
        }
        let now = now_millis();
        let code = error_code.map(CaptureErrorCode::as_str);
        let changed = conn.execute(
            "UPDATE captures
             SET status = ?2,
                 updated_at = ?3,
                 started_at = CASE
                     WHEN ?2 = 'recording' THEN COALESCE(started_at, ?3)
                     ELSE started_at
                 END,
                 ended_at = CASE
                     WHEN ?2 IN ('stopping', 'cancelled') THEN COALESCE(ended_at, ?3)
                     ELSE ended_at
                 END,
                 error_code = ?4,
                 error_message = ?5
             WHERE id = ?1 AND status = ?6",
            params![
                id,
                target.as_str(),
                now,
                code,
                error_message,
                current.status.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture state changed before the transition could commit",
            ));
        }
        get_capture_in_connection(&conn, id)
    }

    pub fn delete_capture_metadata(&self, id: i64) -> Result<bool, CaptureError> {
        self.ensure_capture_durable()?;
        let current = self.get_capture(id)?;
        if !current.status.can_delete_metadata() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "active capture metadata cannot be deleted",
            ));
        }
        let conn = self.connection()?;
        let changed = conn.execute(
            "DELETE FROM captures WHERE id = ?1 AND status = ?2",
            params![id, current.status.as_str()],
        )?;
        Ok(changed == 1)
    }

    pub fn create_capture_chunk(&self, chunk: &NewCaptureChunk) -> Result<i64, CaptureError> {
        self.ensure_capture_durable()?;
        validate_chunk(chunk)?;
        let conn = self.connection()?;
        let capture = get_capture_in_connection(&conn, chunk.capture_id)?;
        validate_chunk_capture_state(capture.status)?;
        insert_capture_chunk(&conn, chunk)?;
        Ok(conn.last_insert_rowid())
    }

    /// Commit one finalized microphone/system pair and its active-media
    /// checkpoint in a single short SQLite transaction. WAV publication and
    /// manifest I/O must complete before calling this method.
    #[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
    pub(crate) fn commit_ready_chunk_pair(
        &self,
        microphone: &NewCaptureChunk,
        system: &NewCaptureChunk,
        active_duration_ms: i64,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        validate_chunk(microphone)?;
        validate_chunk(system)?;
        if microphone.capture_id != system.capture_id
            || microphone.ordinal != system.ordinal
            || microphone.track != CaptureTrack::Microphone
            || system.track != CaptureTrack::System
            || microphone.status != CaptureChunkState::Ready
            || system.status != CaptureChunkState::Ready
            || active_duration_ms < microphone.end_ms.max(system.end_ms)
        {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "finalized dual-track chunk metadata is inconsistent",
            ));
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let capture = get_capture_in_transaction(&tx, microphone.capture_id)?;
        validate_chunk_capture_state(capture.status)?;
        insert_capture_chunk(&tx, microphone)?;
        insert_capture_chunk(&tx, system)?;
        tx.execute(
            "UPDATE captures
             SET active_duration_ms = MAX(active_duration_ms, ?2),
                 updated_at = ?3
             WHERE id = ?1",
            params![microphone.capture_id, active_duration_ms, now_millis()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Turn the audio an interrupted capture left mid-chunk into indexed,
    /// transcribable chunks.
    ///
    /// A killed process leaves `.partial` WAVs holding real audio behind a
    /// header that advertises none, and the processing validator refuses any
    /// staging directory containing one. Without this step the tail of a
    /// crashed meeting — the part the user has no other copy of — is
    /// unreachable.
    ///
    /// Ordering mirrors the recorder's own publication: repair, then rename,
    /// then manifest, then SQLite. Re-running is safe; a rename that landed
    /// before an interrupted manifest write is picked up as an unindexed WAV
    /// on the next attempt.
    #[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
    pub(crate) fn recover_open_chunks(&self, capture_id: i64) -> Result<usize, CaptureError> {
        use std::collections::{BTreeMap, HashMap};

        self.ensure_capture_durable()?;
        let capture = self.get_capture(capture_id)?;
        if !matches!(
            capture.status,
            CaptureState::Interrupted
                | CaptureState::CaptureFailed
                | CaptureState::TranscriptionFailed
        ) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not eligible for open-chunk recovery",
            ));
        }
        let staging = capture.staging_path.as_ref().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "capture staging path is missing",
            )
        })?;
        let mut manifest = recovery::read_manifest(staging, capture_id)?;
        let indexed = manifest
            .chunks
            .iter()
            .map(|chunk| chunk.ordinal)
            .collect::<std::collections::HashSet<_>>();

        // Each track advances on its own frame count, so the timeline has to
        // continue from that track's own last endpoint, not the manifest's
        // merged one.
        let mut track_end_ms: HashMap<CaptureTrack, i64> = HashMap::new();
        for unit in self.list_processing_units(capture_id)? {
            let entry = track_end_ms.entry(unit.track).or_insert(0_i64);
            *entry = (*entry).max(unit.end_ms);
        }

        let mut candidates: BTreeMap<i64, Vec<recovery::RepairedPartial>> = BTreeMap::new();
        for relative in recovery::discover_open_partials(staging) {
            let path = staging.join(&relative);
            let repaired = recovery::repair_open_partial(&path)?;
            if repaired.frames == 0 {
                // No complete frame ever reached the disk. There is no audio
                // here to preserve, and an empty chunk cannot be transcribed.
                fs::remove_file(&path).map_err(|_| {
                    CaptureError::new(
                        CaptureErrorCode::ChunkWriteFailed,
                        "empty meeting partial could not be removed",
                    )
                })?;
                continue;
            }
            if indexed.contains(&repaired.ordinal) {
                // Nonempty same-ordinal orphans fail the read-only capability
                // check. Do not index the same ordinal twice if this lower-level
                // helper is called directly.
                continue;
            }
            candidates
                .entry(repaired.ordinal)
                .or_default()
                .push(repaired);
        }
        // A rename that survived an interrupted manifest write leaves a
        // finished WAV nothing points at. Adopt it instead of stranding it.
        for relative in recovery::discover_unindexed_wavs(staging, Some(&manifest)) {
            let path = staging.join(&relative);
            let name = relative.to_string_lossy();
            let Some((track, ordinal)) = recovery::parse_partial_name(&format!("{name}.partial"))
            else {
                continue;
            };
            let reader = hound::WavReader::open(&path).map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::ChunkInvalid,
                    "unindexed meeting WAV could not be read",
                )
            })?;
            let frames = u64::from(reader.duration());
            let sample_rate = reader.spec().sample_rate;
            if frames == 0 {
                // A completed-looking header-only orphan can coexist with
                // valid indexed audio. Remove it during explicit recovery so
                // the processing validator does not reject the usable chunks.
                fs::remove_file(&path).map_err(|_| {
                    CaptureError::new(
                        CaptureErrorCode::ChunkWriteFailed,
                        "empty unindexed meeting WAV could not be removed",
                    )
                })?;
                continue;
            }
            if indexed.contains(&ordinal) {
                // Nonempty same-ordinal orphans fail the read-only capability
                // check. Do not index the same ordinal twice if this lower-level
                // helper is called directly.
                continue;
            }
            let byte_length = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            candidates
                .entry(ordinal)
                .or_default()
                .push(recovery::RepairedPartial {
                    track,
                    ordinal,
                    partial_path: path,
                    frames,
                    sample_rate,
                    duration_ms: recovery::frames_to_millis(frames, sample_rate),
                    byte_length: i64::try_from(byte_length).unwrap_or(i64::MAX),
                });
        }
        if candidates.is_empty() {
            return Ok(0);
        }

        let mut recovered = 0_usize;
        for (ordinal, repaired) in candidates {
            let mut published: HashMap<CaptureTrack, (PathBuf, PathBuf, i64, i64, i64)> =
                HashMap::new();
            for chunk in &repaired {
                let start_ms = track_end_ms.get(&chunk.track).copied().unwrap_or(0);
                let end_ms = start_ms.saturating_add(chunk.duration_ms);
                let prefix = match chunk.track {
                    CaptureTrack::Microphone => "mic",
                    CaptureTrack::System => "system",
                };
                let name = PathBuf::from(format!("{prefix}-{ordinal:05}.wav"));
                let final_path = staging.join(&name);
                if chunk.partial_path != final_path {
                    if fs::symlink_metadata(&final_path).is_ok() {
                        return Err(CaptureError::new(
                            CaptureErrorCode::ChunkWriteFailed,
                            "recovered meeting WAV destination already exists",
                        ));
                    }
                    fs::rename(&chunk.partial_path, &final_path).map_err(|_| {
                        CaptureError::new(
                            CaptureErrorCode::ChunkWriteFailed,
                            "recovered meeting WAV could not be published",
                        )
                    })?;
                }
                published.insert(
                    chunk.track,
                    (name, final_path, start_ms, end_ms, chunk.byte_length),
                );
            }
            let entry = |track: CaptureTrack| {
                published
                    .get(&track)
                    .map(|(name, _, start, end, _)| (name.clone(), *start, *end))
            };
            recovery::push_manifest_chunk(
                &mut manifest,
                ordinal,
                entry(CaptureTrack::Microphone),
                entry(CaptureTrack::System),
            );
            recovery::atomic_write_manifest(staging, &manifest).map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::ChunkWriteFailed,
                    "recovered meeting manifest could not be published",
                )
            })?;

            let mut conn = self.connection()?;
            let tx = conn.transaction()?;
            let mut active_duration_ms = 0_i64;
            for (track, (_, path, start_ms, end_ms, byte_length)) in &published {
                let row = NewCaptureChunk {
                    capture_id,
                    ordinal,
                    track: *track,
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                    path: path.clone(),
                    byte_length: *byte_length,
                    status: CaptureChunkState::Ready,
                };
                validate_chunk(&row)?;
                insert_capture_chunk(&tx, &row)?;
                active_duration_ms = active_duration_ms.max(*end_ms);
                track_end_ms.insert(*track, *end_ms);
            }
            tx.execute(
                "UPDATE captures
                 SET active_duration_ms = MAX(active_duration_ms, ?2), updated_at = ?3
                 WHERE id = ?1",
                params![capture_id, active_duration_ms, now_millis()],
            )?;
            tx.commit()?;
            recovered += published.len();
        }
        Ok(recovered)
    }

    /// Insert every timed segment for one chunk and mark that chunk complete
    /// in a single transaction. Any uniqueness or validation error rolls back
    /// both the segment rows and the chunk/capture checkpoint updates.
    pub fn commit_chunk_segments(
        &self,
        capture_id: i64,
        chunk_id: i64,
        segments: &[NewCaptureSegment],
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(capture_id)?;
        validate_id(chunk_id)?;
        for segment in segments {
            validate_segment(segment)?;
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let capture_status: Option<String> = tx
            .query_row(
                "SELECT status FROM captures WHERE id = ?1",
                params![capture_id],
                |row| row.get(0),
            )
            .optional()?;
        match capture_status.as_deref() {
            Some("transcribing") => {}
            Some(_) => {
                return Err(CaptureError::new(
                    CaptureErrorCode::InvalidStateTransition,
                    "capture cannot commit segments in its current state",
                ))
            }
            None => {
                return Err(CaptureError::new(
                    CaptureErrorCode::CaptureNotFound,
                    "capture not found",
                ))
            }
        }
        let chunk: Option<(String, String)> = tx
            .query_row(
                "SELECT status, track FROM capture_chunks
                 WHERE id = ?1 AND capture_id = ?2",
                params![chunk_id, capture_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((chunk_status, chunk_track)) = chunk else {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "capture chunk was not found",
            ));
        };
        if !matches!(chunk_status.as_str(), "ready" | "transcribing") {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture chunk is not ready for segment commit",
            ));
        }
        if segments
            .iter()
            .any(|segment| segment.track.as_str() != chunk_track)
        {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "capture segment track does not match its chunk",
            ));
        }
        for segment in segments {
            tx.execute(
                "INSERT INTO capture_segments
                    (capture_id, chunk_id, track, ordinal, start_ms, end_ms,
                     text_raw, text_final, language, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    capture_id,
                    chunk_id,
                    segment.track.as_str(),
                    segment.ordinal,
                    segment.start_ms,
                    segment.end_ms,
                    segment.text_raw,
                    segment.text_final,
                    segment.language,
                    segment.confidence,
                ],
            )?;
        }
        tx.execute(
            "UPDATE capture_chunks SET status = 'transcribed', error_code = NULL
             WHERE id = ?1",
            params![chunk_id],
        )?;
        tx.execute(
            "UPDATE captures
             SET completed_chunk_count = completed_chunk_count + 1,
                 updated_at = ?2
             WHERE id = ?1",
            params![capture_id, now_millis()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn get_processing_profile(
        &self,
        capture_id: i64,
    ) -> Result<ProcessingProfileRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(capture_id)?;
        let conn = self.connection()?;
        let row: Option<(String, Option<String>, Option<String>, String)> = conn
            .query_row(
                "SELECT stt_model_id, language, vocabulary_prompt, corrections_json
                 FROM capture_processing_profiles WHERE capture_id = ?1",
                params![capture_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((stt_model_id, language, vocabulary_prompt, corrections_json)) = row else {
            return Err(CaptureError::new(
                CaptureErrorCode::ProcessingProfileMissing,
                "capture has no processing profile",
            ));
        };
        let correction_rules = serde_json::from_str(&corrections_json).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ProcessingProfileInvalid,
                "capture processing profile is invalid",
            )
        })?;
        if stt_model_id.trim().is_empty() {
            return Err(CaptureError::new(
                CaptureErrorCode::ProcessingProfileInvalid,
                "capture processing profile is invalid",
            ));
        }
        Ok(ProcessingProfileRecord {
            stt_model_id,
            language,
            vocabulary_prompt,
            correction_rules,
        })
    }

    pub(crate) fn normalize_processing_units(&self, capture_id: i64) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        conn.execute(
            "UPDATE capture_chunks SET status = 'ready', error_code = NULL
             WHERE capture_id = ?1 AND status = 'transcribing'",
            params![capture_id],
        )?;
        Ok(())
    }

    pub(crate) fn list_processing_units(
        &self,
        capture_id: i64,
    ) -> Result<Vec<ProcessingUnit>, CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id, capture_id, ordinal, track, start_ms, end_ms, path,
                    byte_length, status
             FROM capture_chunks WHERE capture_id = ?1
             ORDER BY ordinal ASC,
                      CASE track WHEN 'microphone' THEN 0 ELSE 1 END ASC,
                      id ASC",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| {
                let track: String = row.get(3)?;
                let status: String = row.get(8)?;
                Ok(ProcessingUnit {
                    id: row.get(0)?,
                    capture_id: row.get(1)?,
                    ordinal: row.get(2)?,
                    track: CaptureTrack::parse(&track)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    start_ms: row.get(4)?,
                    end_ms: row.get(5)?,
                    path: PathBuf::from(row.get::<_, String>(6)?),
                    byte_length: row.get(7)?,
                    status: parse_chunk_state(&status)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub(crate) fn processing_progress(
        &self,
        capture_id: i64,
    ) -> Result<ProcessingProgress, CaptureError> {
        let capture = self.get_capture(capture_id)?;
        let conn = self.connection()?;
        let (completed, total): (i64, i64) = conn.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'transcribed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status IN
                    ('ready', 'transcribing', 'transcribed') THEN 1 ELSE 0 END), 0)
             FROM capture_chunks WHERE capture_id = ?1",
            params![capture_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let fraction = if total == 0 {
            1.0
        } else {
            (completed as f64 / total as f64).clamp(0.0, 1.0)
        };
        Ok(ProcessingProgress {
            capture_id,
            status: capture.status,
            completed_units: completed,
            total_units: total,
            fraction,
        })
    }

    pub(crate) fn commit_processed_unit(
        &self,
        capture_id: i64,
        chunk_id: i64,
        segments: &[NewCaptureSegment],
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(capture_id)?;
        validate_id(chunk_id)?;
        for segment in segments {
            validate_segment(segment)?;
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let capture_status: String = tx
            .query_row(
                "SELECT status FROM captures WHERE id = ?1",
                params![capture_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found")
            })?;
        if capture_status != CaptureState::Transcribing.as_str() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not transcribing",
            ));
        }
        let (chunk_status, chunk_track): (String, String) = tx
            .query_row(
                "SELECT status, track FROM capture_chunks WHERE id = ?1 AND capture_id = ?2",
                params![chunk_id, capture_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::ChunkInvalid,
                    "capture chunk was not found",
                )
            })?;
        if !matches!(chunk_status.as_str(), "ready" | "transcribing") {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture chunk is not ready",
            ));
        }
        if segments
            .iter()
            .any(|segment| segment.track.as_str() != chunk_track)
        {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "capture segment track does not match chunk",
            ));
        }
        let mut next_by_track = std::collections::HashMap::<CaptureTrack, i64>::new();
        for segment in segments {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                next_by_track.entry(segment.track)
            {
                let next: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM capture_segments
                     WHERE capture_id = ?1 AND track = ?2",
                    params![capture_id, segment.track.as_str()],
                    |row| row.get(0),
                )?;
                entry.insert(next);
            }
            let next = next_by_track
                .get_mut(&segment.track)
                .expect("segment track was inserted above");
            tx.execute(
                "INSERT INTO capture_segments
                    (capture_id, chunk_id, track, ordinal, start_ms, end_ms,
                     text_raw, text_final, language, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    capture_id,
                    chunk_id,
                    segment.track.as_str(),
                    *next,
                    segment.start_ms,
                    segment.end_ms,
                    segment.text_raw,
                    segment.text_final,
                    segment.language,
                    segment.confidence
                ],
            )?;
            *next = next.saturating_add(1);
        }
        tx.execute(
            "UPDATE capture_chunks SET status = 'transcribed', error_code = NULL
             WHERE id = ?1 AND capture_id = ?2",
            params![chunk_id, capture_id],
        )?;
        tx.execute(
            "UPDATE captures SET completed_chunk_count = (
                 SELECT COUNT(*) FROM capture_chunks
                 WHERE capture_id = ?1 AND status = 'transcribed'
             ), updated_at = ?2 WHERE id = ?1",
            params![capture_id, now_millis()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn mark_processing_unit_failed(
        &self,
        capture_id: i64,
        chunk_id: i64,
        error_code: CaptureErrorCode,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let status: String = tx
            .query_row(
                "SELECT status FROM captures WHERE id = ?1",
                params![capture_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found")
            })?;
        if status != CaptureState::Transcribing.as_str() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not transcribing",
            ));
        }
        let changed = tx.execute(
            "UPDATE capture_chunks SET status = 'ready', error_code = ?3
             WHERE id = ?1 AND capture_id = ?2 AND status IN ('ready', 'transcribing')",
            params![chunk_id, capture_id, error_code.as_str()],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "capture chunk was not ready",
            ));
        }
        tx.execute(
            "UPDATE captures SET status = 'transcription_failed', error_code = ?2,
             error_message = 'meeting transcription failed', updated_at = ?3
             WHERE id = ?1 AND status = 'transcribing'",
            params![capture_id, error_code.as_str(), now_millis()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn cancel_transcription_if_active(
        &self,
        capture_id: i64,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(capture_id)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let status: String = tx
            .query_row(
                "SELECT status FROM captures WHERE id = ?1",
                params![capture_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found")
            })?;
        if status != CaptureState::Transcribing.as_str() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not transcribing",
            ));
        }
        tx.execute(
            "UPDATE capture_chunks SET status = 'ready', error_code = NULL
             WHERE capture_id = ?1 AND status = 'transcribing'",
            params![capture_id],
        )?;
        tx.execute(
            "UPDATE captures SET status = 'transcription_failed', transcription_paused = 0,
             error_code = ?2, error_message = 'meeting transcription was stopped by the user', updated_at = ?3
             WHERE id = ?1 AND status = 'transcribing'",
            params![
                capture_id,
                CaptureErrorCode::TranscriptionCancelled.as_str(),
                now_millis()
            ],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_capture(capture_id)
    }

    pub(crate) fn fail_transcription_if_active(
        &self,
        capture_id: i64,
        error_code: CaptureErrorCode,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        // `finalizing` has to be covered as well as `transcribing`. Processing
        // moves through both, and a failure in the second half used to match
        // nothing here — leaving the capture stuck in a non-terminal state with
        // no error and no way out. Each half gets its own terminal state so the
        // row still offers the right retry.
        conn.execute(
            "UPDATE captures SET
               status = CASE status
                          WHEN 'finalizing' THEN 'finalization_failed'
                          ELSE 'transcription_failed'
                        END,
               transcription_paused = 0,
               error_code = ?2,
               error_message = CASE status
                                 WHEN 'finalizing' THEN 'meeting transcript could not be saved'
                                 ELSE 'meeting transcription failed'
                               END,
               updated_at = ?3
             WHERE id = ?1 AND status IN ('transcribing', 'finalizing')",
            params![capture_id, error_code.as_str(), now_millis()],
        )?;
        Ok(())
    }

    pub(crate) fn enter_finalizing_if_complete(
        &self,
        capture_id: i64,
    ) -> Result<ProcessingProgress, CaptureError> {
        self.ensure_capture_durable()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let (status, paused): (String, bool) = tx
            .query_row(
                "SELECT status, transcription_paused FROM captures WHERE id = ?1",
                params![capture_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found")
            })?;
        if status != CaptureState::Transcribing.as_str() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not transcribing",
            ));
        }
        if paused {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptionPaused,
                "meeting transcription paused",
            ));
        }
        let (total_chunks, incomplete): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status <> 'transcribed' THEN 1 ELSE 0 END), 0)
             FROM capture_chunks WHERE capture_id = ?1",
            params![capture_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let usable: i64 = tx.query_row(
            "SELECT COUNT(*) FROM capture_segments WHERE capture_id = ?1 AND TRIM(text_final) <> ''",
            params![capture_id], |row| row.get(0)
        )?;
        if incomplete != 0 || total_chunks == 0 {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptionFailed,
                "capture is not complete",
            ));
        }
        if usable == 0 {
            // Every audio unit completed successfully, but Whisper produced no
            // words. Preserve the audio for optional retry without presenting
            // this ordinary outcome as an engine failure. Keep the existing
            // terminal status for compatibility with installed SQLite schemas.
            return Err(CaptureError::new(
                CaptureErrorCode::NoSpeechRecognized,
                "no speech was recognized in the recording",
            ));
        }
        tx.execute(
            "UPDATE captures SET status = 'finalizing', transcription_paused = 0,
             completed_chunk_count = (
                 SELECT COUNT(*) FROM capture_chunks WHERE capture_id = ?1 AND status = 'transcribed'
             ), error_code = NULL, error_message = NULL, updated_at = ?2
             WHERE id = ?1 AND status = 'transcribing' AND transcription_paused = 0",
            params![capture_id, now_millis()],
        )?;
        let (completed, total): (i64, i64) = tx.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN status = 'transcribed' THEN 1 ELSE 0 END), 0),
                COUNT(*) FROM capture_chunks WHERE capture_id = ?1",
            params![capture_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.commit()?;
        Ok(ProcessingProgress {
            capture_id,
            status: CaptureState::Finalizing,
            completed_units: completed,
            total_units: total,
            fraction: if total == 0 {
                1.0
            } else {
                (completed as f64 / total as f64).clamp(0.0, 1.0)
            },
        })
    }

    pub(crate) fn retry_transcription(
        &self,
        capture_id: i64,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        let current = self.get_capture(capture_id)?;
        if !matches!(
            current.status,
            CaptureState::Interrupted
                | CaptureState::CaptureFailed
                | CaptureState::TranscriptionFailed
        ) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not eligible for transcription retry",
            ));
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE captures SET status = 'transcribing', transcription_paused = 0,
             error_code = NULL, error_message = NULL, updated_at = ?2
             WHERE id = ?1 AND status IN ('interrupted', 'capture_failed', 'transcription_failed')",
            params![capture_id, now_millis()],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture changed before retry",
            ));
        }
        tx.execute(
            "UPDATE capture_chunks SET error_code = NULL
             WHERE capture_id = ?1 AND status IN ('ready', 'transcribing')",
            params![capture_id],
        )?;
        let record = get_capture_in_transaction(&tx, capture_id)?;
        tx.commit()?;
        Ok(record)
    }

    pub(crate) fn transcription_paused(&self, capture_id: i64) -> Result<bool, CaptureError> {
        validate_id(capture_id)?;
        let conn = self.connection()?;
        conn.query_row(
            "SELECT transcription_paused FROM captures WHERE id = ?1",
            params![capture_id],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .ok_or_else(|| CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found"))
    }

    pub(crate) fn set_transcription_paused(
        &self,
        capture_id: i64,
        paused: bool,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(capture_id)?;
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE captures SET transcription_paused = ?2, updated_at = ?3
             WHERE id = ?1 AND status = 'transcribing'",
            params![capture_id, paused, now_millis()],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not transcribing",
            ));
        }
        Ok(())
    }

    fn transcription_queued(&self, capture_id: i64, created_at: i64) -> Result<bool, CaptureError> {
        let conn = self.connection()?;
        let earlier: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM captures
             WHERE status = 'transcribing' AND transcription_paused = 0
               AND (created_at < ?2 OR (created_at = ?2 AND id < ?1)))",
            params![capture_id, created_at],
            |row| row.get(0),
        )?;
        Ok(earlier)
    }

    pub(crate) fn transcribing_capture_ids(&self) -> Result<Vec<i64>, CaptureError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM captures WHERE status = 'transcribing' AND transcription_paused = 0
             ORDER BY created_at ASC, id ASC",
        )?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        Ok(ids)
    }

    pub(crate) fn set_completed_audio_retention(
        &self,
        capture_id: i64,
        retention: SuccessfulAudioRetention,
        audio_bytes: i64,
        completed_at: i64,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(capture_id)?;
        let retention = retention.capture_retention();
        let delete_at = retention.delete_at(completed_at);
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE captures SET audio_retention = ?2, audio_delete_at = ?3,
                 audio_bytes = ?4, updated_at = ?5
             WHERE id = ?1 AND status = 'completed' AND staging_path IS NOT NULL",
            params![
                capture_id,
                retention.as_str(),
                delete_at,
                audio_bytes.max(0),
                completed_at
            ],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "completed capture has no retained audio",
            ));
        }
        get_capture_in_connection(&conn, capture_id)
    }

    /// Give audio that v0.1.1 kept indefinitely the longest current window,
    /// counted from when recording ended. Failed captures go back to being
    /// preserved until retried, saved, or deleted. A no-op once none remain.
    pub(crate) fn retire_kept_audio(&self) -> Result<usize, CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE captures
                SET audio_retention = CASE WHEN status = 'completed'
                        THEN ?2 ELSE ?3 END,
                    audio_delete_at = CASE WHEN status = 'completed' AND staging_path IS NOT NULL
                        THEN COALESCE(ended_at, updated_at) + ?4 ELSE NULL END
              WHERE audio_retention = ?1",
            params![
                LEGACY_KEPT_AUDIO,
                MeetingAudioRetention::After7Days.as_str(),
                MeetingAudioRetention::FailurePreserved.as_str(),
                7 * 24 * HOUR_MS
            ],
        )?;
        Ok(changed)
    }

    pub(crate) fn expired_completed_audio(&self, now: i64) -> Result<Vec<i64>, CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id FROM captures
             WHERE status = 'completed' AND staging_path IS NOT NULL
               AND audio_delete_at IS NOT NULL AND audio_delete_at <= ?1
             ORDER BY audio_delete_at ASC, id ASC",
        )?;
        let ids = statement
            .query_map(params![now], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub(crate) fn audio_chunk_paths(
        &self,
        capture_id: i64,
    ) -> Result<Vec<(i64, CaptureTrack, PathBuf)>, CaptureError> {
        validate_id(capture_id)?;
        let capture = self.get_capture(capture_id)?;
        if capture.staging_path.is_none() {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "capture has no retained audio",
            ));
        }
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT ordinal, track, path FROM capture_chunks
             WHERE capture_id = ?1 AND status <> 'deleted'
             ORDER BY ordinal ASC, CASE track WHEN 'microphone' THEN 0 ELSE 1 END",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(ordinal, track, path)| {
                Ok((ordinal, CaptureTrack::parse(&track)?, PathBuf::from(path)))
            })
            .collect()
    }

    pub(crate) fn reconcile_completed_staging_paths(
        &self,
        expected_root: &Path,
    ) -> Result<u64, CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id, staging_path FROM captures
             WHERE status = 'completed' AND staging_path IS NOT NULL",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut cleared = 0;
        for (capture_id, path) in rows {
            let expected = expected_root.join(capture_id.to_string());
            if Path::new(&path) == expected
                && matches!(fs::symlink_metadata(&expected), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
            {
                cleared += conn.execute(
                    "UPDATE captures SET staging_path = NULL, audio_delete_at = NULL,
                         audio_bytes = 0, updated_at = ?2
                     WHERE id = ?1 AND status = 'completed' AND staging_path = ?3",
                    params![capture_id, now_millis(), path],
                )?;
            }
        }
        Ok(cleared as u64)
    }

    pub(crate) fn clear_retained_audio_path(
        &self,
        capture_id: i64,
        expected_path: Option<&Path>,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        let expected = expected_path.and_then(Path::to_str).ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::CleanupFailed,
                "capture staging path is invalid",
            )
        })?;
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE captures SET staging_path = NULL, audio_delete_at = NULL,
                 audio_bytes = 0, updated_at = ?3
             WHERE id = ?1 AND staging_path = ?2
               AND status IN ('completed', 'interrupted', 'capture_failed',
                              'transcription_failed', 'finalization_failed', 'cancelled')",
            params![capture_id, expected, now_millis()],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::CleanupFailed,
                "retained audio changed before cleanup could commit",
            ));
        }
        Ok(())
    }

    pub(crate) fn clear_completed_staging_path(
        &self,
        capture_id: i64,
        expected_path: Option<&Path>,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        let expected = expected_path.ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::CleanupFailed,
                "capture staging path is missing",
            )
        })?;
        let expected = expected.to_str().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::CleanupFailed,
                "capture staging path is invalid",
            )
        })?;
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE captures SET staging_path = NULL, audio_delete_at = NULL,
                 audio_bytes = 0, updated_at = ?3
             WHERE id = ?1 AND status = 'completed' AND staging_path = ?2",
            params![capture_id, expected, now_millis()],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::CleanupFailed,
                "completed staging path changed before cleanup could commit",
            ));
        }
        Ok(())
    }

    fn mark_stale_captures_interrupted(&self) -> Result<u64, CaptureError> {
        self.ensure_capture_durable()?;
        let conn = self.connection()?;
        let interrupted = conn.execute(
            "UPDATE captures
             SET status = 'interrupted',
                 updated_at = ?1,
                 error_code = 'capture_interrupted',
                 error_message = 'capture was interrupted before recovery discovery'
             WHERE status IN (
                 'starting', 'recording', 'pausing', 'paused',
                 'resuming', 'stopping'
             )",
            params![now_millis()],
        )?;
        // A process can die after persisting `deleting` but before the
        // validated staging directory was removed. Never leave that row
        // invisible or retry-less on the next launch.
        let cleanup_failed = conn.execute(
            "UPDATE captures
             SET status = 'cleanup_failed',
                 updated_at = ?1,
                 error_code = 'cleanup_failed',
                 error_message = 'capture cleanup was interrupted and can be retried'
             WHERE status = 'deleting'",
            params![now_millis()],
        )?;
        Ok((interrupted + cleanup_failed) as u64)
    }

    #[allow(dead_code)]
    pub(crate) fn checkpoint_timing(
        &self,
        id: i64,
        active_duration_ms: i64,
        paused_duration_ms: i64,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        if active_duration_ms < 0 || paused_duration_ms < 0 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "capture timing cannot be negative",
            ));
        }
        let conn = self.connection()?;
        conn.execute(
            "UPDATE captures
             SET active_duration_ms = MAX(active_duration_ms, ?2),
                 paused_duration_ms = MAX(paused_duration_ms, ?3),
                 updated_at = ?4
             WHERE id = ?1",
            params![id, active_duration_ms, paused_duration_ms, now_millis()],
        )?;
        get_capture_in_connection(&conn, id)
    }

    fn recovery_capture_ids(&self) -> Result<Vec<i64>, CaptureError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM captures
             WHERE status IN (
                 'finalizing', 'interrupted', 'capture_failed',
                 'transcription_failed', 'finalization_failed', 'cleanup_failed'
             )
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        Ok(rows)
    }

    fn existing_capture_ids(&self, ids: &[i64]) -> Result<HashSet<i64>, CaptureError> {
        let conn = self.connection()?;
        let mut existing = HashSet::new();
        let mut stmt = conn.prepare("SELECT 1 FROM captures WHERE id = ?1")?;
        for id in ids {
            if stmt.exists(params![id])? {
                existing.insert(*id);
            }
        }
        Ok(existing)
    }

    /// Prepare one stable finalization snapshot. Rendering inputs are frozen
    /// while `finalizing`; unknown optional timestamps remain omitted rather
    /// than being fabricated. The transaction performs no filesystem work.
    fn prepare_markdown_finalization(
        &self,
        id: i64,
    ) -> Result<markdown::MarkdownSnapshot, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let current = get_capture_in_transaction(&tx, id)?;
        if !matches!(
            current.status,
            CaptureState::Finalizing | CaptureState::FinalizationFailed
        ) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture must be finalizing before Markdown can be generated",
            ));
        }
        let now = now_millis();
        tx.execute(
            "UPDATE captures
             SET status = 'finalizing',
                 updated_at = ?2,
                 error_code = NULL,
                 error_message = NULL
             WHERE id = ?1",
            params![id, now],
        )?;
        let capture = get_capture_in_transaction(&tx, id)?;
        let segments = markdown_segments(&tx, id)?;
        tx.commit()?;
        Ok(markdown::MarkdownSnapshot { capture, segments })
    }

    fn completed_markdown_snapshot(
        &self,
        id: i64,
    ) -> Result<markdown::MarkdownSnapshot, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        let conn = self.connection()?;
        let capture = get_capture_in_connection(&conn, id)?;
        if capture.status != CaptureState::Completed {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "only completed captures can regenerate Markdown",
            ));
        }
        let segments = markdown_segments(&conn, id)?;
        Ok(markdown::MarkdownSnapshot { capture, segments })
    }

    fn complete_markdown_finalization(
        &self,
        id: i64,
        path: &Path,
        rendered: &markdown::RenderedMarkdown,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        if !path.is_absolute() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "completed transcript path must be absolute",
            ));
        }
        markdown::validate_completed_file(path)?;
        let path = path.to_str().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "completed transcript path must be valid UTF-8",
            )
        })?;
        // The filesystem check above completed before this SQLite guard is
        // acquired. The specialized service is the only caller and publishes
        // the final file atomically before reaching this commit.
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE captures
             SET status = 'completed',
                 transcript_text = ?2,
                 word_count = ?3,
                 markdown_path = ?4,
                 updated_at = ?5,
                 error_code = NULL,
                 error_message = NULL
             WHERE id = ?1 AND status = 'finalizing'",
            params![
                id,
                rendered.transcript_text,
                rendered.word_count,
                path,
                now_millis()
            ],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture state changed before Markdown finalization could commit",
            ));
        }
        let capture = get_capture_in_transaction(&tx, id)?;
        tx.commit()?;
        Ok(capture)
    }

    fn complete_markdown_rename(
        &self,
        id: i64,
        expected_title: &str,
        previous_path: &Path,
        title: &str,
        path: &Path,
        require_file: bool,
    ) -> Result<CaptureRecord, CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        let title = validate_title(title)?;
        if require_file {
            markdown::validate_completed_file(path)?;
        } else {
            markdown::validate_final_markdown_path(path)?;
        }
        let previous_path = previous_path.to_str().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "previous transcript path must be valid UTF-8",
            )
        })?;
        let path = path.to_str().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "renamed transcript path must be valid UTF-8",
            )
        })?;
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE captures
             SET title = ?2,
                 markdown_path = ?3,
                 updated_at = ?4
             WHERE id = ?1
               AND status = 'completed'
               AND title = ?5
               AND markdown_path = ?6",
            params![id, title, path, now_millis(), expected_title, previous_path],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture changed before the Markdown rename could commit",
            ));
        }
        let capture = get_capture_in_transaction(&tx, id)?;
        tx.commit()?;
        Ok(capture)
    }

    fn complete_markdown_regeneration(
        &self,
        id: i64,
        path: &Path,
        rendered: &markdown::RenderedMarkdown,
    ) -> Result<(), CaptureError> {
        self.ensure_capture_durable()?;
        validate_id(id)?;
        if !path.is_absolute() {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "regenerated transcript path must be absolute",
            ));
        }
        markdown::validate_completed_file(path)?;
        let path = path.to_str().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "regenerated transcript path must be valid UTF-8",
            )
        })?;
        let conn = self.connection()?;
        let changed = conn.execute(
            "UPDATE captures
             SET transcript_text = ?2,
                 word_count = ?3
             WHERE id = ?1
               AND status = 'completed'
               AND markdown_path = ?4",
            params![id, rendered.transcript_text, rendered.word_count, path],
        )?;
        if changed != 1 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture changed before Markdown regeneration could commit",
            ));
        }
        Ok(())
    }

    fn mark_markdown_finalization_failed(&self, id: i64, cause: &CaptureError) {
        if self.ensure_capture_durable().is_err() || validate_id(id).is_err() {
            return;
        }
        let Ok(conn) = self.connection() else {
            return;
        };
        let _ = conn.execute(
            "UPDATE captures
             SET status = 'finalization_failed',
                 updated_at = ?2,
                 error_code = ?3,
                 error_message = 'the portable Markdown transcript could not be finalized'
             WHERE id = ?1
               AND status IN ('finalizing', 'finalization_failed')",
            params![id, now_millis(), cause.code.as_str()],
        );
    }

    fn completed_ai_handoff_context(
        &self,
        id: i64,
    ) -> Result<MeetingAiHandoffContext, CaptureError> {
        let capture = self.get_capture(id)?;
        if capture.status != CaptureState::Completed {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptUnavailable,
                "capture transcript is not completed",
            ));
        }
        let transcript = capture
            .transcript_text
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::TranscriptUnavailable,
                    "completed capture has no transcript text",
                )
            })?;
        Ok(MeetingAiHandoffContext {
            title: capture.title,
            transcript,
        })
    }

    fn completed_transcript_text(&self, id: i64) -> Result<String, CaptureError> {
        self.completed_ai_handoff_context(id)
            .map(|context| context.transcript)
    }

    /// Newest durable completed meeting context for menu-bar copy actions.
    /// Unfinished captures and legacy completed rows with blank text are skipped.
    pub(crate) fn latest_completed_ai_handoff_context(
        &self,
    ) -> Result<Option<MeetingAiHandoffContext>, CaptureError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT title, transcript_text
             FROM captures
             WHERE status = 'completed'
               AND COALESCE(TRIM(transcript_text), '') <> ''
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            [],
            |row| {
                Ok(MeetingAiHandoffContext {
                    title: row.get(0)?,
                    transcript: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(CaptureError::from)
    }

    fn completed_transcript_path(&self, id: i64) -> Result<PathBuf, CaptureError> {
        let capture = self.get_capture(id)?;
        if capture.status != CaptureState::Completed {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptUnavailable,
                "capture transcript is not completed",
            ));
        }
        capture.markdown_path.ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::TranscriptFileMissing,
                "completed capture has no stored Markdown path",
            )
        })
    }
}

const CAPTURE_COLUMNS: &str = "id, kind, status, title, created_at, updated_at,
    started_at, ended_at, active_duration_ms, paused_duration_ms,
    source_kind, source_name, source_bundle, language, word_count,
    transcript_text, markdown_path, staging_path, error_code, error_message,
    completed_chunk_count, audio_retention, audio_delete_at, audio_bytes";
const CAPTURE_SELECT: &str = "SELECT id, kind, status, title, created_at, updated_at,
    started_at, ended_at, active_duration_ms, paused_duration_ms,
    source_kind, source_name, source_bundle, language, word_count,
    transcript_text, markdown_path, staging_path, error_code, error_message,
    completed_chunk_count, audio_retention, audio_delete_at, audio_bytes FROM captures";
const CAPTURE_SELECT_JOIN_FTS: &str = "SELECT c.id, c.kind, c.status, c.title,
    c.created_at, c.updated_at, c.started_at, c.ended_at,
    c.active_duration_ms, c.paused_duration_ms, c.source_kind, c.source_name,
    c.source_bundle, c.language, c.word_count, c.transcript_text,
    c.markdown_path, c.staging_path, c.error_code, c.error_message,
    c.completed_chunk_count, c.audio_retention, c.audio_delete_at, c.audio_bytes FROM captures c
    JOIN captures_fts f ON f.rowid = c.id";

fn get_capture_in_transaction(
    tx: &Transaction<'_>,
    id: i64,
) -> Result<CaptureRecord, CaptureError> {
    let sql = format!("SELECT {CAPTURE_COLUMNS} FROM captures WHERE id = ?1");
    tx.query_row(&sql, params![id], capture_from_row)
        .optional()?
        .transpose()?
        .ok_or_else(|| CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found"))
}

fn get_capture_in_connection(
    conn: &rusqlite::Connection,
    id: i64,
) -> Result<CaptureRecord, CaptureError> {
    let sql = format!("SELECT {CAPTURE_COLUMNS} FROM captures WHERE id = ?1");
    conn.query_row(&sql, params![id], capture_from_row)
        .optional()?
        .transpose()?
        .ok_or_else(|| CaptureError::new(CaptureErrorCode::CaptureNotFound, "capture not found"))
}

fn markdown_segments(
    conn: &rusqlite::Connection,
    capture_id: i64,
) -> Result<Vec<markdown::MarkdownSegment>, CaptureError> {
    let mut statement = conn.prepare(
        "SELECT id, track, start_ms, end_ms, text_final
         FROM capture_segments
         WHERE capture_id = ?1
         ORDER BY start_ms ASC,
                  end_ms ASC,
                  CASE track WHEN 'microphone' THEN 0 ELSE 1 END ASC,
                  id ASC",
    )?;
    let rows = statement
        .query_map(params![capture_id], |row| {
            let track: String = row.get(1)?;
            Ok((row.get(0)?, track, row.get(2)?, row.get(3)?, row.get(4)?))
        })?
        .collect::<Result<Vec<(i64, String, i64, i64, String)>, _>>()?;
    rows.into_iter()
        .map(|(id, track, start_ms, end_ms, text_final)| {
            Ok(markdown::MarkdownSegment {
                id,
                track: CaptureTrack::parse(&track)?,
                start_ms,
                end_ms,
                text_final,
            })
        })
        .collect()
}

fn summary_row_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<CaptureSummaryRow, CaptureError>> {
    let status: String = row.get(1)?;
    let status = CaptureState::parse(&status);
    let retention: String = row.get(15)?;
    let retention = MeetingAudioRetention::parse(&retention);
    let row = CaptureSummaryRow {
        id: row.get(0)?,
        status: CaptureState::Draft,
        title: row.get(2)?,
        created_at: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        active_duration_ms: row.get(6)?,
        paused_duration_ms: row.get(7)?,
        source_name: row.get(8)?,
        language: row.get(9)?,
        word_count: row.get(10)?,
        error_code: row.get(11)?,
        error_message: row.get(12)?,
        markdown_path: row.get::<_, Option<String>>(13)?.map(PathBuf::from),
        has_recovery_audio: row.get::<_, i64>(14)? != 0,
        transcription_paused: false,
        transcription_queued: false,
        can_retry_transcription: false,
        retry_unavailable_reason: None,
        audio_retention: MeetingAudioRetention::FailurePreserved,
        audio_delete_at: row.get(16)?,
        audio_bytes: row.get(17)?,
        completed_units: 0,
        total_units: 0,
    };
    Ok(status.and_then(|status| {
        retention.map(|audio_retention| CaptureSummaryRow {
            status,
            audio_retention,
            ..row
        })
    }))
}

fn markdown_status(status: CaptureState, path: Option<&Path>) -> CaptureMarkdownStatus {
    if status != CaptureState::Completed {
        return CaptureMarkdownStatus::NotApplicable;
    }
    let Some(path) = path else {
        return CaptureMarkdownStatus::Unavailable;
    };
    if markdown::validate_final_markdown_path(path).is_err() {
        return CaptureMarkdownStatus::Unavailable;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            CaptureMarkdownStatus::Unavailable
        }
        Ok(_) => CaptureMarkdownStatus::Available,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CaptureMarkdownStatus::Missing
        }
        Err(_) => CaptureMarkdownStatus::Unavailable,
    }
}

fn capture_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<CaptureRecord, CaptureError>> {
    let status: String = row.get(2)?;
    let record = CaptureRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        status: CaptureState::Draft,
        title: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
        active_duration_ms: row.get(8)?,
        paused_duration_ms: row.get(9)?,
        source_kind: row.get(10)?,
        source_name: row.get(11)?,
        source_bundle: row.get(12)?,
        language: row.get(13)?,
        word_count: row.get(14)?,
        transcript_text: row.get(15)?,
        markdown_path: row.get::<_, Option<String>>(16)?.map(PathBuf::from),
        staging_path: row.get::<_, Option<String>>(17)?.map(PathBuf::from),
        error_code: row.get(18)?,
        error_message: row.get(19)?,
        completed_chunk_count: row.get(20)?,
        audio_retention: MeetingAudioRetention::FailurePreserved,
        audio_delete_at: row.get(22)?,
        audio_bytes: row.get(23)?,
    };
    let retention: String = row.get(21)?;
    Ok(CaptureState::parse(&status).and_then(|status| {
        MeetingAudioRetention::parse(&retention).map(|audio_retention| CaptureRecord {
            status,
            audio_retention,
            ..record
        })
    }))
}

fn validate_id(id: i64) -> Result<(), CaptureError> {
    if id <= 0 {
        Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "capture id must be positive",
        ))
    } else {
        Ok(())
    }
}

fn validate_title(title: &str) -> Result<String, CaptureError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "capture title must contain between 1 and 200 characters",
        ));
    }
    Ok(title.to_owned())
}

fn validate_page(limit: i64, offset: i64) -> Result<(), CaptureError> {
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) || offset < 0 {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "capture page requires limit 1..=500 and a non-negative offset",
        ));
    }
    Ok(())
}

fn validate_chunk(chunk: &NewCaptureChunk) -> Result<(), CaptureError> {
    validate_id(chunk.capture_id)?;
    if chunk.ordinal < 0
        || chunk.start_ms < 0
        || chunk.end_ms < chunk.start_ms
        || chunk.byte_length < 0
        || chunk.path.as_os_str().is_empty()
    {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "capture chunk metadata is invalid",
        ));
    }
    Ok(())
}

fn parse_chunk_state(value: &str) -> Result<CaptureChunkState, CaptureError> {
    match value {
        "open" => Ok(CaptureChunkState::Open),
        "ready" => Ok(CaptureChunkState::Ready),
        "transcribing" => Ok(CaptureChunkState::Transcribing),
        "transcribed" => Ok(CaptureChunkState::Transcribed),
        "invalid" => Ok(CaptureChunkState::Invalid),
        "deleted" => Ok(CaptureChunkState::Deleted),
        _ => Err(CaptureError::new(
            CaptureErrorCode::InvalidPersistedState,
            "capture chunk has an unknown status",
        )),
    }
}

fn validate_chunk_capture_state(state: CaptureState) -> Result<(), CaptureError> {
    if matches!(
        state,
        CaptureState::Starting
            | CaptureState::Recording
            | CaptureState::Pausing
            | CaptureState::Paused
            | CaptureState::Resuming
            | CaptureState::Stopping
            | CaptureState::Interrupted
            | CaptureState::CaptureFailed
    ) {
        Ok(())
    } else {
        Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            "capture cannot accept chunks in its current state",
        ))
    }
}

fn insert_capture_chunk(
    conn: &rusqlite::Connection,
    chunk: &NewCaptureChunk,
) -> Result<(), CaptureError> {
    conn.execute(
        "INSERT INTO capture_chunks
            (capture_id, ordinal, track, start_ms, end_ms, path,
             byte_length, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            chunk.capture_id,
            chunk.ordinal,
            chunk.track.as_str(),
            chunk.start_ms,
            chunk.end_ms,
            chunk.path.to_string_lossy(),
            chunk.byte_length,
            chunk.status.as_str(),
        ],
    )?;
    Ok(())
}

fn validate_segment(segment: &NewCaptureSegment) -> Result<(), CaptureError> {
    if segment.ordinal < 0
        || segment.start_ms < 0
        || segment.end_ms < segment.start_ms
        || segment.confidence.is_some_and(|value| !value.is_finite())
    {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "capture segment metadata is invalid",
        ));
    }
    Ok(())
}

fn count_words(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

fn fts_prefix_expr(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn feature_gate() -> Result<(), CaptureError> {
    #[cfg(feature = "meeting-capture")]
    {
        Ok(())
    }
    #[cfg(not(feature = "meeting-capture"))]
    {
        Err(CaptureError::new(
            CaptureErrorCode::FeatureDisabled,
            "meeting capture is not included in this build",
        ))
    }
}

fn staging_root(app: &tauri::AppHandle) -> Result<PathBuf, CaptureError> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("captures").join("staging"))
        .map_err(|_| CaptureError::persistence())
}

#[allow(dead_code)]
pub(crate) fn configured_output_directory(app: &tauri::AppHandle) -> Result<PathBuf, CaptureError> {
    let (documents, app_data) = resolve_output_roots(app)?;
    markdown::with_output_lock(|| {
        markdown::get_output_directory(&documents, &app_data).map(|output| output.path)
    })
}

/// Remove only the exact staging directory allocated for this capture. The
/// equality check prevents a database path or symlink from broadening cleanup.
pub(crate) fn remove_validated_staging(
    app: &tauri::AppHandle,
    capture: &CaptureRecord,
) -> Result<(), CaptureError> {
    let expected = staging_root(app)?.join(capture.id.to_string());
    let Some(path) = capture.staging_path.as_ref() else {
        return Ok(());
    };
    if path != &expected || !path.is_absolute() {
        return Err(CaptureError::new(
            CaptureErrorCode::CleanupFailed,
            "capture staging path failed validation",
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::CleanupFailed,
                    "capture staging data could not be removed",
                )
            })?;
            Ok(())
        }
        Ok(_) => Err(CaptureError::new(
            CaptureErrorCode::CleanupFailed,
            "capture staging path is not a trusted directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CaptureError::new(
            CaptureErrorCode::CleanupFailed,
            "capture staging data could not be inspected",
        )),
    }
}

pub(crate) fn capture_audio_bytes(
    app: &tauri::AppHandle,
    capture: &CaptureRecord,
) -> Result<i64, CaptureError> {
    let expected = staging_root(app)?.join(capture.id.to_string());
    let Some(path) = capture.staging_path.as_ref() else {
        return Ok(0);
    };
    if path != &expected || !path.is_absolute() {
        return Err(CaptureError::new(
            CaptureErrorCode::CleanupFailed,
            "capture staging path failed validation",
        ));
    }
    let entries = fs::read_dir(path).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::AudioUnavailable,
            "retained capture audio could not be inspected",
        )
    })?;
    let mut total = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "retained capture audio could not be inspected",
            )
        })?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "retained capture audio could not be inspected",
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let path = entry.path();
        let is_audio = matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("wav" | "partial")
        );
        if is_audio {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(i64::try_from(total).unwrap_or(i64::MAX))
}

pub(crate) fn cleanup_expired_capture_audio(
    app: &tauri::AppHandle,
    store: &TranscriptStore,
    now: i64,
) -> Result<u64, CaptureError> {
    let _guard = AUDIO_FILES_LOCK
        .lock()
        .map_err(|_| CaptureError::persistence())?;
    store.retire_kept_audio()?;
    let ids = store.expired_completed_audio(now)?;
    let mut cleaned = 0_u64;
    for id in ids {
        let capture = store.get_capture(id)?;
        remove_validated_staging(app, &capture)?;
        store.clear_completed_staging_path(id, capture.staging_path.as_deref())?;
        crate::window_events::emit_meeting_ui(
            app,
            "murmur://meeting-capture-state",
            serde_json::json!({ "capture_id": id }),
        );
        cleaned += 1;
    }
    Ok(cleaned)
}

fn ui_summary(
    store: &TranscriptStore,
    capture_id: i64,
    expected_staging_root: &Path,
) -> Result<CaptureSummary, CaptureError> {
    Ok(TranscriptStore::summary_from_row(
        store.get_capture_summary(capture_id, expected_staging_root)?,
    ))
}

fn ui_detail(
    store: &TranscriptStore,
    capture_id: i64,
    expected_staging_root: &Path,
) -> Result<CaptureDetail, CaptureError> {
    let capture = store.get_capture_detail(capture_id)?;
    let summary = TranscriptStore::summary_from_row(
        store.get_capture_summary(capture_id, expected_staging_root)?,
    );
    Ok(CaptureDetail {
        summary,
        source_kind: capture.source_kind,
        source_bundle: capture.source_bundle,
        transcript_text: capture.transcript_text,
        markdown_path: capture.markdown_path,
    })
}

fn user_delete_initiation_allowed(status: CaptureState) -> bool {
    matches!(
        status,
        CaptureState::Draft
            | CaptureState::Completed
            | CaptureState::Interrupted
            | CaptureState::CaptureFailed
            | CaptureState::TranscriptionFailed
            | CaptureState::FinalizationFailed
    )
}

fn delete_capture_with_cleanup_inner(
    app: &tauri::AppHandle,
    store: &TranscriptStore,
    capture_id: i64,
    retry_cleanup: bool,
) -> Result<bool, CaptureError> {
    let current = store.get_capture(capture_id)?;
    if retry_cleanup {
        if current.status != CaptureState::CleanupFailed {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not waiting for cleanup retry",
            ));
        }
    } else if current.status == CaptureState::Cancelled {
        remove_validated_staging(app, &current)?;
        return store.delete_capture_metadata(capture_id);
    } else if !user_delete_initiation_allowed(current.status) {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            "capture cannot be deleted while recording or processing is in flight",
        ));
    }

    store.transition_capture(capture_id, CaptureState::Deleting, None, None)?;
    let deleting = store.get_capture(capture_id)?;
    if let Err(error) = remove_validated_staging(app, &deleting) {
        let _ = store.transition_capture(
            capture_id,
            CaptureState::CleanupFailed,
            Some(CaptureErrorCode::CleanupFailed),
            Some(error.message.clone()),
        );
        return Err(error);
    }
    store.transition_capture(capture_id, CaptureState::Cancelled, None, None)?;
    store.delete_capture_metadata(capture_id)
}

fn delete_capture_ids_with_cleanup(
    app: &tauri::AppHandle,
    store: &TranscriptStore,
    capture_ids: Vec<i64>,
) -> Result<u64, CaptureError> {
    let _guard = AUDIO_FILES_LOCK
        .lock()
        .map_err(|_| CaptureError::persistence())?;
    let mut deleted = 0_u64;
    for capture_id in capture_ids {
        let retry_cleanup = store.get_capture(capture_id)?.status == CaptureState::CleanupFailed;
        if delete_capture_with_cleanup_inner(app, store, capture_id, retry_cleanup)? {
            deleted += 1;
        }
    }
    Ok(deleted)
}

fn purge_meeting_transcripts_per_retention(
    app: &tauri::AppHandle,
    store: &TranscriptStore,
    retention: &MeetingTranscriptRetentionState,
) -> Result<u64, CaptureError> {
    let Some(cutoff) = retention.cutoff_millis() else {
        return Ok(0);
    };
    let ids = store.completed_capture_ids_older_than(cutoff)?;
    delete_capture_ids_with_cleanup(app, store, ids)
}

fn announce_meeting_history_change(app: &tauri::AppHandle, store: &TranscriptStore, deleted: u64) {
    if deleted == 0 {
        return;
    }
    crate::window_events::emit_main(
        app,
        "murmur://meeting-history-changed",
        serde_json::json!({ "deleted": deleted }),
    );
    crate::tray::refresh_copy_availability(app, store);
}

pub(crate) fn cleanup_expired_meeting_transcripts(
    app: &tauri::AppHandle,
    store: &TranscriptStore,
    retention: &MeetingTranscriptRetentionState,
) -> Result<u64, CaptureError> {
    let deleted = purge_meeting_transcripts_per_retention(app, store, retention)?;
    announce_meeting_history_change(app, store, deleted);
    Ok(deleted)
}

type StoreState<'a> = tauri::State<'a, Arc<TranscriptStore>>;
type UpdateGateState<'a> = tauri::State<'a, Arc<crate::update_install::UpdateInstallGate>>;

fn begin_update_activity(
    gate: &UpdateGateState<'_>,
) -> Result<crate::update_install::UpdateActivityGuard, CaptureError> {
    Arc::clone(gate.inner()).begin_activity().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::CaptureBusy,
            "an update installation is starting",
        )
    })
}

#[tauri::command]
pub fn capture_store_health(store: StoreState<'_>) -> StoreHealth {
    store.health()
}

#[tauri::command]
pub async fn capture_create(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    settings: tauri::State<'_, Arc<processing::MeetingProcessingSettingsState>>,
    gate: tauri::State<'_, processing::HeavyProcessingGate>,
    title: String,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    let _permit = Arc::clone(gate.inner())
        .acquire_owned()
        .await
        .map_err(|_| CaptureError::persistence())?;
    let profile = settings.snapshot()?;
    let root = staging_root(&app)?;
    let profile = ProcessingProfileRecord::from(profile);
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store.create_capture_with_processing_profile(&title, &root, &profile)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_retry_transcription(
    store: StoreState<'_>,
    coordinator: tauri::State<'_, Arc<processing::MeetingTranscriptionCoordinator>>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    let store_handle = Arc::clone(store.inner());
    let record = tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        store_handle.retry_transcription(capture_id)
    })
    .await
    .map_err(|_| CaptureError::persistence())??;
    coordinator.enqueue(capture_id);
    Ok(record)
}

#[tauri::command]
pub async fn capture_retry_audio_cleanup(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    let store_handle = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        let capture = store_handle.get_capture(capture_id)?;
        if capture.status != CaptureState::Completed {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not completed",
            ));
        }
        remove_validated_staging(&app, &capture)?;
        store_handle.clear_completed_staging_path(capture_id, capture.staging_path.as_deref())?;
        store_handle.get_capture(capture_id)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_summary_list(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<CaptureSummary>, CaptureError> {
    feature_gate()?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store
            .list_capture_summaries(
                limit.unwrap_or(SUMMARY_PAGE_LIMIT),
                offset.unwrap_or(0),
                &root,
            )
            .map(|rows| {
                rows.into_iter()
                    .map(TranscriptStore::summary_from_row)
                    .collect()
            })
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_stats(store: StoreState<'_>) -> Result<CaptureStats, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || store.capture_stats())
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_summary_search(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<CaptureSummary>, CaptureError> {
    feature_gate()?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store
            .search_capture_summaries(&query, limit.unwrap_or(SUMMARY_PAGE_LIMIT), &root)
            .map(|rows| {
                rows.into_iter()
                    .map(TranscriptStore::summary_from_row)
                    .collect()
            })
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_summary_get(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || ui_summary(&store, capture_id, &root))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_detail_get(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<CaptureDetail, CaptureError> {
    feature_gate()?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || ui_detail(&store, capture_id, &root))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_ui_rename(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
    title: String,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        rename_capture(&store, capture_id, &title)?;
        ui_summary(&store, capture_id, &root)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_ui_retry_transcription(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    coordinator: tauri::State<'_, Arc<processing::MeetingTranscriptionCoordinator>>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let store_handle = Arc::clone(store.inner());
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
        crate::murmur_diag!("[murmur][retry-diag] retry requested for capture_id={capture_id}");
        let capture = store_handle.get_capture(capture_id)?;
        let units = store_handle.list_processing_units(capture_id)?;
        if !recovery::has_recoverable_audio(
            capture_id,
            capture.staging_path.as_deref(),
            &root,
            &units,
        ) {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "capture has no recoverable audio",
            ));
        }
        // Retry must not report success and enqueue work that is guaranteed to
        // fail immediately because an older/corrupt capture lacks its immutable
        // processing settings.
        store_handle.get_processing_profile(capture_id)?;
        // An interrupted capture still holds its final chunk as an open
        // `.partial`. Index it before the retry transitions out of a state
        // that allows chunk inserts, or the tail is silently dropped and the
        // processing validator rejects the whole capture.
        #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
        store_handle
            .recover_open_chunks(capture_id)
            .or_else(|error| {
                match error.code {
                    // Nothing to recover is the ordinary case for a retry that
                    // is not a crash recovery.
                    CaptureErrorCode::InvalidStateTransition => Ok(0),
                    _ => Err(error),
                }
            })
            .inspect_err(|error| {
                crate::murmur_diag!(
                    "[murmur][retry-diag] recover_open_chunks refused: {:?}",
                    error.code
                );
            })?;
        store_handle
            .retry_transcription(capture_id)
            .inspect_err(|_error| {
                #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
                crate::murmur_diag!(
                    "[murmur][retry-diag] retry_transcription refused: {:?}",
                    _error.code
                );
            })?;
        #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
        crate::murmur_diag!("[murmur][retry-diag] capture_id={capture_id} moved to transcribing");
        ui_summary(&store_handle, capture_id, &root)
    })
    .await
    .map_err(|_| CaptureError::persistence())??;
    coordinator.enqueue_with_activity(capture_id, update_activity);
    Ok(summary)
}

#[tauri::command]
pub async fn capture_ui_pause_transcription(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    coordinator: tauri::State<'_, Arc<processing::MeetingTranscriptionCoordinator>>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    coordinator.pause(capture_id).await?;
    ui_summary(store.inner(), capture_id, &staging_root(&app)?)
}

#[tauri::command]
pub async fn capture_ui_resume_transcription(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    coordinator: tauri::State<'_, Arc<processing::MeetingTranscriptionCoordinator>>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    coordinator.resume(capture_id)?;
    ui_summary(store.inner(), capture_id, &staging_root(&app)?)
}

#[tauri::command]
pub async fn capture_ui_cancel_transcription(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    coordinator: tauri::State<'_, Arc<processing::MeetingTranscriptionCoordinator>>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    coordinator.cancel(capture_id).await?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || ui_summary(&store, capture_id, &root))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_ui_retry_audio_cleanup(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let store_handle = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        let capture = store_handle.get_capture(capture_id)?;
        if capture.status != CaptureState::Completed {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture is not completed",
            ));
        }
        remove_validated_staging(&app, &capture)?;
        store_handle.clear_completed_staging_path(capture_id, capture.staging_path.as_deref())?;
        ui_summary(&store_handle, capture_id, &root)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub fn set_meeting_audio_retention(
    state: tauri::State<'_, Arc<MeetingAudioRetentionState>>,
    retention: SuccessfulAudioRetention,
) -> Result<(), CaptureError> {
    state.set(retention)
}

#[tauri::command]
pub async fn set_meeting_transcript_retention(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    state: tauri::State<'_, Arc<MeetingTranscriptRetentionState>>,
    update_gate: UpdateGateState<'_>,
    days: u32,
) -> Result<u64, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    state.set_days(days);
    let store = Arc::clone(store.inner());
    let worker_store = Arc::clone(&store);
    let retention = Arc::clone(state.inner());
    let worker_app = app.clone();
    let deleted = tauri::async_runtime::spawn_blocking(move || {
        purge_meeting_transcripts_per_retention(&worker_app, &worker_store, &retention)
    })
    .await
    .map_err(|_| CaptureError::persistence())??;
    announce_meeting_history_change(&app, &store, deleted);
    Ok(deleted)
}

#[tauri::command]
pub async fn capture_clear_all_meetings(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
) -> Result<u64, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let store = Arc::clone(store.inner());
    let worker_app = app.clone();
    let worker_store = Arc::clone(&store);
    let deleted = tauri::async_runtime::spawn_blocking(move || {
        let ids = worker_store.deletable_capture_ids()?;
        delete_capture_ids_with_cleanup(&worker_app, &worker_store, ids)
    })
    .await
    .map_err(|_| CaptureError::persistence())??;
    announce_meeting_history_change(&app, &store, deleted);
    Ok(deleted)
}

#[tauri::command]
pub async fn capture_delete_audio(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        let capture = store.get_capture(capture_id)?;
        if capture.staging_path.is_none() {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "capture has no retained audio",
            ));
        }
        if !matches!(
            capture.status,
            CaptureState::Completed
                | CaptureState::Interrupted
                | CaptureState::CaptureFailed
                | CaptureState::TranscriptionFailed
                | CaptureState::FinalizationFailed
                | CaptureState::Cancelled
        ) {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "capture audio is in use",
            ));
        }
        remove_validated_staging(&app, &capture)?;
        store.clear_retained_audio_path(capture_id, capture.staging_path.as_deref())?;
        ui_summary(&store, capture_id, &root)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_save_audio_copy(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
    destination: String,
) -> Result<(), CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let destination = PathBuf::from(destination);
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
        {
            let capture = store.get_capture(capture_id)?;
            if matches!(
                capture.status,
                CaptureState::Interrupted | CaptureState::CaptureFailed
            ) {
                store.recover_open_chunks(capture_id)?;
            }
        }
        audio_export::save_audio_copy(&store, capture_id, &root, &destination)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_ui_retry_finalization(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let output_roots = resolve_output_roots(&app);
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || match output_roots {
        Ok((documents, app_data)) => {
            finalize_markdown_from_settings(&store, capture_id, &documents, &app_data)?;
            ui_summary(&store, capture_id, &root)
        }
        Err(error) => {
            store.mark_markdown_finalization_failed(capture_id, &error);
            Err(error)
        }
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_ui_regenerate_markdown(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
    overwrite: bool,
) -> Result<CaptureSummary, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        regenerate_markdown(&store, capture_id, overwrite)?;
        ui_summary(&store, capture_id, &root)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_delete_with_cleanup(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<bool, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        let deleted = delete_capture_with_cleanup_inner(&app, &store, capture_id, false)?;
        crate::tray::refresh_copy_availability(&app, &store);
        Ok(deleted)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_list(
    store: StoreState<'_>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<CaptureRecord>, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store.list_captures(limit.unwrap_or(DEFAULT_PAGE_LIMIT), offset.unwrap_or(0))
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_get(
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || store.get_capture(capture_id))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_search(
    store: StoreState<'_>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<CaptureRecord>, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store.search_captures(&query, limit.unwrap_or(DEFAULT_PAGE_LIMIT))
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_update(
    store: StoreState<'_>,
    capture_id: i64,
    patch: CapturePatch,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    if patch.title.is_some() {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "title changes must use capture_rename so completed Markdown stays synchronized",
        ));
    }
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || store.update_capture(capture_id, patch))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_rename(
    store: StoreState<'_>,
    capture_id: i64,
    title: String,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || rename_capture(&store, capture_id, &title))
        .await
        .map_err(|_| CaptureError::persistence())?
}

fn validate_external_transition_target(target: CaptureState) -> Result<(), CaptureError> {
    if matches!(target, CaptureState::Finalizing | CaptureState::Completed) {
        Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            "finalization states are reserved for the processing pipeline",
        ))
    } else {
        Ok(())
    }
}

#[tauri::command]
pub async fn capture_transition(
    store: StoreState<'_>,
    capture_id: i64,
    target: CaptureState,
    error_code: Option<CaptureErrorCode>,
    error_message: Option<String>,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    validate_external_transition_target(target)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store.transition_capture(capture_id, target, error_code, error_message)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_delete(store: StoreState<'_>, capture_id: i64) -> Result<bool, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        let capture = store.get_capture(capture_id)?;
        if let Some(path) = capture.staging_path.as_ref() {
            match path.try_exists() {
                Ok(false) => {}
                Ok(true) | Err(_) => {
                    return Err(CaptureError::new(
                        CaptureErrorCode::CleanupRequired,
                        "capture staging data must be cleaned up before metadata deletion",
                    ))
                }
            }
        }
        store.delete_capture_metadata(capture_id)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_retry_cleanup(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
    capture_id: i64,
) -> Result<bool, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = AUDIO_FILES_LOCK
            .lock()
            .map_err(|_| CaptureError::persistence())?;
        delete_capture_with_cleanup_inner(&app, &store, capture_id, true)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub interrupted_capture_ids: Vec<i64>,
    pub candidates: Vec<recovery::RecoveryCandidate>,
}

#[tauri::command]
pub async fn capture_recovery_discover(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    update_gate: UpdateGateState<'_>,
) -> Result<RecoveryReport, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let root = staging_root(&app)?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store.mark_stale_captures_interrupted()?;
        let interrupted_capture_ids = store.recovery_capture_ids()?;

        // Directory walking and manifest reads finish before the next SQLite
        // lock acquisition. This keeps filesystem latency outside the mutex.
        let mut candidates = recovery::discover(&root).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::RecoveryDiscoveryFailed,
                "capture staging directories could not be inspected",
            )
        })?;
        let candidate_ids = candidates
            .iter()
            .filter_map(|candidate| candidate.capture_id)
            .collect::<Vec<_>>();
        let existing = store.existing_capture_ids(&candidate_ids)?;
        for candidate in &mut candidates {
            candidate.known_capture = candidate
                .capture_id
                .is_some_and(|capture_id| existing.contains(&capture_id));
        }
        Ok(RecoveryReport {
            interrupted_capture_ids,
            candidates,
        })
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

pub(crate) fn resolve_output_roots(
    app: &tauri::AppHandle,
) -> Result<(PathBuf, PathBuf), CaptureError> {
    let documents = app.path().document_dir().map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::OutputDirectoryUnwritable,
            "the Documents directory could not be resolved",
        )
    })?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|_| CaptureError::persistence())?;
    Ok((documents, app_data))
}

fn write_or_adopt_collision_safe_markdown(
    directory: &Path,
    snapshot: &markdown::MarkdownSnapshot,
    contents: &[u8],
) -> Result<PathBuf, CaptureError> {
    for collision in 1..=10_000 {
        let path = directory.join(markdown::filename_for_capture(
            &snapshot.capture,
            collision,
        )?);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                // A prior process may have crashed after atomically publishing
                // these deterministic bytes but before SQLite completion. An
                // exact regular-file match is safe to adopt; any other object
                // remains untouched and consumes this collision slot.
                if !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && metadata.len() == contents.len() as u64
                    && std::fs::read(&path).is_ok_and(|existing| existing == contents)
                {
                    return Ok(path);
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match markdown::atomic_write(&path, contents, markdown::WriteMode::CreateNew) {
                    Ok(()) => return Ok(path),
                    Err(error) if error.code == CaptureErrorCode::OutputFileExists => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(_) => {
                return Err(CaptureError::new(
                    CaptureErrorCode::MarkdownWriteFailed,
                    "the transcript destination could not be inspected",
                ))
            }
        }
    }
    Err(CaptureError::new(
        CaptureErrorCode::MarkdownWriteFailed,
        "too many transcript filename collisions",
    ))
}

#[cfg(test)]
fn finalize_markdown(
    store: &TranscriptStore,
    capture_id: i64,
    output_directory: &Path,
) -> Result<CaptureRecord, CaptureError> {
    markdown::with_output_lock(|| {
        finalize_markdown_unlocked(store, capture_id, output_directory, |path, rendered| {
            store.complete_markdown_finalization(capture_id, path, rendered)
        })
    })
}

#[cfg(test)]
fn finalize_markdown_with_commit(
    store: &TranscriptStore,
    capture_id: i64,
    output_directory: &Path,
    commit: impl FnOnce(&Path, &markdown::RenderedMarkdown) -> Result<CaptureRecord, CaptureError>,
) -> Result<CaptureRecord, CaptureError> {
    markdown::with_output_lock(|| {
        finalize_markdown_unlocked(store, capture_id, output_directory, commit)
    })
}

fn finalize_markdown_unlocked(
    store: &TranscriptStore,
    capture_id: i64,
    output_directory: &Path,
    commit: impl FnOnce(&Path, &markdown::RenderedMarkdown) -> Result<CaptureRecord, CaptureError>,
) -> Result<CaptureRecord, CaptureError> {
    let snapshot = store.prepare_markdown_finalization(capture_id)?;
    markdown::capture_requires_finalize(&snapshot)?;
    let result = (|| {
        // Directory validation and rendering occur after the snapshot method
        // has released the SQLite connection guard.
        let directory = markdown::validate_output_directory(output_directory)?;
        let rendered = markdown::render(&snapshot)?;
        let path = write_or_adopt_collision_safe_markdown(
            &directory,
            &snapshot,
            rendered.markdown.as_bytes(),
        )?;

        match commit(&path, &rendered) {
            Ok(capture) => Ok(capture),
            Err(error) => {
                // A local SQLite commit error can be ambiguous. If a fresh
                // read proves that the exact path reached `completed`, keep
                // the file and treat finalization as successful.
                if let Ok(capture) = store.get_capture(capture_id) {
                    if capture.status == CaptureState::Completed
                        && capture.markdown_path.as_deref() == Some(path.as_path())
                    {
                        return Ok(capture);
                    }
                }
                // Keep the atomically published, complete file. A retry scans
                // deterministic collision candidates and adopts an exact byte
                // match, so crashes and database failures do not create a
                // duplicate or risk deleting a user/cloud replacement.
                Err(CaptureError::new(
                    error.code,
                    "the transcript file was written but database finalization failed; a complete untracked file remains for safe reconciliation on retry",
                ))
            }
        }
    })();
    if let Err(error) = &result {
        store.mark_markdown_finalization_failed(capture_id, error);
    }
    result
}

pub(crate) fn finalize_markdown_from_settings(
    store: &TranscriptStore,
    capture_id: i64,
    documents: &Path,
    app_data: &Path,
) -> Result<CaptureRecord, CaptureError> {
    markdown::with_output_lock(|| {
        let output = match markdown::get_output_directory(documents, app_data) {
            Ok(output) => output,
            Err(error) => {
                store.mark_markdown_finalization_failed(capture_id, &error);
                return Err(error);
            }
        };
        finalize_markdown_unlocked(store, capture_id, &output.path, |path, rendered| {
            store.complete_markdown_finalization(capture_id, path, rendered)
        })
    })
}

fn stage_completed_markdown(path: &Path) -> Result<PathBuf, CaptureError> {
    let parent = path.parent().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the stored transcript path has no parent directory",
        )
    })?;
    for _ in 0..10_000 {
        let counter = MARKDOWN_RENAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let staged = parent.join(format!(
            ".murmur-title-rename-{}-{counter}.md.tmp",
            std::process::id()
        ));
        match markdown::rename_no_replace(path, &staged) {
            Ok(()) => return Ok(staged),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(CaptureError::new(
                    CaptureErrorCode::MarkdownWriteFailed,
                    "the completed transcript could not be staged safely for rename",
                ))
            }
        }
    }
    Err(CaptureError::new(
        CaptureErrorCode::MarkdownWriteFailed,
        "too many transcript rename staging collisions",
    ))
}

fn restore_staged_markdown(staged: &Path, previous_path: &Path) {
    // Never replace a file recreated by an editor or sync client while the
    // rename was in progress. If restoration cannot claim the old name, the
    // unique staged file remains on disk rather than risking data loss.
    let _ = markdown::rename_no_replace(staged, previous_path);
}

fn collision_safe_missing_path(
    parent: &Path,
    capture: &CaptureRecord,
) -> Result<PathBuf, CaptureError> {
    for collision in 1..=10_000 {
        let path = parent.join(markdown::filename_for_capture(capture, collision)?);
        match fs::symlink_metadata(&path) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(path),
            Err(_) => {
                return Err(CaptureError::new(
                    CaptureErrorCode::MarkdownWriteFailed,
                    "the renamed transcript destination could not be inspected",
                ))
            }
        }
    }
    Err(CaptureError::new(
        CaptureErrorCode::MarkdownWriteFailed,
        "too many transcript filename collisions",
    ))
}

fn rename_completed_markdown(
    store: &TranscriptStore,
    capture_id: i64,
    title: &str,
) -> Result<CaptureRecord, CaptureError> {
    let mut capture = store.get_capture(capture_id)?;
    if capture.status != CaptureState::Completed {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            "only completed captures can rename completed Markdown",
        ));
    }
    let Some(previous_path) = capture.markdown_path.clone() else {
        return store.update_capture(
            capture_id,
            CapturePatch {
                title: Some(title.to_owned()),
                transcript_text: None,
            },
        );
    };

    markdown::validate_final_markdown_path(&previous_path)?;
    let parent = previous_path.parent().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the stored transcript path has no parent directory",
        )
    })?;
    let canonical_parent = markdown::validate_output_directory(parent)?;
    if canonical_parent != parent {
        return Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the stored transcript directory no longer resolves safely",
        ));
    }

    let expected_title = capture.title.clone();
    capture.title = validate_title(title)?;
    match fs::symlink_metadata(&previous_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Keep Copy Path and a later regeneration aligned with the new
            // title even when the user-owned Markdown file was moved/deleted.
            let path = collision_safe_missing_path(parent, &capture)?;
            return store.complete_markdown_rename(
                capture_id,
                &expected_title,
                &previous_path,
                &capture.title,
                &path,
                false,
            );
        }
        Err(_) => {
            return Err(CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the completed transcript file could not be inspected for rename",
            ));
        }
        Ok(_) => markdown::validate_completed_file(&previous_path)?,
    }

    // Move the source to an app-unique sibling before reading or rewriting it.
    // This prevents a later path-based replacement/removal from clobbering a
    // file recreated by an editor or sync client, and also handles case-only
    // filename changes on case-insensitive macOS volumes.
    let staged_path = stage_completed_markdown(&previous_path)?;
    let operation = (|| {
        let staged_contents = fs::read(&staged_path).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the staged transcript file could not be read for rename",
            )
        })?;
        let rewritten_contents = markdown::rewrite_title(&staged_contents, &capture.title)?;

        for collision in 1..=10_000 {
            let path = parent.join(markdown::filename_for_capture(&capture, collision)?);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    // Adopt an exact regular-file match left by an interrupted
                    // prior attempt. Any other object consumes the slot and is
                    // never overwritten or removed.
                    if !metadata.file_type().is_symlink()
                        && metadata.is_file()
                        && metadata.len() == rewritten_contents.len() as u64
                        && fs::read(&path).is_ok_and(|contents| contents == rewritten_contents)
                    {
                        let renamed = store.complete_markdown_rename(
                            capture_id,
                            &expected_title,
                            &previous_path,
                            &capture.title,
                            &path,
                            true,
                        )?;
                        return Ok((renamed, staged_contents));
                    }
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match markdown::atomic_write(
                        &path,
                        &rewritten_contents,
                        markdown::WriteMode::CreateNew,
                    ) {
                        Ok(()) => {}
                        Err(error) if error.code == CaptureErrorCode::OutputFileExists => continue,
                        Err(error) => return Err(error),
                    }
                    let renamed = store.complete_markdown_rename(
                        capture_id,
                        &expected_title,
                        &previous_path,
                        &capture.title,
                        &path,
                        true,
                    )?;
                    return Ok((renamed, staged_contents));
                }
                Err(_) => {
                    return Err(CaptureError::new(
                        CaptureErrorCode::MarkdownWriteFailed,
                        "the renamed transcript destination could not be inspected",
                    ))
                }
            }
        }

        Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "too many transcript filename collisions",
        ))
    })();

    match operation {
        Ok((capture, staged_contents)) => {
            // An editor with an already-open descriptor can still modify the
            // staged inode after it moves. Never discard those newer bytes;
            // restore them under the prior name instead. A file recreated at
            // that name is likewise never replaced.
            if fs::read(&staged_path).is_ok_and(|contents| contents == staged_contents) {
                let _ = fs::remove_file(&staged_path);
            } else {
                restore_staged_markdown(&staged_path, &previous_path);
            }
            Ok(capture)
        }
        Err(error) => {
            restore_staged_markdown(&staged_path, &previous_path);
            Err(error)
        }
    }
}

fn rename_capture(
    store: &TranscriptStore,
    capture_id: i64,
    title: &str,
) -> Result<CaptureRecord, CaptureError> {
    let title = validate_title(title)?;
    // Finalization uses this same lock. Re-read status while holding it so a
    // capture cannot become completed between the decision and title update.
    markdown::with_output_lock(|| {
        let capture = store.get_capture(capture_id)?;
        if capture.status == CaptureState::Completed {
            rename_completed_markdown(store, capture_id, &title)
        } else {
            store.update_capture(
                capture_id,
                CapturePatch {
                    title: Some(title),
                    transcript_text: None,
                },
            )
        }
    })
}

fn regenerate_markdown(
    store: &TranscriptStore,
    capture_id: i64,
    overwrite: bool,
) -> Result<PathBuf, CaptureError> {
    markdown::with_output_lock(|| {
        let snapshot = store.completed_markdown_snapshot(capture_id)?;
        let path = snapshot.capture.markdown_path.as_ref().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::TranscriptFileMissing,
                "completed capture has no stored Markdown path",
            )
        })?;
        markdown::validate_final_markdown_path(path)?;
        let parent = path.parent().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the stored transcript path has no parent directory",
            )
        })?;
        let canonical_parent = markdown::validate_output_directory(parent)?;
        if canonical_parent != parent {
            return Err(CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the stored transcript directory no longer resolves safely",
            ));
        }
        let mode = markdown::ensure_regeneration_target(path, overwrite)?;
        let rendered = markdown::render(&snapshot)?;
        markdown::atomic_write(path, rendered.markdown.as_bytes(), mode)?;
        store.complete_markdown_regeneration(capture_id, path, &rendered)?;
        Ok(path.clone())
    })
}

fn completed_path_for_action(
    store: &TranscriptStore,
    capture_id: i64,
    require_file: bool,
) -> Result<PathBuf, CaptureError> {
    let path = store.completed_transcript_path(capture_id)?;
    markdown::validate_final_markdown_path(&path)?;
    if require_file {
        markdown::validate_completed_file(&path)?;
    }
    Ok(path)
}

#[tauri::command]
pub async fn get_capture_output_directory(
    app: tauri::AppHandle,
) -> Result<CaptureOutputDirectory, CaptureError> {
    feature_gate()?;
    let (documents, app_data) = resolve_output_roots(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        markdown::with_output_lock(|| markdown::get_output_directory(&documents, &app_data))
    })
    .await
    .map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::OutputDirectoryUnwritable,
            "capture output directory task failed",
        )
    })?
}

#[tauri::command]
pub async fn set_capture_output_directory(
    app: tauri::AppHandle,
    update_gate: UpdateGateState<'_>,
    path: PathBuf,
) -> Result<CaptureOutputDirectory, CaptureError> {
    feature_gate()?;
    let _update_activity = begin_update_activity(&update_gate)?;
    let (documents, app_data) = resolve_output_roots(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        markdown::with_output_lock(|| markdown::set_output_directory(&path, &documents, &app_data))
    })
    .await
    .map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::OutputDirectoryUnwritable,
            "capture output directory task failed",
        )
    })?
}

#[tauri::command]
pub async fn capture_finalize_markdown(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    feature_gate()?;
    let output_roots = resolve_output_roots(&app);
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || match output_roots {
        Ok((documents, app_data)) => {
            finalize_markdown_from_settings(&store, capture_id, &documents, &app_data)
        }
        Err(error) => {
            store.mark_markdown_finalization_failed(capture_id, &error);
            Err(error)
        }
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_regenerate_markdown(
    store: StoreState<'_>,
    capture_id: i64,
    overwrite: bool,
) -> Result<PathBuf, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || regenerate_markdown(&store, capture_id, overwrite))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_read_transcript(
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<String, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || store.completed_transcript_text(capture_id))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_read_ai_handoff_context(
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<MeetingAiHandoffContext, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || store.completed_ai_handoff_context(capture_id))
        .await
        .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_get_transcript_path(
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<PathBuf, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        completed_path_for_action(&store, capture_id, false)
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_read_transcript_file(
    store: StoreState<'_>,
    capture_id: i64,
) -> Result<String, CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let path = completed_path_for_action(&store, capture_id, true)?;
        let metadata = std::fs::metadata(&path).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::TranscriptFileMissing,
                "the completed transcript file is unavailable",
            )
        })?;
        if metadata.len() > 64 * 1024 * 1024 {
            return Err(CaptureError::new(
                CaptureErrorCode::TranscriptUnavailable,
                "the completed transcript file exceeds the read limit",
            ));
        }
        std::fs::read_to_string(path).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::TranscriptUnavailable,
                "the completed transcript file is not valid UTF-8",
            )
        })
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_open(store: StoreState<'_>, capture_id: i64) -> Result<(), CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let path = completed_path_for_action(&store, capture_id, true)?;
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(&path)
                .spawn()
                .map_err(|_| {
                    CaptureError::new(
                        CaptureErrorCode::TranscriptUnavailable,
                        "the completed transcript could not be opened",
                    )
                })?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err(CaptureError::new(
                CaptureErrorCode::UnsupportedMacos,
                "opening meeting transcripts is currently supported only on macOS",
            ))
        }
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[tauri::command]
pub async fn capture_reveal(store: StoreState<'_>, capture_id: i64) -> Result<(), CaptureError> {
    feature_gate()?;
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let path = completed_path_for_action(&store, capture_id, true)?;
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg("-R")
                .arg(&path)
                .spawn()
                .map_err(|_| {
                    CaptureError::new(
                        CaptureErrorCode::TranscriptUnavailable,
                        "the completed transcript could not be revealed in Finder",
                    )
                })?;
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Err(CaptureError::new(
                CaptureErrorCode::UnsupportedMacos,
                "revealing meeting transcripts is currently supported only on macOS",
            ))
        }
    })
    .await
    .map_err(|_| CaptureError::persistence())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestStore {
        path: PathBuf,
        root: PathBuf,
        store: TranscriptStore,
    }

    impl TestStore {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("murmur-mt1-{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            // Resolve macOS's `/var` compatibility symlink before exercising
            // the strict no-symlink database-directory boundary.
            let root = fs::canonicalize(root).unwrap();
            let path = root.join("murmur.db");
            let store = TranscriptStore::open(&path).unwrap();
            Self { path, root, store }
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn advance_to_recording(store: &TranscriptStore, id: i64) {
        for state in [
            CaptureState::SelectingSource,
            CaptureState::Starting,
            CaptureState::Recording,
        ] {
            store.transition_capture(id, state, None, None).unwrap();
        }
    }

    fn advance_to_transcribing(store: &TranscriptStore, id: i64) {
        advance_to_recording(store, id);
        for state in [CaptureState::Stopping, CaptureState::Transcribing] {
            store.transition_capture(id, state, None, None).unwrap();
        }
    }

    #[test]
    fn manual_transcription_hold_is_durable_and_does_not_cancel_other_meetings() {
        let test = TestStore::new("manual-transcription-hold");
        let first = test.store.create_capture("First", &test.root).unwrap();
        let second = test.store.create_capture("Second", &test.root).unwrap();
        let third = test.store.create_capture("Third", &test.root).unwrap();
        for id in [first.id, second.id, third.id] {
            advance_to_transcribing(&test.store, id);
        }
        test.store.set_transcription_paused(first.id, true).unwrap();
        test.store.set_transcription_paused(third.id, true).unwrap();
        assert_eq!(
            test.store.transcribing_capture_ids().unwrap(),
            vec![second.id]
        );
        let reopened = TranscriptStore::open(&test.path).unwrap();
        assert!(reopened.transcription_paused(first.id).unwrap());
        assert!(reopened.transcription_paused(third.id).unwrap());
        let summary = reopened.get_capture_summary(first.id, &test.root).unwrap();
        assert!(TranscriptStore::summary_from_row(summary).transcription_paused);
        let next = TranscriptStore::summary_from_row(
            reopened.get_capture_summary(second.id, &test.root).unwrap(),
        );
        assert!(!next.transcription_queued);
        reopened.set_transcription_paused(first.id, false).unwrap();
        let next = TranscriptStore::summary_from_row(
            reopened.get_capture_summary(second.id, &test.root).unwrap(),
        );
        assert!(next.transcription_queued);
        assert_eq!(
            reopened.transcribing_capture_ids().unwrap(),
            vec![first.id, second.id]
        );
        reopened.cancel_transcription_if_active(third.id).unwrap();
        assert!(!reopened.transcription_paused(third.id).unwrap());
        reopened.retry_transcription(third.id).unwrap();
        assert_eq!(
            reopened.transcribing_capture_ids().unwrap(),
            vec![first.id, second.id, third.id]
        );
    }

    #[test]
    fn holding_nontranscribing_capture_is_rejected_without_changing_state() {
        let test = TestStore::new("hold-invalid-state");
        let capture = test.store.create_capture("Draft", &test.root).unwrap();
        assert_eq!(
            test.store
                .set_transcription_paused(capture.id, true)
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
        assert!(!test.store.transcription_paused(capture.id).unwrap());
    }

    #[test]
    fn manually_paused_capture_cannot_be_finalized_or_lost_on_restart() {
        let test = TestStore::new("hold-finalization");
        let capture = test.store.create_capture("Paused", &test.root).unwrap();
        advance_to_transcribing(&test.store, capture.id);
        test.store
            .set_transcription_paused(capture.id, true)
            .unwrap();
        let error = test
            .store
            .enter_finalizing_if_complete(capture.id)
            .unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::TranscriptionPaused);
        let reopened = TranscriptStore::open(&test.path).unwrap();
        assert_eq!(
            reopened.get_capture(capture.id).unwrap().status,
            CaptureState::Transcribing
        );
        assert!(reopened.transcription_paused(capture.id).unwrap());
        assert!(reopened.transcribing_capture_ids().unwrap().is_empty());
    }

    fn finalizing_fixture(test: &TestStore, title: &str) -> CaptureRecord {
        let capture = test.store.create_capture(title, &test.root).unwrap();
        advance_to_recording(&test.store, capture.id);
        let microphone_chunk = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::Microphone,
                start_ms: 0,
                end_ms: 20_000,
                path: test.root.join("mic-00000.wav"),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        let system_chunk = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::System,
                start_ms: 0,
                end_ms: 20_000,
                path: test.root.join("system-00000.wav"),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        test.store
            .commit_chunk_segments(
                capture.id,
                microphone_chunk,
                &[NewCaptureSegment {
                    track: CaptureTrack::Microphone,
                    ordinal: 0,
                    start_ms: 4_000,
                    end_ms: 6_000,
                    text_raw: "Let's start".into(),
                    text_final: "Let's start".into(),
                    language: Some("en".into()),
                    confidence: Some(0.9),
                }],
            )
            .unwrap();
        test.store
            .commit_chunk_segments(
                capture.id,
                system_chunk,
                &[NewCaptureSegment {
                    track: CaptureTrack::System,
                    ordinal: 0,
                    start_ms: 11_000,
                    end_ms: 15_000,
                    text_raw: "First item".into(),
                    text_final: "First item".into(),
                    language: Some("en".into()),
                    confidence: Some(0.8),
                }],
            )
            .unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures
                 SET created_at = 1721212645123,
                     started_at = 1721212650123,
                     active_duration_ms = 20000,
                     paused_duration_ms = 2000,
                     source_kind = 'application',
                     source_name = 'Google Chrome',
                     source_bundle = 'com.google.Chrome',
                     language = 'en'
                 WHERE id = ?1",
                params![capture.id],
            )
            .unwrap();
        }
        test.store
            .transition_capture(capture.id, CaptureState::Finalizing, None, None)
            .unwrap()
    }

    fn insert_finalizing_segment(
        test: &TestStore,
        capture_id: i64,
        track: CaptureTrack,
        ordinal: i64,
        start_ms: i64,
        end_ms: i64,
        text: &str,
    ) {
        let conn = test.store.connection().unwrap();
        let chunk_id: i64 = conn
            .query_row(
                "SELECT id FROM capture_chunks WHERE capture_id = ?1 AND track = ?2",
                params![capture_id, track.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO capture_segments
                 (capture_id, chunk_id, track, ordinal, start_ms, end_ms,
                  text_raw, text_final, language, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'en', 0.9)",
            params![
                capture_id,
                chunk_id,
                track.as_str(),
                ordinal,
                start_ms,
                end_ms,
                text
            ],
        )
        .unwrap();
    }

    #[test]
    fn latest_completed_context_skips_unfinished_and_blank_captures() {
        let test = TestStore::new("latest-completed-context");
        assert_eq!(
            test.store.latest_completed_ai_handoff_context().unwrap(),
            None
        );

        let older = test.store.create_capture("Older", &test.root).unwrap();
        let newer = test.store.create_capture("Newer", &test.root).unwrap();
        let unfinished = test.store.create_capture("Unfinished", &test.root).unwrap();
        let blank = test.store.create_capture("Blank", &test.root).unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET status = 'completed', transcript_text = 'older transcript', created_at = 100 WHERE id = ?1",
                params![older.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE captures SET status = 'completed', transcript_text = 'newer transcript', created_at = 200 WHERE id = ?1",
                params![newer.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE captures SET transcript_text = 'unfinished transcript', created_at = 300 WHERE id = ?1",
                params![unfinished.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE captures SET status = 'completed', transcript_text = '   ', created_at = 400 WHERE id = ?1",
                params![blank.id],
            )
            .unwrap();
        }

        assert_eq!(
            test.store.latest_completed_ai_handoff_context().unwrap(),
            Some(MeetingAiHandoffContext {
                title: "Newer".into(),
                transcript: "newer transcript".into(),
            })
        );
    }

    #[test]
    fn capture_stats_count_only_completed_meetings() {
        let test = TestStore::new("capture-stats");
        let first = test.store.create_capture("First", &test.root).unwrap();
        let second = test.store.create_capture("Second", &test.root).unwrap();
        let _unfinished = test.store.create_capture("Unfinished", &test.root).unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET status = 'completed' WHERE id IN (?1, ?2)",
                params![first.id, second.id],
            )
            .unwrap();
        }

        let stats = test.store.capture_stats().unwrap();
        assert_eq!(stats.total_completed, 2);
        assert_eq!(stats.total_deletable, 3);

        assert!(test.store.delete_capture_metadata(first.id).unwrap());
        let stats = test.store.capture_stats().unwrap();
        assert_eq!(stats.total_completed, 1);
        assert_eq!(stats.total_deletable, 2);
    }

    #[test]
    fn meeting_transcript_retention_selects_only_old_completed_captures() {
        let test = TestStore::new("meeting-transcript-retention");
        let old = test.store.create_capture("Old", &test.root).unwrap();
        let recent = test.store.create_capture("Recent", &test.root).unwrap();
        let unfinished = test.store.create_capture("Unfinished", &test.root).unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET status = 'completed', created_at = 100 WHERE id = ?1",
                params![old.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE captures SET status = 'completed', created_at = 300 WHERE id = ?1",
                params![recent.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE captures SET created_at = 50 WHERE id = ?1",
                params![unfinished.id],
            )
            .unwrap();
        }

        assert_eq!(
            test.store.completed_capture_ids_older_than(200).unwrap(),
            vec![old.id]
        );
        assert_eq!(
            test.store.deletable_capture_ids().unwrap(),
            vec![unfinished.id, old.id, recent.id]
        );
    }

    #[test]
    fn meeting_transcript_retention_defaults_to_forever() {
        let retention = MeetingTranscriptRetentionState::new();
        assert_eq!(retention.days(), 0);
        assert_eq!(retention.cutoff_millis(), None);

        retention.set_days(30);
        assert_eq!(retention.days(), 30);
        assert!(retention.cutoff_millis().is_some());
    }

    #[test]
    fn processing_profile_is_atomic_and_immutable() {
        let test = TestStore::new("processing-profile");
        let profile = ProcessingProfileRecord {
            stt_model_id: "model-a".into(),
            language: Some("en".into()),
            vocabulary_prompt: Some("Glossary: Murmur.".into()),
            correction_rules: vec![crate::corrections::CorrectionRule {
                find: "murmur".into(),
                replace: "Murmur".into(),
                case_sensitive: false,
                whole_word: true,
            }],
        };
        let capture = test
            .store
            .create_capture_with_processing_profile("Profile", &test.root, &profile)
            .unwrap();
        assert_eq!(
            test.store
                .get_processing_profile(capture.id)
                .unwrap()
                .stt_model_id,
            "model-a"
        );
        let loaded = test.store.get_processing_profile(capture.id).unwrap();
        assert_eq!(loaded.language.as_deref(), Some("en"));
        assert_eq!(
            loaded.vocabulary_prompt.as_deref(),
            Some("Glossary: Murmur.")
        );
        assert_eq!(loaded.correction_rules.len(), 1);
    }

    #[test]
    fn new_meetings_auto_detect_while_saved_legacy_profiles_survive_resume() {
        let test = TestStore::new("meeting-language-independence");
        let legacy_profile = ProcessingProfileRecord {
            stt_model_id: "model-a".into(),
            language: Some("es".into()),
            vocabulary_prompt: None,
            correction_rules: Vec::new(),
        };
        let legacy = test
            .store
            .create_capture_with_processing_profile("Legacy", &test.root, &legacy_profile)
            .unwrap();
        advance_to_transcribing(&test.store, legacy.id);

        let settings = processing::MeetingProcessingSettingsState::new();
        settings.set_model("model-a".into());
        settings.set_vocabulary_prompt(None);
        settings.set_corrections(Vec::new());
        let fresh = test
            .store
            .create_capture_with_processing_profile(
                "New",
                &test.root,
                &settings.snapshot().unwrap().into(),
            )
            .unwrap();
        assert_eq!(
            test.store
                .get_processing_profile(fresh.id)
                .unwrap()
                .language,
            None
        );
        // Simulate restart: processing loads the immutable profile from disk, not
        // the new-meeting snapshot or current dictation auto-detection behavior.
        let reopened = TranscriptStore::open(&test.path).unwrap();
        let resumed_profile = reopened.get_processing_profile(legacy.id).unwrap();
        assert_eq!(resumed_profile.language.as_deref(), Some("es"));
        assert_eq!(resumed_profile.stt_model_id, legacy_profile.stt_model_id);
        assert_eq!(
            reopened.get_capture(legacy.id).unwrap().status,
            CaptureState::Transcribing
        );
        assert_eq!(
            reopened.get_processing_profile(fresh.id).unwrap().language,
            None
        );
    }

    #[test]
    fn state_machine_accepts_only_declared_edges() {
        use CaptureState::*;
        let states = [
            Draft,
            SelectingSource,
            Starting,
            Recording,
            Pausing,
            Paused,
            Resuming,
            Stopping,
            Transcribing,
            Finalizing,
            Completed,
            Interrupted,
            CaptureFailed,
            TranscriptionFailed,
            FinalizationFailed,
            Deleting,
            CleanupFailed,
            Cancelled,
        ];
        let legal = [
            (Draft, SelectingSource),
            (Draft, Deleting),
            (SelectingSource, Starting),
            (SelectingSource, Deleting),
            (Starting, Recording),
            (Starting, Interrupted),
            (Starting, CaptureFailed),
            (Starting, Deleting),
            (Recording, Pausing),
            (Recording, Stopping),
            (Recording, Interrupted),
            (Recording, CaptureFailed),
            (Recording, Deleting),
            (Pausing, Paused),
            (Pausing, Interrupted),
            (Pausing, CaptureFailed),
            (Pausing, Deleting),
            (Paused, Resuming),
            (Paused, Stopping),
            (Paused, Interrupted),
            (Paused, CaptureFailed),
            (Paused, Deleting),
            (Resuming, Recording),
            (Resuming, Interrupted),
            (Resuming, CaptureFailed),
            (Resuming, Deleting),
            (Stopping, Transcribing),
            (Stopping, Interrupted),
            (Stopping, CaptureFailed),
            (Stopping, Deleting),
            (Transcribing, Finalizing),
            (Transcribing, TranscriptionFailed),
            (Transcribing, Deleting),
            (Finalizing, Completed),
            (Finalizing, FinalizationFailed),
            (Finalizing, Deleting),
            (Interrupted, Transcribing),
            (Interrupted, Deleting),
            (CaptureFailed, Transcribing),
            (CaptureFailed, Deleting),
            (TranscriptionFailed, Transcribing),
            (TranscriptionFailed, Deleting),
            (FinalizationFailed, Finalizing),
            (FinalizationFailed, Deleting),
            (Deleting, Cancelled),
            (Deleting, CleanupFailed),
            (CleanupFailed, Deleting),
            (Completed, Deleting),
        ];
        for from in states {
            for to in states {
                assert_eq!(
                    is_legal_transition(from, to),
                    legal.contains(&(from, to)),
                    "unexpected transition result for {} -> {}",
                    from.as_str(),
                    to.as_str()
                );
            }
        }
    }

    #[test]
    fn feature_gate_matches_build_configuration() {
        #[cfg(feature = "meeting-capture")]
        assert!(feature_gate().is_ok());
        #[cfg(not(feature = "meeting-capture"))]
        assert_eq!(
            feature_gate().unwrap_err().code,
            CaptureErrorCode::FeatureDisabled
        );
    }

    #[test]
    fn capture_state_survives_database_reopen() {
        let test = TestStore::new("restart");
        let capture = test.store.create_capture("Planning", &test.root).unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::SelectingSource, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Starting, None, None)
            .unwrap();
        let reopened = TranscriptStore::open(&test.path).unwrap();
        let capture = reopened.get_capture(capture.id).unwrap();
        assert_eq!(capture.status, CaptureState::Starting);
        assert_eq!(capture.title, "Planning");
    }

    /// Build the staging directory a force-quit leaves behind: one cleanly
    /// published chunk pair, and a second pair still open as `.partial` with
    /// its length fields never patched.
    fn interrupted_staging(test: &TestStore, capture_id: i64, staging: &Path) {
        fn published_wav(path: &Path, channels: u16, frames: u32) -> i64 {
            let spec = hound::WavSpec {
                channels,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(path, spec).unwrap();
            for index in 0..frames * u32::from(channels) {
                writer.write_sample(index as i16).unwrap();
            }
            writer.finalize().unwrap();
            fs::metadata(path).unwrap().len() as i64
        }
        fn crashed_wav(path: &Path, channels: u16, frames: u32) {
            let block_align = channels * 2;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(b"WAVE");
            bytes.extend_from_slice(b"fmt ");
            bytes.extend_from_slice(&16_u32.to_le_bytes());
            bytes.extend_from_slice(&1_u16.to_le_bytes());
            bytes.extend_from_slice(&channels.to_le_bytes());
            bytes.extend_from_slice(&48_000_u32.to_le_bytes());
            bytes.extend_from_slice(&(48_000 * u32::from(block_align)).to_le_bytes());
            bytes.extend_from_slice(&block_align.to_le_bytes());
            bytes.extend_from_slice(&16_u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend(vec![7_u8; (frames * u32::from(block_align)) as usize]);
            fs::write(path, &bytes).unwrap();
        }

        fs::create_dir_all(staging).unwrap();
        // Chunk 0 published: 48 000 mono mic frames (1 s), 48 000 stereo
        // system frames. Per-track endpoints deliberately differ.
        let mic_bytes = published_wav(&staging.join("mic-00000.wav"), 1, 48_000);
        let system_bytes = published_wav(&staging.join("system-00000.wav"), 2, 47_900);
        // Chunk 1 still open when the process died.
        crashed_wav(&staging.join("mic-00001.wav.partial"), 1, 24_000);
        crashed_wav(&staging.join("system-00001.wav.partial"), 2, 23_800);

        let mut manifest = recovery::Manifest::empty(capture_id, 1);
        recovery::push_manifest_chunk(
            &mut manifest,
            0,
            Some((PathBuf::from("mic-00000.wav"), 0, 1_000)),
            Some((PathBuf::from("system-00000.wav"), 0, 997)),
        );
        recovery::atomic_write_manifest(staging, &manifest).unwrap();

        for (track, end_ms, path, byte_length) in [
            (
                CaptureTrack::Microphone,
                1_000,
                staging.join("mic-00000.wav"),
                mic_bytes,
            ),
            (
                CaptureTrack::System,
                997,
                staging.join("system-00000.wav"),
                system_bytes,
            ),
        ] {
            test.store
                .create_capture_chunk(&NewCaptureChunk {
                    capture_id,
                    ordinal: 0,
                    track,
                    start_ms: 0,
                    end_ms,
                    path,
                    byte_length,
                    status: CaptureChunkState::Ready,
                })
                .unwrap();
        }
    }

    #[test]
    fn recovery_repairs_open_partials_into_transcribable_chunks() {
        let test = TestStore::new("recover-open-chunks");
        let capture = test.store.create_capture("Crashed", &test.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&test.store, capture.id);
        interrupted_staging(&test, capture.id, &staging);
        test.store.mark_stale_captures_interrupted().unwrap();
        assert_eq!(
            test.store.get_capture(capture.id).unwrap().status,
            CaptureState::Interrupted
        );

        let recovered = test.store.recover_open_chunks(capture.id).unwrap();
        assert_eq!(recovered, 2);

        // No `.partial` survives, so the processing validator will accept the
        // directory at all.
        assert!(!staging.join("mic-00001.wav.partial").exists());
        assert!(!staging.join("system-00001.wav.partial").exists());
        assert!(staging.join("mic-00001.wav").exists());
        assert!(staging.join("system-00001.wav").exists());

        // The recovered tail is real, readable audio, not an empty stub.
        let mic = hound::WavReader::open(staging.join("mic-00001.wav")).unwrap();
        assert_eq!(mic.duration(), 24_000);
        let system = hound::WavReader::open(staging.join("system-00001.wav")).unwrap();
        assert_eq!(system.duration(), 23_800);

        // Each track continues from its own endpoint, not the merged one.
        let units = test.store.list_processing_units(capture.id).unwrap();
        assert_eq!(units.len(), 4);
        let tail = |track: CaptureTrack| {
            units
                .iter()
                .find(|unit| unit.ordinal == 1 && unit.track == track)
                .unwrap()
        };
        assert_eq!(tail(CaptureTrack::Microphone).start_ms, 1_000);
        assert_eq!(tail(CaptureTrack::Microphone).end_ms, 1_500);
        assert_eq!(tail(CaptureTrack::System).start_ms, 997);
        assert_eq!(tail(CaptureTrack::System).end_ms, 1_492);

        // And the whole directory now passes the gate that used to reject it.
        let units = test.store.list_processing_units(capture.id).unwrap();
        recovery::validate_processing_manifest(&staging, capture.id, &test.root, &units).unwrap();
    }

    #[test]
    fn summary_retry_capability_requires_readable_audio_without_mutating_recovery() {
        let empty = TestStore::new("retry-capability-empty");
        let capture = empty.store.create_capture("Empty", &empty.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        fs::create_dir_all(&staging).unwrap();
        recovery::atomic_write_manifest(&staging, &recovery::Manifest::empty(capture.id, 1))
            .unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        hound::WavWriter::create(staging.join("mic-00000.wav"), spec)
            .unwrap()
            .finalize()
            .unwrap();
        advance_to_recording(&empty.store, capture.id);
        empty.store.mark_stale_captures_interrupted().unwrap();
        let summary = TranscriptStore::summary_from_row(
            empty
                .store
                .get_capture_summary(capture.id, &empty.root)
                .unwrap(),
        );
        assert!(summary.has_recovery_audio);
        assert!(!summary.can_retry_transcription);

        let indexed = TestStore::new("retry-capability-indexed");
        let capture = indexed
            .store
            .create_capture("Indexed", &indexed.root)
            .unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&indexed.store, capture.id);
        interrupted_staging(&indexed, capture.id, &staging);
        for name in ["mic-00001.wav.partial", "system-00001.wav.partial"] {
            fs::remove_file(staging.join(name)).unwrap();
        }
        indexed.store.mark_stale_captures_interrupted().unwrap();
        let summary = TranscriptStore::summary_from_row(
            indexed
                .store
                .get_capture_summary(capture.id, &indexed.root)
                .unwrap(),
        );
        assert!(summary.can_retry_transcription);
        assert_eq!(
            serde_json::to_value(&summary).unwrap()["can_retry_transcription"],
            true,
        );
        assert_eq!(summary.retry_unavailable_reason, None);

        // Older rows without their immutable processing settings retain audio,
        // but must not advertise a retry that the coordinator will reject.
        indexed
            .store
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM capture_processing_profiles WHERE capture_id = ?1",
                params![capture.id],
            )
            .unwrap();
        let missing_profile = TranscriptStore::summary_from_row(
            indexed
                .store
                .get_capture_summary(capture.id, &indexed.root)
                .unwrap(),
        );
        assert!(!missing_profile.can_retry_transcription);
        assert_eq!(
            missing_profile.retry_unavailable_reason,
            Some(CaptureErrorCode::ProcessingProfileMissing)
        );

        let partial = TestStore::new("retry-capability-partial");
        let capture = partial
            .store
            .create_capture("Crash open", &partial.root)
            .unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&partial.store, capture.id);
        interrupted_staging(&partial, capture.id, &staging);
        for name in ["mic-00000.wav", "system-00000.wav"] {
            fs::remove_file(staging.join(name)).unwrap();
        }
        {
            let conn = partial.store.connection().unwrap();
            conn.execute(
                "DELETE FROM capture_chunks WHERE capture_id = ?1",
                params![capture.id],
            )
            .unwrap();
        }
        recovery::atomic_write_manifest(&staging, &recovery::Manifest::empty(capture.id, 1))
            .unwrap();
        partial.store.mark_stale_captures_interrupted().unwrap();
        let before = [
            "manifest.json",
            "mic-00001.wav.partial",
            "system-00001.wav.partial",
        ]
        .map(|name| (name, fs::read(staging.join(name)).unwrap()));
        let summary = TranscriptStore::summary_from_row(
            partial
                .store
                .get_capture_summary(capture.id, &partial.root)
                .unwrap(),
        );
        assert!(summary.can_retry_transcription);
        for (name, bytes) in before {
            assert_eq!(
                fs::read(staging.join(name)).unwrap(),
                bytes,
                "summary listing must not mutate {name}"
            );
        }
    }

    #[test]
    fn summary_retry_capability_accepts_unindexed_audio_and_fails_closed() {
        let test = TestStore::new("retry-capability-fail-closed");
        let capture = test.store.create_capture("Unindexed", &test.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        fs::create_dir_all(&staging).unwrap();
        recovery::atomic_write_manifest(&staging, &recovery::Manifest::empty(capture.id, 1))
            .unwrap();
        let audio_path = staging.join("mic-00000.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&audio_path, spec).unwrap();
        writer.write_sample(1_i16).unwrap();
        writer.finalize().unwrap();

        assert!(recovery::has_recoverable_audio(
            capture.id,
            Some(&staging),
            &test.root,
            &[],
        ));
        assert!(!recovery::has_recoverable_audio(
            capture.id,
            Some(Path::new("../unsafe-staging")),
            &test.root,
            &[],
        ));
        assert!(!recovery::has_recoverable_audio(
            capture.id,
            Some(&test.root.join("not-the-capture")),
            &test.root,
            &[],
        ));

        fs::remove_file(&audio_path).unwrap();
        assert!(!recovery::has_recoverable_audio(
            capture.id,
            Some(&staging),
            &test.root,
            &[],
        ));

        fs::write(&audio_path, b"not a wav").unwrap();
        assert!(!recovery::has_recoverable_audio(
            capture.id,
            Some(&staging),
            &test.root,
            &[],
        ));

        fs::remove_file(&audio_path).unwrap();
        fs::write(staging.join("manifest.json"), b"not json").unwrap();
        assert!(!recovery::has_recoverable_audio(
            capture.id,
            Some(&staging),
            &test.root,
            &[],
        ));

        fs::remove_dir_all(&staging).unwrap();
        assert!(!recovery::has_recoverable_audio(
            capture.id,
            Some(&staging),
            &test.root,
            &[],
        ));
    }

    #[test]
    fn recovery_is_safe_to_run_twice_and_adopts_orphaned_renames() {
        let test = TestStore::new("recover-idempotent");
        let capture = test.store.create_capture("Crashed", &test.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&test.store, capture.id);
        interrupted_staging(&test, capture.id, &staging);
        test.store.mark_stale_captures_interrupted().unwrap();

        assert_eq!(test.store.recover_open_chunks(capture.id).unwrap(), 2);
        // A second pass has nothing left to do and must not duplicate rows.
        assert_eq!(test.store.recover_open_chunks(capture.id).unwrap(), 0);
        assert_eq!(
            test.store.list_processing_units(capture.id).unwrap().len(),
            4
        );

        let units = test.store.list_processing_units(capture.id).unwrap();
        recovery::validate_processing_manifest(&staging, capture.id, &test.root, &units).unwrap();
    }

    #[test]
    fn recovery_removes_empty_unindexed_wav_beside_valid_audio() {
        let test = TestStore::new("recover-empty-unindexed");
        let capture = test.store.create_capture("Crashed", &test.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&test.store, capture.id);
        interrupted_staging(&test, capture.id, &staging);
        for name in ["mic-00001.wav.partial", "system-00001.wav.partial"] {
            fs::remove_file(staging.join(name)).unwrap();
        }
        // Keep ordinal 0 as a valid one-sided microphone chunk, then add an
        // empty unindexed system counterpart at that same ordinal.
        fs::remove_file(staging.join("system-00000.wav")).unwrap();
        test.store
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM capture_chunks WHERE capture_id = ?1 AND track = 'system'",
                params![capture.id],
            )
            .unwrap();
        let mut manifest = recovery::Manifest::empty(capture.id, 1);
        recovery::push_manifest_chunk(
            &mut manifest,
            0,
            Some((PathBuf::from("mic-00000.wav"), 0, 1_000)),
            None,
        );
        recovery::atomic_write_manifest(&staging, &manifest).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        hound::WavWriter::create(staging.join("system-00000.wav"), spec)
            .unwrap()
            .finalize()
            .unwrap();
        test.store.mark_stale_captures_interrupted().unwrap();

        let summary = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(capture.id, &test.root)
                .unwrap(),
        );
        assert!(summary.can_retry_transcription);
        assert_eq!(test.store.recover_open_chunks(capture.id).unwrap(), 0);
        assert!(!staging.join("system-00000.wav").exists());
        let units = test.store.list_processing_units(capture.id).unwrap();
        recovery::validate_processing_manifest(&staging, capture.id, &test.root, &units).unwrap();

        // A nonempty same-ordinal counterpart cannot yet be merged into the
        // existing manifest chunk, so capability discovery must fail closed.
        let mut writer = hound::WavWriter::create(staging.join("system-00000.wav"), spec).unwrap();
        writer.write_sample(1_i16).unwrap();
        writer.finalize().unwrap();
        let blocked = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(capture.id, &test.root)
                .unwrap(),
        );
        assert!(!blocked.can_retry_transcription);
        assert_eq!(
            blocked.retry_unavailable_reason,
            Some(CaptureErrorCode::AudioUnavailable)
        );
    }

    #[test]
    fn recovery_discards_a_partial_that_never_captured_a_frame() {
        let test = TestStore::new("recover-empty");
        let capture = test.store.create_capture("Crashed", &test.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&test.store, capture.id);
        interrupted_staging(&test, capture.id, &staging);
        // Replace the open pair with header-only files: the process died
        // before any audio reached the disk.
        for name in ["mic-00001.wav.partial", "system-00001.wav.partial"] {
            let path = staging.join(name);
            let bytes = fs::read(&path).unwrap();
            fs::write(&path, &bytes[..44]).unwrap();
        }
        test.store.mark_stale_captures_interrupted().unwrap();

        assert_eq!(test.store.recover_open_chunks(capture.id).unwrap(), 0);
        assert!(!staging.join("mic-00001.wav.partial").exists());
        assert!(!staging.join("mic-00001.wav").exists());
        assert_eq!(
            test.store.list_processing_units(capture.id).unwrap().len(),
            2
        );

        let units = test.store.list_processing_units(capture.id).unwrap();
        recovery::validate_processing_manifest(&staging, capture.id, &test.root, &units).unwrap();
    }

    #[test]
    fn recovery_refuses_a_capture_that_is_not_interrupted() {
        let test = TestStore::new("recover-wrong-state");
        let capture = test.store.create_capture("Live", &test.root).unwrap();
        let staging = capture.staging_path.clone().unwrap();
        advance_to_recording(&test.store, capture.id);
        interrupted_staging(&test, capture.id, &staging);

        let error = test.store.recover_open_chunks(capture.id).unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::InvalidStateTransition);
        // A recording capture keeps its open chunk untouched.
        assert!(staging.join("mic-00001.wav.partial").exists());
    }

    #[test]
    fn illegal_transition_returns_typed_error_without_mutation() {
        let test = TestStore::new("illegal-transition");
        let capture = test.store.create_capture("Test", &test.root).unwrap();
        let error = test
            .store
            .transition_capture(capture.id, CaptureState::Completed, None, None)
            .unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::InvalidStateTransition);
        assert_eq!(
            test.store.get_capture(capture.id).unwrap().status,
            CaptureState::Draft
        );
    }

    #[test]
    fn ephemeral_store_rejects_capture_creation_and_start() {
        let store = TranscriptStore::open_ephemeral().unwrap();
        let root = std::env::temp_dir();
        let create_error = store.create_capture("Unsafe", &root).unwrap_err();
        assert_eq!(create_error.code, CaptureErrorCode::PersistenceUnavailable);

        let conn = store.connection().unwrap();
        conn.execute(
            "INSERT INTO captures
                (kind, status, title, created_at, updated_at)
             VALUES ('meeting', 'draft', 'fixture', 1, 1)",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        drop(conn);
        let start_error = store
            .transition_capture(id, CaptureState::SelectingSource, None, None)
            .unwrap_err();
        assert_eq!(start_error.code, CaptureErrorCode::PersistenceUnavailable);
    }

    #[test]
    fn crud_orders_updates_searches_and_deletes() {
        let test = TestStore::new("crud");
        let first = test.store.create_capture("First", &test.root).unwrap();
        let second = test.store.create_capture("Second", &test.root).unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET created_at = 10, updated_at = 10 WHERE id = ?1",
                params![first.id],
            )
            .unwrap();
            conn.execute(
                "UPDATE captures SET created_at = 20, updated_at = 20 WHERE id = ?1",
                params![second.id],
            )
            .unwrap();
        }
        let listed = test.store.list_captures(10, 0).unwrap();
        assert_eq!(
            listed.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![second.id, first.id]
        );

        test.store
            .update_capture(
                second.id,
                CapturePatch {
                    title: Some("Beta draft without transcript".into()),
                    transcript_text: None,
                },
            )
            .unwrap();

        advance_to_transcribing(&test.store, first.id);
        let updated = test
            .store
            .update_capture(
                first.id,
                CapturePatch {
                    title: Some("Roadmap Review".into()),
                    transcript_text: Some("alpha launch notes".into()),
                },
            )
            .unwrap();
        assert_eq!(updated.word_count, 3);
        assert_eq!(test.store.search_captures("roadm", 10).unwrap().len(), 1);
        assert_eq!(test.store.search_captures("alpha", 10).unwrap().len(), 1);
        test.store
            .update_capture(
                first.id,
                CapturePatch {
                    title: None,
                    transcript_text: Some("beta release notes".into()),
                },
            )
            .unwrap();
        assert!(test.store.search_captures("alpha", 10).unwrap().is_empty());
        // The second capture has a matching title but no transcript, so it is
        // intentionally excluded from content search.
        assert_eq!(test.store.search_captures("beta", 10).unwrap().len(), 1);

        test.store
            .transition_capture(first.id, CaptureState::Finalizing, None, None)
            .unwrap();
        test.store
            .transition_capture(first.id, CaptureState::Completed, None, None)
            .unwrap();
        assert!(test.store.delete_capture_metadata(first.id).unwrap());
        assert!(test.store.search_captures("beta", 10).unwrap().is_empty());
        assert_eq!(
            test.store.get_capture(first.id).unwrap_err().code,
            CaptureErrorCode::CaptureNotFound
        );
    }

    #[test]
    fn concurrent_transition_commits_once() {
        use std::sync::{Arc, Barrier};

        let test = TestStore::new("concurrent-transition");
        let store = Arc::new(TranscriptStore::open(&test.path).unwrap());
        let capture_id = store.create_capture("Concurrent", &test.root).unwrap().id;
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                store.transition_capture(capture_id, CaptureState::SelectingSource, None, None)
            }));
        }
        barrier.wait();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter_map(|result| result.as_ref().err())
                .next()
                .unwrap()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
    }

    #[test]
    fn store_health_reports_durable_and_ephemeral_provenance() {
        let test = TestStore::new("health");
        assert_eq!(test.store.health(), StoreHealth::Durable);
        let ephemeral = TranscriptStore::open_ephemeral().unwrap();
        assert_eq!(
            ephemeral.health(),
            StoreHealth::Ephemeral {
                reason: "durable_storage_unavailable".into()
            }
        );
    }

    #[test]
    fn chunk_uniqueness_and_successful_segment_checkpoint() {
        let test = TestStore::new("chunk-success");
        let capture = test.store.create_capture("Chunk", &test.root).unwrap();
        advance_to_recording(&test.store, capture.id);
        let chunk = NewCaptureChunk {
            capture_id: capture.id,
            ordinal: 0,
            track: CaptureTrack::System,
            start_ms: 0,
            end_ms: 1000,
            path: test.root.join("system-00000.wav"),
            byte_length: 100,
            status: CaptureChunkState::Ready,
        };
        let chunk_id = test.store.create_capture_chunk(&chunk).unwrap();
        assert_eq!(
            test.store.create_capture_chunk(&chunk).unwrap_err().code,
            CaptureErrorCode::CaptureConflict
        );
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        test.store
            .commit_chunk_segments(
                capture.id,
                chunk_id,
                &[NewCaptureSegment {
                    track: CaptureTrack::System,
                    ordinal: 0,
                    start_ms: 0,
                    end_ms: 500,
                    text_raw: "hello".into(),
                    text_final: "hello".into(),
                    language: Some("en".into()),
                    confidence: Some(0.9),
                }],
            )
            .unwrap();

        let conn = test.store.connection().unwrap();
        let (status, segment_count): (String, i64) = conn
            .query_row(
                "SELECT cc.status,
                        (SELECT COUNT(*) FROM capture_segments cs
                         WHERE cs.chunk_id = cc.id)
                 FROM capture_chunks cc WHERE cc.id = ?1",
                params![chunk_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "transcribed");
        assert_eq!(segment_count, 1);
        drop(conn);
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .completed_chunk_count,
            1
        );
    }

    #[test]
    fn cancelling_transcription_preserves_committed_work_and_allows_retry() {
        let test = TestStore::new("cancel-transcription");
        let capture = test.store.create_capture("Cancel", &test.root).unwrap();
        advance_to_recording(&test.store, capture.id);
        let first = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::Microphone,
                start_ms: 0,
                end_ms: 1_000,
                path: test.root.join("mic-00000.wav"),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        let second = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 1,
                track: CaptureTrack::Microphone,
                start_ms: 1_000,
                end_ms: 2_000,
                path: test.root.join("mic-00001.wav"),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE capture_chunks SET status = 'transcribed' WHERE id = ?1",
                params![first],
            )
            .unwrap();
            conn.execute(
                "UPDATE capture_chunks SET status = 'transcribing' WHERE id = ?1",
                params![second],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO capture_segments
                    (capture_id, chunk_id, track, ordinal, start_ms, end_ms,
                     text_raw, text_final)
                 VALUES (?1, ?2, 'microphone', 0, 0, 500, 'kept', 'kept')",
                params![capture.id, first],
            )
            .unwrap();
        }

        let cancelled = test
            .store
            .cancel_transcription_if_active(capture.id)
            .unwrap();
        assert_eq!(cancelled.status, CaptureState::TranscriptionFailed);
        assert_eq!(
            cancelled.error_code.as_deref(),
            Some("transcription_cancelled")
        );
        assert_eq!(cancelled.staging_path, capture.staging_path);
        let conn = test.store.connection().unwrap();
        let first_status: String = conn
            .query_row(
                "SELECT status FROM capture_chunks WHERE id = ?1",
                params![first],
                |row| row.get(0),
            )
            .unwrap();
        let second_status: String = conn
            .query_row(
                "SELECT status FROM capture_chunks WHERE id = ?1",
                params![second],
                |row| row.get(0),
            )
            .unwrap();
        let segment_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM capture_segments WHERE chunk_id = ?1",
                params![first],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(first_status, "transcribed");
        assert_eq!(second_status, "ready");
        assert_eq!(segment_count, 1);
        assert_eq!(
            test.store.retry_transcription(capture.id).unwrap().status,
            CaptureState::Transcribing
        );
    }

    #[test]
    fn chunk_segment_transaction_rolls_back_on_duplicate() {
        let test = TestStore::new("rollback");
        let capture = test.store.create_capture("Segments", &test.root).unwrap();
        advance_to_recording(&test.store, capture.id);
        let chunk_id = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::Microphone,
                start_ms: 0,
                end_ms: 1000,
                path: test.root.join("mic-00000.wav"),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        let duplicate = NewCaptureSegment {
            track: CaptureTrack::Microphone,
            ordinal: 0,
            start_ms: 0,
            end_ms: 500,
            text_raw: "hello".into(),
            text_final: "hello".into(),
            language: Some("en".into()),
            confidence: Some(0.9),
        };
        assert!(test
            .store
            .commit_chunk_segments(capture.id, chunk_id, &[duplicate.clone(), duplicate])
            .is_err());

        let conn = test.store.connection().unwrap();
        let segment_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM capture_segments WHERE chunk_id = ?1",
                params![chunk_id],
                |row| row.get(0),
            )
            .unwrap();
        let chunk_status: String = conn
            .query_row(
                "SELECT status FROM capture_chunks WHERE id = ?1",
                params![chunk_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(segment_count, 0);
        assert_eq!(chunk_status, "ready");
        drop(conn);
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .completed_chunk_count,
            0
        );
    }

    #[test]
    fn empty_processed_microphone_unit_advances_once_without_inserting_rows() {
        let test = TestStore::new("gated-empty-mic-unit");
        let capture = test
            .store
            .create_capture("Quiet meeting", &test.root)
            .unwrap();
        advance_to_recording(&test.store, capture.id);
        let path = test.root.join("quiet.wav");
        let chunk_id = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::Microphone,
                start_ms: 0,
                end_ms: 1_000,
                path: path.clone(),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        test.store
            .commit_processed_unit(capture.id, chunk_id, &[])
            .unwrap();
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .completed_chunk_count,
            1
        );
        let conn = test.store.connection().unwrap();
        let segment_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM capture_segments WHERE capture_id = ?1",
                params![capture.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(segment_count, 0);
        drop(conn);
        let unit = test
            .store
            .list_processing_units(capture.id)
            .unwrap()
            .remove(0);
        assert_eq!(unit.status, CaptureChunkState::Transcribed);
        assert_eq!(unit.path, path);
        assert_eq!(
            test.store
                .commit_processed_unit(capture.id, chunk_id, &[])
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
        assert_eq!(
            test.store
                .enter_finalizing_if_complete(capture.id)
                .unwrap_err()
                .code,
            CaptureErrorCode::NoSpeechRecognized
        );
    }

    #[test]
    fn completed_audio_without_recognized_words_is_distinct_from_incomplete_processing() {
        let test = TestStore::new("no-speech-meeting");
        // Missing audio must not be mistaken for a silent recording.
        let no_audio = test.store.create_capture("No audio", &test.root).unwrap();
        advance_to_recording(&test.store, no_audio.id);
        test.store
            .transition_capture(no_audio.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(no_audio.id, CaptureState::Transcribing, None, None)
            .unwrap();
        assert_eq!(
            test.store
                .enter_finalizing_if_complete(no_audio.id)
                .unwrap_err()
                .code,
            CaptureErrorCode::TranscriptionFailed
        );

        let capture = test
            .store
            .create_capture("Quiet meeting", &test.root)
            .unwrap();
        advance_to_recording(&test.store, capture.id);
        let chunk_id = test
            .store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::Microphone,
                start_ms: 0,
                end_ms: 1000,
                path: test.root.join("quiet.wav"),
                byte_length: 100,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        assert_eq!(
            test.store
                .enter_finalizing_if_complete(capture.id)
                .unwrap_err()
                .code,
            CaptureErrorCode::TranscriptionFailed
        );
        test.store
            .commit_chunk_segments(capture.id, chunk_id, &[])
            .unwrap();
        let result = test
            .store
            .enter_finalizing_if_complete(capture.id)
            .unwrap_err();
        assert_eq!(result.code, CaptureErrorCode::NoSpeechRecognized);
        test.store
            .fail_transcription_if_active(capture.id, result.code)
            .unwrap();
        let saved = test.store.get_capture(capture.id).unwrap();
        assert_eq!(saved.status, CaptureState::TranscriptionFailed);
        assert_eq!(saved.error_code.as_deref(), Some("no_speech_recognized"));
        assert_eq!(saved.staging_path, capture.staging_path);

        // Existing generic failures are presented accurately on upgrade
        // without silently rewriting their database rows or deleting audio.
        test.store
            .connection()
            .unwrap()
            .execute(
                "UPDATE captures SET error_code = 'transcription_failed' WHERE id = ?1",
                params![capture.id],
            )
            .unwrap();
        let upgraded = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(capture.id, &test.root)
                .unwrap(),
        );
        assert_eq!(upgraded.error_code.as_deref(), Some("no_speech_recognized"));
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .error_code
                .as_deref(),
            Some("transcription_failed")
        );
    }

    #[test]
    fn external_transition_cannot_bypass_markdown_completion() {
        assert_eq!(
            validate_external_transition_target(CaptureState::Completed)
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
        assert_eq!(
            validate_external_transition_target(CaptureState::Finalizing)
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
    }

    #[test]
    fn finalization_writes_markdown_then_updates_database_path() {
        let test = TestStore::new("markdown-finalize");
        let capture = finalizing_fixture(&test, "Project / Planning");
        insert_finalizing_segment(
            &test,
            capture.id,
            CaptureTrack::Microphone,
            1,
            6_500,
            8_000,
            "Second thought",
        );
        insert_finalizing_segment(
            &test,
            capture.id,
            CaptureTrack::Microphone,
            2,
            12_000,
            13_000,
            "Right",
        );
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();

        assert_eq!(completed.status, CaptureState::Completed);
        let path = completed.markdown_path.unwrap();
        assert!(path.is_absolute());
        assert!(path.exists());
        assert!(!path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".tmp"));
        let markdown = fs::read_to_string(&path).unwrap();
        assert!(markdown.starts_with("---\nid: "));
        assert!(markdown.contains("**[00:00:04] You:** Let's start Second thought"));
        assert!(!markdown.contains("**[00:00:06] You:**"));
        assert!(markdown.contains("**[00:00:11] Meeting audio:** First item"));
        assert!(markdown.contains("**[00:00:12] You:** Right"));
        assert!(!markdown.contains("(overlapping)"));

        let persisted = test.store.get_capture(capture.id).unwrap();
        assert_eq!(persisted.markdown_path.as_deref(), Some(path.as_path()));
        assert_eq!(
            persisted.transcript_text.as_deref(),
            Some(
                "[00:00:04] You: Let's start Second thought\n\n[00:00:11] Meeting audio: First item\n\n[00:00:12] You: Right"
            )
        );
        assert_eq!(persisted.word_count, 7);
        assert_eq!(
            test.store
                .update_capture(
                    capture.id,
                    CapturePatch {
                        title: None,
                        transcript_text: Some("inconsistent replacement".into()),
                    },
                )
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
    }

    #[test]
    fn pre_finalization_rename_updates_title_without_touching_staging() {
        let test = TestStore::new("pre-finalization-rename");
        let capture = test.store.create_capture("Original", &test.root).unwrap();
        let staging_path = capture.staging_path.clone();

        let renamed = rename_capture(&test.store, capture.id, "Planning / Review").unwrap();

        assert_eq!(renamed.title, "Planning / Review");
        assert_eq!(renamed.staging_path, staging_path);
        assert!(renamed.markdown_path.is_none());
    }

    #[test]
    fn completed_rename_moves_file_updates_path_and_rewrites_title() {
        let test = TestStore::new("markdown-rename");
        let capture = finalizing_fixture(&test, "Meeting 2024-07-17 10:30");
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let previous_path = completed.markdown_path.clone().unwrap();
        let mut user_edited_markdown = fs::read_to_string(&previous_path).unwrap();
        user_edited_markdown.push_str("## Personal Notes\nKeep this user-authored note.\n");
        fs::write(&previous_path, user_edited_markdown).unwrap();
        let mut expected_record = completed.clone();
        expected_record.title = "Roadmap / Review".into();
        let expected_path = previous_path
            .parent()
            .unwrap()
            .join(markdown::filename_for_capture(&expected_record, 1).unwrap());

        let renamed = rename_capture(&test.store, capture.id, "Roadmap / Review").unwrap();

        assert_eq!(renamed.title, "Roadmap / Review");
        assert_eq!(
            renamed.markdown_path.as_deref(),
            Some(expected_path.as_path())
        );
        assert!(!previous_path.exists());
        assert!(expected_path.exists());
        assert!(expected_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("Roadmap Review - "));
        let markdown = fs::read_to_string(expected_path).unwrap();
        assert!(markdown.contains("title: \"Roadmap / Review\""));
        assert!(markdown.contains("\n# Roadmap / Review\n"));
        assert!(markdown.contains("## Personal Notes\nKeep this user-authored note."));
        assert!(!markdown.contains("Meeting 2024-07-17 10:30"));
    }

    #[test]
    fn completed_rename_preserves_collision_and_uses_numbered_filename() {
        let test = TestStore::new("markdown-rename-collision");
        let capture = finalizing_fixture(&test, "Original");
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let previous_path = completed.markdown_path.clone().unwrap();
        let mut expected_record = completed.clone();
        expected_record.title = "Planning".into();
        let collision_path = previous_path
            .parent()
            .unwrap()
            .join(markdown::filename_for_capture(&expected_record, 1).unwrap());
        fs::write(&collision_path, "unrelated user file").unwrap();

        let renamed = rename_capture(&test.store, capture.id, "Planning").unwrap();
        let renamed_path = renamed.markdown_path.unwrap();

        assert_eq!(
            fs::read_to_string(collision_path).unwrap(),
            "unrelated user file"
        );
        assert!(!previous_path.exists());
        assert!(renamed_path.exists());
        let renamed_filename = renamed_path.file_name().unwrap().to_string_lossy();
        assert!(renamed_filename.starts_with("Planning - "));
        assert!(renamed_filename.ends_with(" (2).md"));
        let markdown = fs::read_to_string(renamed_path).unwrap();
        assert!(markdown.contains("title: \"Planning\""));
        assert!(markdown.contains("\n# Planning\n"));
    }

    #[test]
    fn completed_rename_updates_missing_path_before_regeneration() {
        let test = TestStore::new("markdown-rename-missing");
        let capture = finalizing_fixture(&test, "Original");
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let previous_path = completed.markdown_path.unwrap();
        fs::remove_file(&previous_path).unwrap();

        let renamed = rename_capture(&test.store, capture.id, "Planning").unwrap();
        let renamed_path = renamed.markdown_path.unwrap();

        assert_ne!(renamed_path, previous_path);
        let renamed_filename = renamed_path.file_name().unwrap().to_string_lossy();
        assert!(renamed_filename.starts_with("Planning - "));
        assert!(renamed_filename.ends_with(".md"));
        assert!(!renamed_path.exists());
        assert_eq!(
            regenerate_markdown(&test.store, capture.id, false).unwrap(),
            renamed_path
        );
        let markdown = fs::read_to_string(renamed_path).unwrap();
        assert!(markdown.contains("title: \"Planning\""));
        assert!(markdown.contains("\n# Planning\n"));
    }

    #[test]
    fn completed_rename_applies_case_only_filename_change() {
        let test = TestStore::new("markdown-rename-case-only");
        let capture = finalizing_fixture(&test, "Planning");
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let previous_path = completed.markdown_path.unwrap();

        let renamed = rename_capture(&test.store, capture.id, "planning").unwrap();
        let renamed_path = renamed.markdown_path.unwrap();

        assert_eq!(
            renamed_path.file_name().unwrap().to_string_lossy(),
            previous_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replace("Planning - ", "planning - ")
        );
        assert!(renamed_path.exists());
        let markdown = fs::read_to_string(renamed_path).unwrap();
        assert!(markdown.contains("title: \"planning\""));
        assert!(markdown.contains("\n# planning\n"));
    }

    #[test]
    fn completed_rename_adopts_exact_file_from_interrupted_attempt() {
        let test = TestStore::new("markdown-rename-adopt");
        let capture = finalizing_fixture(&test, "Original");
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let previous_path = completed.markdown_path.clone().unwrap();
        let source = fs::read(&previous_path).unwrap();
        let rewritten = markdown::rewrite_title(&source, "Planning").unwrap();
        let mut expected_record = completed;
        expected_record.title = "Planning".into();
        let expected_path = previous_path
            .parent()
            .unwrap()
            .join(markdown::filename_for_capture(&expected_record, 1).unwrap());
        fs::write(&expected_path, &rewritten).unwrap();

        let renamed = rename_capture(&test.store, capture.id, "Planning").unwrap();

        assert_eq!(
            renamed.markdown_path.as_deref(),
            Some(expected_path.as_path())
        );
        assert!(!previous_path.exists());
        assert_eq!(fs::read(expected_path).unwrap(), rewritten);
    }

    #[test]
    fn collision_preserves_unrelated_file_and_uses_numbered_name() {
        let test = TestStore::new("markdown-collision");
        let capture = finalizing_fixture(&test, "Planning");
        let output = markdown::validate_output_directory(&test.root.join("output")).unwrap();
        let expected = output.join(
            markdown::filename_for_capture(&test.store.get_capture(capture.id).unwrap(), 1)
                .unwrap(),
        );
        fs::write(&expected, "unrelated").unwrap();

        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        assert_eq!(fs::read_to_string(expected).unwrap(), "unrelated");
        assert!(completed
            .markdown_path
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(" (2).md"));
    }

    #[test]
    fn missing_completed_file_regenerates_deterministically() {
        let test = TestStore::new("markdown-regenerate");
        let capture = finalizing_fixture(&test, "Regenerate");
        insert_finalizing_segment(
            &test,
            capture.id,
            CaptureTrack::Microphone,
            1,
            6_500,
            8_000,
            "Second thought",
        );
        insert_finalizing_segment(
            &test,
            capture.id,
            CaptureTrack::Microphone,
            2,
            12_000,
            13_000,
            "Right",
        );
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let path = completed.markdown_path.unwrap();
        let original = fs::read(&path).unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET transcript_text = 'legacy segment rendering' WHERE id = ?1",
                params![capture.id],
            )
            .unwrap();
        }
        fs::remove_file(&path).unwrap();

        assert_eq!(
            regenerate_markdown(&test.store, capture.id, false).unwrap(),
            path
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .transcript_text
                .as_deref(),
            Some(
                "[00:00:04] You: Let's start Second thought\n\n[00:00:11] Meeting audio: First item\n\n[00:00:12] You: Right"
            )
        );

        fs::write(&path, "external edit").unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET transcript_text = 'legacy segment rendering' WHERE id = ?1",
                params![capture.id],
            )
            .unwrap();
        }
        assert_eq!(
            regenerate_markdown(&test.store, capture.id, false)
                .unwrap_err()
                .code,
            CaptureErrorCode::OutputFileExists
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "external edit");
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .transcript_text
                .as_deref(),
            Some("legacy segment rendering")
        );
        regenerate_markdown(&test.store, capture.id, true).unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .transcript_text
                .as_deref(),
            Some(
                "[00:00:04] You: Let's start Second thought\n\n[00:00:11] Meeting audio: First item\n\n[00:00:12] You: Right"
            )
        );
    }

    #[test]
    fn invalid_persisted_output_setting_marks_finalization_failed() {
        let test = TestStore::new("markdown-setting-failure");
        let capture = finalizing_fixture(&test, "Setting failure");
        let documents = test.root.join("Documents");
        let app_data = test.root.join("AppData");
        fs::create_dir_all(&documents).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        fs::write(
            app_data.join("capture-output-directory.json"),
            "not valid JSON",
        )
        .unwrap();

        let error = finalize_markdown_from_settings(&test.store, capture.id, &documents, &app_data)
            .unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::OutputDirectoryUnwritable);
        let failed = test.store.get_capture(capture.id).unwrap();
        assert_eq!(failed.status, CaptureState::FinalizationFailed);
        assert_eq!(
            failed.error_code.as_deref(),
            Some("output_directory_unwritable")
        );

        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET error_code = 'stale_error' WHERE id = ?1",
                params![capture.id],
            )
            .unwrap();
        }
        assert!(
            finalize_markdown_from_settings(&test.store, capture.id, &documents, &app_data,)
                .is_err()
        );
        assert_eq!(
            test.store
                .get_capture(capture.id)
                .unwrap()
                .error_code
                .as_deref(),
            Some("output_directory_unwritable")
        );
    }

    #[test]
    fn filesystem_failure_retains_retryable_capture_state() {
        let test = TestStore::new("markdown-write-failure");
        let capture = finalizing_fixture(&test, "Failure");
        let invalid_output = test.root.join("not-a-directory");
        fs::write(&invalid_output, "file").unwrap();

        let error = finalize_markdown(&test.store, capture.id, &invalid_output).unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::OutputDirectoryUnwritable);
        let failed = test.store.get_capture(capture.id).unwrap();
        assert_eq!(failed.status, CaptureState::FinalizationFailed);
        assert_eq!(
            failed.error_code.as_deref(),
            Some("output_directory_unwritable")
        );
        assert!(failed.markdown_path.is_none());
        assert!(failed.staging_path.is_some());

        fs::remove_file(&invalid_output).unwrap();
        let completed = finalize_markdown(&test.store, capture.id, &invalid_output).unwrap();
        assert_eq!(completed.status, CaptureState::Completed);
        assert!(completed.markdown_path.unwrap().exists());
    }

    #[test]
    fn database_finalization_failure_leaves_reconcilable_complete_file() {
        let test = TestStore::new("markdown-db-failure");
        let capture = finalizing_fixture(&test, "Database failure");
        let output = test.root.join("output");
        let error =
            finalize_markdown_with_commit(&test.store, capture.id, &output, |_path, _rendered| {
                Err(CaptureError::persistence())
            })
            .unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::PersistenceUnavailable);
        let failed = test.store.get_capture(capture.id).unwrap();
        assert_eq!(failed.status, CaptureState::FinalizationFailed);
        assert_eq!(
            failed.error_code.as_deref(),
            Some("persistence_unavailable")
        );
        let files_before_retry = fs::read_dir(&output)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".md"))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(files_before_retry.len(), 1);
        let orphan_path = fs::canonicalize(&files_before_retry[0]).unwrap();
        let orphan_bytes = fs::read(&orphan_path).unwrap();

        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        assert_eq!(
            completed.markdown_path.as_deref(),
            Some(orphan_path.as_path())
        );
        assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
        assert_eq!(
            fs::read_dir(output)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".md"))
                .count(),
            1
        );
    }

    #[test]
    fn finalizing_inputs_are_frozen_and_recovery_lists_finalizing_rows() {
        let test = TestStore::new("markdown-freeze");
        let capture = finalizing_fixture(&test, "Frozen");
        assert_eq!(
            test.store
                .update_capture(
                    capture.id,
                    CapturePatch {
                        title: Some("Changed".into()),
                        transcript_text: None,
                    },
                )
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidStateTransition
        );
        assert!(test
            .store
            .recovery_capture_ids()
            .unwrap()
            .contains(&capture.id));
    }

    #[test]
    fn summary_serialization_omits_transcript_and_staging_paths() {
        let test = TestStore::new("summary-serialization");
        let capture = finalizing_fixture(&test, "Summary");
        let output = test.root.join("output");
        let completed = finalize_markdown(&test.store, capture.id, &output).unwrap();
        let summary = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(completed.id, &test.root)
                .unwrap(),
        );
        let value = serde_json::to_value(&summary).unwrap();
        assert!(value.get("transcript_text").is_none());
        assert!(value.get("staging_path").is_none());
        assert!(value.get("markdown_path").is_none());
        assert_eq!(summary.markdown_status, CaptureMarkdownStatus::Available);
        assert!(summary.has_recovery_audio);

        fs::remove_file(completed.markdown_path.unwrap()).unwrap();
        let missing = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(completed.id, &test.root)
                .unwrap(),
        );
        assert_eq!(missing.markdown_status, CaptureMarkdownStatus::Missing);

        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET markdown_path = ?2 WHERE id = ?1",
                params![completed.id, test.root.to_string_lossy().to_string()],
            )
            .unwrap();
        }
        let unavailable = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(completed.id, &test.root)
                .unwrap(),
        );
        assert_eq!(
            unavailable.markdown_status,
            CaptureMarkdownStatus::Unavailable
        );
    }

    #[test]
    fn inactive_delete_whitelist_excludes_recording_and_processing_states() {
        use CaptureState::*;
        for state in [
            Draft,
            Completed,
            Interrupted,
            CaptureFailed,
            TranscriptionFailed,
            FinalizationFailed,
        ] {
            assert!(user_delete_initiation_allowed(state));
        }
        for state in [
            SelectingSource,
            Starting,
            Recording,
            Pausing,
            Paused,
            Resuming,
            Stopping,
            Transcribing,
            Finalizing,
            Deleting,
            CleanupFailed,
            Cancelled,
        ] {
            assert!(!user_delete_initiation_allowed(state));
        }
    }

    #[test]
    fn summary_search_only_returns_completed_transcripts() {
        let test = TestStore::new("summary-search");
        let completed = finalizing_fixture(&test, "Completed roadmap");
        finalize_markdown(&test.store, completed.id, &test.root.join("output")).unwrap();

        let partial = test
            .store
            .create_capture("Partial roadmap", &test.root)
            .unwrap();
        advance_to_transcribing(&test.store, partial.id);
        test.store
            .update_capture(
                partial.id,
                CapturePatch {
                    title: None,
                    transcript_text: Some("roadmap partial".into()),
                },
            )
            .unwrap();

        let matches = test
            .store
            .search_capture_summaries("roadmap", 25, &test.root)
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, completed.id);
        assert_eq!(matches[0].status, CaptureState::Completed);
    }

    #[test]
    fn summary_progress_uses_durable_chunk_counts_and_zero_totals_are_empty() {
        let test = TestStore::new("summary-progress");
        let empty = test.store.create_capture("Empty", &test.root).unwrap();
        assert!(TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(empty.id, &test.root)
                .unwrap(),
        )
        .processing
        .is_none());

        let capture = test.store.create_capture("Progress", &test.root).unwrap();
        advance_to_recording(&test.store, capture.id);
        test.store
            .create_capture_chunk(&NewCaptureChunk {
                capture_id: capture.id,
                ordinal: 0,
                track: CaptureTrack::System,
                start_ms: 0,
                end_ms: 1000,
                path: test.root.join("system.wav"),
                byte_length: 10,
                status: CaptureChunkState::Ready,
            })
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Stopping, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Transcribing, None, None)
            .unwrap();
        let summary = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(capture.id, &test.root)
                .unwrap(),
        );
        let progress = summary.processing.unwrap();
        assert_eq!(progress.completed_units, 0);
        assert_eq!(progress.total_units, 1);
        assert_eq!(progress.fraction, 0.0);
        assert_eq!(progress.estimated_remaining_ms, None);
    }

    #[test]
    fn successful_audio_retention_persists_deadline_and_size() {
        let test = TestStore::new("audio-retention");
        let capture = finalizing_fixture(&test, "Retained");
        let completed =
            finalize_markdown(&test.store, capture.id, &test.root.join("output")).unwrap();
        let retained = test
            .store
            .set_completed_audio_retention(
                completed.id,
                SuccessfulAudioRetention::After24Hours,
                92_000_000,
                1_000,
            )
            .unwrap();
        assert_eq!(
            retained.audio_retention,
            MeetingAudioRetention::After24Hours
        );
        assert_eq!(retained.audio_delete_at, Some(1_000 + 24 * HOUR_MS));
        assert_eq!(retained.audio_bytes, 92_000_000);
        assert_eq!(
            test.store
                .expired_completed_audio(1_000 + 24 * HOUR_MS - 1)
                .unwrap(),
            Vec::<i64>::new()
        );
        assert_eq!(
            test.store
                .expired_completed_audio(1_000 + 24 * HOUR_MS)
                .unwrap(),
            vec![completed.id]
        );
        let summary = TranscriptStore::summary_from_row(
            test.store
                .get_capture_summary(completed.id, &test.root)
                .unwrap(),
        );
        assert_eq!(summary.audio_delete_at, Some(1_000 + 24 * HOUR_MS));
        assert_eq!(summary.audio_bytes, 92_000_000);
    }

    #[test]
    fn audio_retention_setting_reads_legacy_keep_as_seven_days() {
        let retention: SuccessfulAudioRetention = serde_json::from_str("\"keep\"").unwrap();
        assert_eq!(retention, SuccessfulAudioRetention::After7Days);
        assert_eq!(
            MeetingAudioRetention::parse(LEGACY_KEPT_AUDIO).unwrap(),
            MeetingAudioRetention::After7Days
        );
    }

    #[test]
    fn legacy_kept_audio_expires_seven_days_after_recording_ends() {
        let test = TestStore::new("legacy-kept-audio");
        let capture = finalizing_fixture(&test, "Kept");
        let completed =
            finalize_markdown(&test.store, capture.id, &test.root.join("output")).unwrap();
        test.store
            .set_completed_audio_retention(
                completed.id,
                SuccessfulAudioRetention::After24Hours,
                92_000_000,
                1_000,
            )
            .unwrap();
        let ended_at = 5_000_i64;
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET audio_retention = 'keep', audio_delete_at = NULL,
                     ended_at = ?2 WHERE id = ?1",
                params![completed.id, ended_at],
            )
            .unwrap();
        }
        assert!(test
            .store
            .expired_completed_audio(i64::MAX)
            .unwrap()
            .is_empty());

        assert_eq!(test.store.retire_kept_audio().unwrap(), 1);
        let retired = test.store.get_capture(completed.id).unwrap();
        assert_eq!(retired.audio_retention, MeetingAudioRetention::After7Days);
        assert_eq!(retired.audio_delete_at, Some(ended_at + 7 * 24 * HOUR_MS));
        assert_eq!(
            test.store
                .expired_completed_audio(ended_at + 7 * 24 * HOUR_MS)
                .unwrap(),
            vec![completed.id]
        );
        assert_eq!(test.store.retire_kept_audio().unwrap(), 0);
    }

    #[test]
    fn legacy_kept_failed_audio_returns_to_failure_preserved() {
        let test = TestStore::new("legacy-kept-failed-audio");
        let capture = test
            .store
            .create_capture("Interrupted", &test.root)
            .unwrap();
        advance_to_recording(&test.store, capture.id);
        test.store
            .transition_capture(
                capture.id,
                CaptureState::Interrupted,
                Some(CaptureErrorCode::CaptureInterrupted),
                Some("interrupted".into()),
            )
            .unwrap();
        {
            let conn = test.store.connection().unwrap();
            conn.execute(
                "UPDATE captures SET audio_retention = 'keep' WHERE id = ?1",
                params![capture.id],
            )
            .unwrap();
        }

        assert_eq!(test.store.retire_kept_audio().unwrap(), 1);
        let retired = test.store.get_capture(capture.id).unwrap();
        assert_eq!(
            retired.audio_retention,
            MeetingAudioRetention::FailurePreserved
        );
        assert_eq!(retired.audio_delete_at, None);
        assert!(test
            .store
            .expired_completed_audio(i64::MAX)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn interrupted_audio_has_no_expiry_by_default() {
        let test = TestStore::new("failure-audio-retention");
        let capture = test
            .store
            .create_capture("Interrupted", &test.root)
            .unwrap();
        advance_to_recording(&test.store, capture.id);
        test.store
            .transition_capture(
                capture.id,
                CaptureState::Interrupted,
                Some(CaptureErrorCode::CaptureInterrupted),
                Some("interrupted".into()),
            )
            .unwrap();
        let capture = test.store.get_capture(capture.id).unwrap();
        assert_eq!(
            capture.audio_retention,
            MeetingAudioRetention::FailurePreserved
        );
        assert_eq!(capture.audio_delete_at, None);
        assert!(test
            .store
            .expired_completed_audio(i64::MAX)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn recovery_discovery_marks_stale_rows_and_finds_manifests() {
        let test = TestStore::new("recovery");
        let capture = test
            .store
            .create_capture("Interrupted", &test.root)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::SelectingSource, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Starting, None, None)
            .unwrap();
        test.store
            .transition_capture(capture.id, CaptureState::Recording, None, None)
            .unwrap();

        let staging = test.root.join(capture.id.to_string());
        fs::create_dir_all(&staging).unwrap();
        fs::write(
            staging.join("manifest.json"),
            format!(
                r#"{{"version":1,"capture_id":{},"created_at":1,"active_duration_ms":0,"paused_duration_ms":0,"tracks":["microphone","system"],"chunks":[]}}"#,
                capture.id
            ),
        )
        .unwrap();

        assert_eq!(test.store.mark_stale_captures_interrupted().unwrap(), 1);
        let mut candidates = recovery::discover(&test.root).unwrap();
        let ids = candidates
            .iter()
            .filter_map(|candidate| candidate.capture_id)
            .collect::<Vec<_>>();
        let existing = test.store.existing_capture_ids(&ids).unwrap();
        for candidate in &mut candidates {
            candidate.known_capture = candidate
                .capture_id
                .is_some_and(|id| existing.contains(&id));
        }
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].known_capture);
        assert_eq!(
            candidates[0].status,
            recovery::RecoveryCandidateStatus::Recoverable
        );
        assert_eq!(
            test.store.get_capture(capture.id).unwrap().status,
            CaptureState::Interrupted
        );
    }
}
