use std::sync::Arc;

use rand_core::{OsRng, RngCore};
use serde::Serialize;
use tauri::AppHandle;

use crate::captures::{MeetingAudioRetentionState, MeetingTranscriptRetentionState};
use crate::history::HistoryRetentionState;
use crate::model_download::{self, ModelRegistry};
use crate::permissions::{self, PermissionSnapshot};

#[derive(Debug, Serialize)]
pub struct SupportModelStatus {
    id: &'static str,
    installed: bool,
    bytes: u64,
    verification: &'static str,
}

#[derive(Debug, Serialize)]
pub struct SupportRetention {
    dictation_history_days: u32,
    meeting_transcript_days: u32,
    meeting_audio: crate::captures::SuccessfulAudioRetention,
}

#[derive(Debug, Serialize)]
pub struct SupportDiagnostics {
    schema_version: u32,
    report_id: String,
    generated_at_ms: i64,
    murmur_version: String,
    build_channel: &'static str,
    operating_system: &'static str,
    architecture: &'static str,
    features: Vec<&'static str>,
    permissions: PermissionSnapshot,
    models: Vec<SupportModelStatus>,
    retention: SupportRetention,
}

fn report_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn feature_ids() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "meeting-capture") {
        features.push("meeting-capture");
    }
    if cfg!(feature = "meeting-capture-debug") {
        features.push("meeting-capture-debug");
    }
    features
}

/// Generate a fresh, allowlisted support report. This command reads typed
/// state directly; it never parses raw logs or serializes native source errors.
#[tauri::command]
pub fn support_diagnostics(
    app: AppHandle,
    history_retention: tauri::State<'_, Arc<HistoryRetentionState>>,
    meeting_audio_retention: tauri::State<'_, Arc<MeetingAudioRetentionState>>,
    meeting_transcript_retention: tauri::State<'_, Arc<MeetingTranscriptRetentionState>>,
) -> SupportDiagnostics {
    let models = ModelRegistry::ENTRIES
        .iter()
        .map(|entry| {
            let info = model_download::model_info_for(&app, entry).ok();
            SupportModelStatus {
                id: entry.id,
                installed: info.as_ref().is_some_and(|info| info.exists),
                bytes: info.as_ref().map_or(0, |info| info.size),
                // Verification results are not persisted. Do not imply that a
                // successful download-time check describes current bytes.
                verification: "not-run",
            }
        })
        .collect();

    SupportDiagnostics {
        schema_version: 1,
        report_id: report_id(),
        generated_at_ms: crate::history::now_millis(),
        murmur_version: app.package_info().version.to_string(),
        build_channel: if cfg!(debug_assertions) {
            "development"
        } else {
            "production"
        },
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        features: feature_ids(),
        permissions: permissions::current(),
        models,
        retention: SupportRetention {
            dictation_history_days: history_retention.days(),
            meeting_transcript_days: meeting_transcript_retention.days(),
            meeting_audio: meeting_audio_retention.snapshot(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_ids_are_random_fixed_width_hex() {
        let first = report_id();
        let second = report_id();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn feature_ids_never_describe_unenabled_features() {
        let features = feature_ids();
        assert_eq!(
            features.contains(&"meeting-capture"),
            cfg!(feature = "meeting-capture")
        );
        assert_eq!(
            features.contains(&"meeting-capture-debug"),
            cfg!(feature = "meeting-capture-debug")
        );
    }
}
