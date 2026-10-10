import { invoke } from "@tauri-apps/api/core";
import { formatBytes } from "@/lib/format";
import { timeUnitKey } from "@/i18n/plurals";

export const CAPTURE_STATES = [
  "draft",
  "selecting_source",
  "starting",
  "recording",
  "pausing",
  "paused",
  "resuming",
  "stopping",
  "transcribing",
  "finalizing",
  "completed",
  "interrupted",
  "capture_failed",
  "transcription_failed",
  "finalization_failed",
  "deleting",
  "cleanup_failed",
  "cancelled",
] as const;

export type CaptureState = (typeof CAPTURE_STATES)[number] | "unknown";
export type CaptureMarkdownStatus =
  | "not_applicable"
  | "available"
  | "missing"
  | "unavailable";

export interface CaptureProcessingSnapshot {
  completed_units: number;
  total_units: number;
  fraction: number;
  estimated_remaining_ms: number | null;
}

export type MeetingAudioRetention =
  | "failure_preserved"
  | "delete_immediately"
  | "after_24_hours"
  | "after_7_days"
  | "keep";

export interface CaptureSummary {
  id: number;
  status: CaptureState;
  title: string;
  created_at: number;
  started_at: number | null;
  ended_at: number | null;
  active_duration_ms: number;
  paused_duration_ms: number;
  source_name: string | null;
  language: string | null;
  word_count: number;
  error_code: string | null;
  error_message: string | null;
  markdown_status: CaptureMarkdownStatus;
  processing: CaptureProcessingSnapshot | null;
  transcription_paused?: boolean;
  transcription_queued?: boolean;
  has_recovery_audio: boolean;
  can_retry_transcription: boolean;
  retry_unavailable_reason: string | null;
  audio_retention: MeetingAudioRetention | null;
  audio_delete_at: number | null;
  audio_bytes: number;
  audio_kept: boolean;
}

export interface CaptureDetail {
  summary: CaptureSummary;
  source_kind: string | null;
  source_bundle: string | null;
  transcript_text: string | null;
  markdown_path: string | null;
}

export interface TranscriptBlock {
  timestamp: string;
  speaker: string;
  text: string;
  overlapping: boolean;
}

/**
 * Split the backend's deterministic transcript rendering into presentational
 * blocks. Durable segment timing remains backend-owned; this only lets the
 * reader give metadata and spoken text separate visual hierarchy. If a future
 * or malformed format arrives, preserve all text in one unstyled block rather
 * than dropping transcript content.
 */
export function parseTranscriptBlocks(value: string | null): TranscriptBlock[] {
  const transcript = value?.trim();
  if (!transcript) return [];

  const entries = transcript.split(/\n{2,}/);
  const blocks: TranscriptBlock[] = [];
  for (const entry of entries) {
    const match = entry.match(/^(>\s*)?\[([^\]]+)]\s+([^:\n]+):\s+([\s\S]+)$/);
    if (!match) return [{ timestamp: "", speaker: "", text: transcript, overlapping: false }];
    const overlapping = Boolean(match[1]);
    const rawSpeaker = match[3].trim();
    const overlapSuffix = " (overlapping)";
    blocks.push({
      timestamp: match[2].trim(),
      speaker: overlapping && rawSpeaker.endsWith(overlapSuffix)
        ? rawSpeaker.slice(0, -overlapSuffix.length)
        : rawSpeaker,
      text: match[4].trim(),
      overlapping,
    });
  }
  return blocks;
}

export interface MeetingCaptureCapabilities {
  feature_enabled: boolean;
  supported: boolean;
  minimum_macos_version: string;
  microphone: "granted" | "denied" | "not-determined" | "unknown";
  persistence_durable: boolean;
}

export interface MeetingRecorderStatus {
  capture: CaptureSummary | null;
  active_duration_ms: number;
  paused_duration_ms: number;
  paused: boolean;
}

export type MeetingControlAction = "pause" | "resume" | "stop" | "cancel";

export type MeetingCancelResult =
  | { status: "confirmation_required"; capture: CaptureSummary }
  | { status: "cancelled"; capture_id: number }
  | { status: "cleanup_failed"; capture: CaptureSummary };

export type CaptureAction =
  | "rename"
  | "delete"
  | "retry_transcription"
  | "pause_transcription"
  | "resume_transcription"
  | "cancel_transcription"
  | "retry_finalization"
  | "choose_folder"
  | "retry_cleanup"
  | "cleanup_audio"
  | "save_audio"
  | "keep_audio"
  | "delete_audio"
  | "regenerate"
  | "copy_transcript"
  | "copy_for_ai"
  | "copy_path"
  | "reveal";

// Deleting a meeting does not remove the generated Markdown or exported copies.
export const MEETING_DELETE_DESCRIPTION = "This removes the meeting and any retained audio from Murmur. Markdown and other saved copies remain on your Mac. This can't be undone.";

/** Shared action allowlist keeps meeting controls consistent across app surfaces. */
export const CAPTURE_ACTIONS: CaptureAction[] = [
  "rename", "delete", "retry_transcription", "pause_transcription", "resume_transcription", "cancel_transcription",
  "retry_finalization", "choose_folder", "retry_cleanup", "cleanup_audio",
  "save_audio", "keep_audio", "delete_audio", "regenerate", "copy_transcript",
  "copy_for_ai", "copy_path", "reveal",
];

export interface StatePresentation {
  label: string;
  tone: "neutral" | "info" | "warning" | "success" | "danger";
  actions: CaptureAction[];
}

export type CaptureErrorCode =
  | "feature_disabled"
  | "capture_not_found"
  | "capture_conflict"
  | "invalid_request"
  | "invalid_state_transition"
  | "invalid_persisted_state"
  | "recovery_discovery_failed"
  | "cleanup_required"
  | "cleanup_failed"
  | "transcript_unavailable"
  | "transcript_file_missing"
  | "output_file_exists"
  | "output_directory_unwritable"
  | "stt_model_missing"
  | "persistence_unavailable"
  | "unsupported_macos"
  | "screen_permission_denied"
  | "microphone_permission_denied"
  | "permission_requires_relaunch"
  | "source_selection_cancelled"
  | "source_unavailable"
  | "capture_busy"
  | "recording_preparation_timed_out"
  | "insufficient_disk_space"
  | "system_audio_start_failed"
  | "microphone_start_failed"
  | "writer_queue_overflow"
  | "chunk_write_failed"
  | "source_ended"
  | "microphone_disconnected"
  | "capture_interrupted"
  | "audio_format_changed"
  | "chunk_invalid"
  | "settings_not_hydrated"
  | "processing_profile_missing"
  | "processing_profile_invalid"
  | "transcription_failed"
  | "no_speech_recognized"
  | "transcription_cancelled"
  | "markdown_write_failed"
  | "audio_unavailable"
  | "audio_export_failed"
  | "unknown";

export interface CaptureError {
  code: CaptureErrorCode;
  message: string;
}

export function isPositiveCaptureId(value: number): boolean {
  return Number.isSafeInteger(value) && value > 0;
}

export function parseCaptureId(value: string | string[] | undefined): number | null {
  if (typeof value !== "string" || !/^\d+$/.test(value)) return null;
  const id = Number(value);
  return isPositiveCaptureId(id) ? id : null;
}

export type CaptureDetailBackOrigin = "home" | "meetings";

export function captureDetailBackOrigin(value: unknown): CaptureDetailBackOrigin | null {
  if (typeof value !== "string") return null;
  const path = value.split(/[?#]/, 1)[0];
  if (path === "/" || path === "") return "home";
  if (path === "/captures" || path === "/captures/") return "meetings";
  return null;
}

function isCaptureState(value: unknown): value is CaptureState {
  return typeof value === "string" && CAPTURE_STATES.includes(value as (typeof CAPTURE_STATES)[number]);
}

function markdownStatus(value: unknown): CaptureMarkdownStatus {
  return value === "available" || value === "missing" || value === "unavailable"
    ? value
    : "not_applicable";
}

function nullableNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function audioRetention(value: unknown): MeetingAudioRetention | null {
  return value === "failure_preserved" || value === "delete_immediately"
    || value === "after_24_hours" || value === "after_7_days" || value === "keep"
    ? value
    : null;
}

export function normalizeSummary(value: unknown): CaptureSummary {
  const input = typeof value === "object" && value !== null ? value as Record<string, unknown> : {};
  const processingInput = typeof input.processing === "object" && input.processing !== null
    ? input.processing as Record<string, unknown>
    : null;
  const processing = processingInput && typeof processingInput.completed_units === "number"
    && typeof processingInput.total_units === "number"
    && typeof processingInput.fraction === "number"
    ? {
        completed_units: Math.max(0, processingInput.completed_units),
        total_units: Math.max(0, processingInput.total_units),
        fraction: Math.min(1, Math.max(0, processingInput.fraction)),
        estimated_remaining_ms: nullableNumber(processingInput.estimated_remaining_ms),
      }
    : null;
  return {
    id: typeof input.id === "number" ? input.id : 0,
    status: isCaptureState(input.status) ? input.status : "unknown",
    title: typeof input.title === "string" ? input.title : "Untitled meeting",
    created_at: typeof input.created_at === "number" ? input.created_at : 0,
    started_at: nullableNumber(input.started_at),
    ended_at: nullableNumber(input.ended_at),
    active_duration_ms: typeof input.active_duration_ms === "number" ? input.active_duration_ms : 0,
    paused_duration_ms: typeof input.paused_duration_ms === "number" ? input.paused_duration_ms : 0,
    source_name: typeof input.source_name === "string" ? input.source_name : null,
    language: typeof input.language === "string" ? input.language : null,
    word_count: typeof input.word_count === "number" ? input.word_count : 0,
    error_code: typeof input.error_code === "string" ? input.error_code : null,
    error_message: typeof input.error_message === "string" ? input.error_message : null,
    markdown_status: markdownStatus(input.markdown_status),
    processing,
    transcription_paused: input.transcription_paused === true,
    transcription_queued: input.transcription_queued === true,
    has_recovery_audio: input.has_recovery_audio === true,
    can_retry_transcription: input.can_retry_transcription === true,
    retry_unavailable_reason: typeof input.retry_unavailable_reason === "string"
      ? input.retry_unavailable_reason
      : null,
    audio_retention: audioRetention(input.audio_retention),
    audio_delete_at: nullableNumber(input.audio_delete_at),
    audio_bytes: typeof input.audio_bytes === "number" ? Math.max(0, input.audio_bytes) : 0,
    audio_kept: input.audio_kept === true,
  };
}

export function normalizeDetail(value: unknown): CaptureDetail {
  const input = typeof value === "object" && value !== null ? value as Record<string, unknown> : {};
  return {
    summary: normalizeSummary(input.summary),
    source_kind: typeof input.source_kind === "string" ? input.source_kind : null,
    source_bundle: typeof input.source_bundle === "string" ? input.source_bundle : null,
    transcript_text: typeof input.transcript_text === "string" ? input.transcript_text : null,
    markdown_path: typeof input.markdown_path === "string" ? input.markdown_path : null,
  };
}

export function normalizeMeetingRecorderStatus(value: unknown): MeetingRecorderStatus {
  const input = typeof value === "object" && value !== null ? value as Record<string, unknown> : {};
  return {
    capture: input.capture == null ? null : normalizeSummary(input.capture),
    active_duration_ms: typeof input.active_duration_ms === "number" ? Math.max(0, input.active_duration_ms) : 0,
    paused_duration_ms: typeof input.paused_duration_ms === "number" ? Math.max(0, input.paused_duration_ms) : 0,
    paused: input.paused === true,
  };
}

export function meetingControls(status: CaptureState | null): MeetingControlAction[] {
  if (status === "recording") return ["pause", "stop", "cancel"];
  if (status === "paused") return ["resume", "stop", "cancel"];
  return [];
}

// Screen Recording is deliberately absent: meeting capture uses Core Audio
// process taps, which need only the microphone grant plus the audio-capture
// grant macOS requests on first use. There is no relaunch dance either — that
// was a ScreenCaptureKit constraint.
export function canStartMeeting(
  capabilities: MeetingCaptureCapabilities | null,
  microphone: MeetingCaptureCapabilities["microphone"],
  hasActiveCapture: boolean,
): boolean {
  return !!capabilities
    && capabilities.feature_enabled
    && capabilities.supported
    && capabilities.persistence_durable
    && microphone === "granted"
    && !hasActiveCapture;
}

/**
 * The recording notice is shown before the first successful meeting capture.
 * After that, eligible starts can use the one-click fast path. The old consent
 * key is migrated so existing users are not shown onboarding a second time.
 */
export const MEETING_RECORDING_NOTICE_KEY = "murmur:meeting-recording-notice-seen";
const LEGACY_MEETING_CONSENT_KEY = "murmur:meeting-consent-acknowledged";

export function meetingRecordingNoticeSeen(): boolean {
  if (typeof localStorage === "undefined") return false;
  if (localStorage.getItem(MEETING_RECORDING_NOTICE_KEY) === "true") return true;
  if (localStorage.getItem(LEGACY_MEETING_CONSENT_KEY) !== "true") return false;

  localStorage.setItem(MEETING_RECORDING_NOTICE_KEY, "true");
  localStorage.removeItem(LEGACY_MEETING_CONSENT_KEY);
  return true;
}

export function setMeetingRecordingNoticeSeen(seen: boolean): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(MEETING_RECORDING_NOTICE_KEY, seen ? "true" : "false");
  localStorage.removeItem(LEGACY_MEETING_CONSENT_KEY);
}

function pad2(value: number): string {
  return value.toString().padStart(2, "0");
}

/**
 * `Meeting YYYY-MM-DD HH:mm`, in the user's local time — the default title for
 * every capture, replacing "Untitled Meeting". Local rather than UTC because
 * this is what the user reads in the library; the Markdown filename prefix is
 * separately derived from `created_at` in Rust.
 *
 * A future title change can overwrite this cleanly because rename already
 * exists; no migration is required.
 */
export function autoMeetingTitle(now: Date = new Date()): string {
  const date = `${now.getFullYear()}-${pad2(now.getMonth() + 1)}-${pad2(now.getDate())}`;
  const time = `${pad2(now.getHours())}:${pad2(now.getMinutes())}`;
  return `Meeting ${date} ${time}`;
}

export function normalizeCaptureError(error: unknown): CaptureError {
  const input = typeof error === "object" && error !== null ? error as Record<string, unknown> : {};
  const rawCode = typeof input.code === "string" ? input.code : "unknown";
  const code = rawCode as CaptureErrorCode;
  return {
    code: [
      "feature_disabled", "capture_not_found", "capture_conflict", "invalid_request",
      "invalid_state_transition", "invalid_persisted_state", "recovery_discovery_failed",
      "cleanup_required", "cleanup_failed", "transcript_unavailable", "transcript_file_missing",
      "output_file_exists", "output_directory_unwritable", "stt_model_missing",
      "persistence_unavailable", "unsupported_macos", "screen_permission_denied",
      "microphone_permission_denied", "permission_requires_relaunch",
      "source_selection_cancelled", "source_unavailable", "capture_busy", "recording_preparation_timed_out",
      "insufficient_disk_space", "system_audio_start_failed", "microphone_start_failed",
      "writer_queue_overflow", "chunk_write_failed", "source_ended", "microphone_disconnected",
      "capture_interrupted", "audio_format_changed", "chunk_invalid", "settings_not_hydrated", "processing_profile_missing",
      "processing_profile_invalid", "transcription_failed", "no_speech_recognized", "transcription_cancelled", "markdown_write_failed",
      "audio_unavailable", "audio_export_failed",
    ].includes(code) ? code : "unknown",
    message: typeof input.message === "string" ? input.message : "The capture action could not be completed.",
  };
}

export function captureErrorCopy(error: CaptureError, translate?: (key: string) => string): string {
  if (translate) return translate(`captureErrors.${error.code}`);
  const copy: Record<CaptureErrorCode, string> = {
    feature_disabled: "Meeting capture is unavailable in this build. Your data has not disappeared.",
    capture_not_found: "This capture no longer exists. Refresh Meetings to update the list.",
    capture_conflict: "This capture conflicts with an existing durable record. Refresh and try again.",
    invalid_request: "That capture request is not valid.",
    invalid_state_transition: "The capture changed before this action could finish. Refresh and try again.",
    invalid_persisted_state: "This capture has an invalid saved state and was not modified.",
    recovery_discovery_failed: "Murmur could not inspect meeting recovery data safely.",
    cleanup_required: "Temporary recovery data must be cleaned up before this capture can be removed.",
    cleanup_failed: "Temporary recovery data could not be removed. Retry cleanup or keep the capture for recovery.",
    transcript_unavailable: "The durable transcript is not available for this capture.",
    transcript_file_missing: "The Markdown file is missing. The durable transcript remains available in Murmur.",
    output_file_exists: "The existing Markdown file may contain edits. Confirm replacement before overwriting it.",
    output_directory_unwritable: "That folder cannot be used for Markdown output. Choose another folder.",
    stt_model_missing: "The local speech model is missing. Restore it in Settings → Speech model, then retry.",
    persistence_unavailable: "Murmur cannot safely access its durable capture database right now.",
    unsupported_macos: "Meeting transcripts require macOS 15 or newer.",
    screen_permission_denied: "Screen & System Audio Recording access is required. No video is saved.",
    microphone_permission_denied: "Microphone access is required to record your side of the meeting.",
    permission_requires_relaunch: "Permission changed. Relaunch Murmur before starting the recording.",
    source_selection_cancelled: "Source selection was cancelled. No audio recording started; the setup record remains in Meetings.",
    source_unavailable: "The selected meeting source is no longer available.",
    capture_busy: "Another dictation or meeting capture is using audio right now.",
    recording_preparation_timed_out: "Transcription did not pause in time. No new recording started. Try again; your previous meeting is safe.",
    insufficient_disk_space: "There is not enough free disk space to start this meeting capture.",
    system_audio_start_failed: "Selected system audio could not start. No complete meeting recording is active.",
    microphone_start_failed: "The microphone track could not start. No complete meeting recording is active.",
    writer_queue_overflow: "Meeting audio could not be written quickly enough. Recoverable audio was preserved.",
    chunk_write_failed: "Meeting audio could not be saved. Recoverable audio was preserved.",
    source_ended: "The selected meeting source ended. Transcribe the audio captured so far from Meetings.",
    microphone_disconnected: "The meeting microphone disconnected. Check Meetings for the preserved recording.",
    capture_interrupted: "Meeting recording was interrupted. Check Meetings for recovery options.",
    audio_format_changed: "The microphone or output route changed its audio format. Murmur stopped before transcription and kept the recovery audio.",
    chunk_invalid: "Murmur received invalid audio timing or chunk data. It stopped safely and kept the staging data for inspection.",
    settings_not_hydrated: "Murmur is still loading local meeting settings. Wait a moment and try again.",
    processing_profile_missing: "This capture has no saved processing profile and cannot be retried safely.",
    processing_profile_invalid: "This capture's saved processing profile is invalid.",
    transcription_failed: "Transcription failed. The captured audio remains available to retry.",
    no_speech_recognized: "No speech was recognized in the recording. Saved audio remains available to retry if you expected words.",
    transcription_cancelled: "Transcription cancelled. The captured audio and completed progress remain available to retry.",
    markdown_write_failed: "The Markdown transcript could not be saved. Choose a folder and retry.",
    audio_unavailable: "No recoverable audio is available.",
    audio_export_failed: "Murmur could not save the audio copy. The retained source audio is unchanged.",
    unknown: "The capture action failed. Refresh and try again.",
  };
  return copy[error.code];
}

export function statePresentation(
  status: CaptureState,
  markdown: CaptureMarkdownStatus = "not_applicable",
  hasRecoveryAudio = false,
  errorCode: string | null = null,
  canRetryTranscription = false,
  transcriptionPaused = false,
  recordingActive = false,
  transcriptionQueued = false,
): StatePresentation {
  switch (status) {
    case "draft": return { label: "Preparing", tone: "neutral", actions: ["delete"] };
    case "selecting_source":
    case "starting": return { label: "Preparing", tone: "neutral", actions: [] };
    case "recording": return { label: "Recording", tone: "info", actions: [] };
    case "pausing": return { label: "Pausing", tone: "info", actions: [] };
    case "paused": return { label: "Paused", tone: "info", actions: [] };
    case "resuming": return { label: "Resuming", tone: "info", actions: [] };
    case "stopping": return { label: "Finishing recording", tone: "info", actions: [] };
    // Deletion remains unavailable while Whisper may be reading the WAVs.
    // Cooperative cancellation waits for the worker to release those files,
    // then returns the capture to a retryable transcription-failed state.
    case "transcribing": return {
      label: transcriptionPaused ? "Transcription paused" : recordingActive ? "Queued during recording" : transcriptionQueued ? "Queued for transcription" : "Transcribing audio",
      tone: "info",
      actions: [transcriptionPaused ? "resume_transcription" : "pause_transcription", "cancel_transcription"],
    };
    case "finalizing": return { label: "Saving transcript", tone: "info", actions: [] };
    case "completed": return {
      label: markdown === "available" ? "Ready" : markdown === "missing" ? "Missing file" : "File unavailable",
      tone: markdown === "available" ? "success" : "warning",
      actions: [
        "copy_transcript",
        "copy_for_ai",
        ...(markdown === "available" || markdown === "missing" ? ["copy_path" as const] : []),
        ...(markdown === "available" ? ["reveal" as const] : []),
        ...(markdown === "missing" ? ["regenerate" as const] : []),
        ...(hasRecoveryAudio ? ["save_audio" as const, "keep_audio" as const, "delete_audio" as const] : []),
        "delete",
      ],
    };
    case "interrupted": return { label: "Interrupted", tone: "warning", actions: [...(canRetryTranscription ? ["retry_transcription" as const] : []), ...(hasRecoveryAudio ? ["save_audio" as const, "delete_audio" as const] : []), "delete"] };
    case "capture_failed": return { label: "Recording failed", tone: "danger", actions: [...(canRetryTranscription ? ["retry_transcription" as const] : []), ...(hasRecoveryAudio ? ["save_audio" as const, "delete_audio" as const] : []), "delete"] };
    case "transcription_failed": return {
      label: errorCode === "no_speech_recognized" ? "No speech recognized" : errorCode === "transcription_cancelled" ? "Transcription cancelled" : "Transcription failed",
      tone: errorCode === "no_speech_recognized" ? "neutral" : errorCode === "transcription_cancelled" ? "warning" : "danger",
      actions: [
        ...(canRetryTranscription ? ["retry_transcription" as const] : []),
        ...(hasRecoveryAudio ? ["save_audio" as const, "delete_audio" as const] : []),
        "delete",
      ],
    };
    case "finalization_failed": return { label: "Saving failed", tone: "danger", actions: ["retry_finalization", "choose_folder", ...(hasRecoveryAudio ? ["save_audio" as const, "delete_audio" as const] : []), "delete"] };
    case "deleting": return { label: "Deleting", tone: "neutral", actions: [] };
    case "cleanup_failed": return { label: "Cleanup failed", tone: "danger", actions: ["retry_cleanup"] };
    case "cancelled": return { label: "Cancelled", tone: "neutral", actions: [] };
    case "unknown": return { label: "Unknown state", tone: "warning", actions: [] };
  }
}

const PROGRESSING_STATES = new Set<CaptureState>(["stopping", "transcribing", "finalizing"]);
const ATTENTION_STATES = new Set<CaptureState>([
  "interrupted", "capture_failed", "transcription_failed", "finalization_failed", "cleanup_failed",
]);
const ACTIVE_STATES = new Set<CaptureState>([
  "draft", "selecting_source", "starting", "recording", "pausing", "paused", "resuming",
  "stopping", "transcribing", "finalizing", "deleting",
]);

export function isCaptureProgressing(status: CaptureState): boolean {
  return PROGRESSING_STATES.has(status);
}

export function captureSection(status: CaptureState, errorCode: string | null = null): "attention" | "progress" | "completed" | "other" {
  if (status === "transcription_failed" && errorCode === "no_speech_recognized") return "other";
  if (ATTENTION_STATES.has(status)) return "attention";
  if (ACTIVE_STATES.has(status)) return "progress";
  if (status === "completed") return "completed";
  return "other";
}

export function primaryCaptureAction(summary: CaptureSummary): CaptureAction | null {
  if (summary.status === "transcribing") return summary.transcription_paused ? "resume_transcription" : "pause_transcription";
  if (summary.status === "transcription_failed" && summary.error_code === "no_speech_recognized") return null;
  if ((summary.status === "interrupted" || summary.status === "capture_failed" || summary.status === "transcription_failed") && summary.can_retry_transcription) return "retry_transcription";
  if (summary.status === "finalization_failed") return "retry_finalization";
  if (summary.status === "cleanup_failed") return "retry_cleanup";
  if (summary.status === "completed") return "copy_for_ai";
  return null;
}

export function captureStateMessage(summary: CaptureSummary, translate?: (key: string, named?: Record<string, string | number>) => string): string | null {
  if (translate) {
    const retryReason = translate(`captureMessages.${summary.retry_unavailable_reason === 'processing_profile_missing' ? 'retryMissing' : summary.retry_unavailable_reason === 'processing_profile_invalid' ? 'retryInvalid' : 'retryNoAudio'}`);
    if (summary.status === 'interrupted') return summary.can_retry_transcription
      ? translate('captureMessages.interruptedRetry') : translate('captureMessages.interruptedUnavailable', { reason: retryReason });
    if (summary.status === 'capture_failed') return summary.can_retry_transcription
      ? translate('captureMessages.failedRetry') : translate('captureMessages.interruptedUnavailable', { reason: retryReason });
    if (summary.status === 'transcription_failed') {
      const key = summary.error_code === 'no_speech_recognized'
        ? (summary.can_retry_transcription ? 'noSpeechRetry' : 'noSpeechDelete')
        : summary.error_code === 'transcription_cancelled'
          ? (summary.can_retry_transcription ? 'stoppedRetry' : 'stoppedUnavailable')
          : (summary.can_retry_transcription ? 'transcriptionRetry' : 'transcriptionUnavailable');
      return translate(`captureMessages.${key}`, { reason: retryReason });
    }
    if (summary.status === 'finalization_failed') return translate('captureMessages.finalizationFailed');
    if (summary.status === 'cleanup_failed') return translate('captureMessages.cleanupFailed');
    if (summary.status === 'completed' && summary.markdown_status === 'missing') return translate('captureMessages.markdownMissing');
    return null;
  }
  const retryUnavailable = summary.retry_unavailable_reason === "processing_profile_missing"
    ? "Your audio is saved on this Mac, but the settings needed to retry this older meeting aren't available."
    : summary.retry_unavailable_reason === "processing_profile_invalid"
      ? "Your audio is saved on this Mac, but its transcription settings are invalid."
      : "No usable audio is available, so this meeting can only be deleted.";
  if (summary.status === "interrupted") return summary.can_retry_transcription
    ? "Recording did not finish. Your audio is saved on this Mac, so you can retry transcription."
    : `Recording did not finish. ${retryUnavailable}`;
  if (summary.status === "capture_failed") return summary.can_retry_transcription
    ? "Recording did not finish. Your audio so far is saved on this Mac, so you can retry transcription."
    : `Recording did not finish. ${retryUnavailable}`;
  if (summary.status === "transcription_failed" && summary.error_code === "no_speech_recognized") return summary.can_retry_transcription
    ? "No words were recognized. If you expected speech, you can retry the saved audio from the menu."
    : "No words were recognized. You can delete this meeting if you don't need it.";
  if (summary.status === "transcription_failed" && summary.error_code === "transcription_cancelled") return summary.can_retry_transcription
    ? "Transcription was cancelled. Your audio is saved on this Mac, so you can retry transcription."
    : `Transcription was cancelled. ${retryUnavailable}`;
  if (summary.status === "transcription_failed") return summary.can_retry_transcription
    ? "Transcription did not finish. Your audio is saved on this Mac, so you can retry transcription."
    : `Transcription did not finish. ${retryUnavailable}`;
  if (summary.status === "finalization_failed") return "The transcript is safe in Murmur, but its Markdown file couldn't be saved.";
  if (summary.status === "cleanup_failed") return "Temporary data could not be removed. The capture remains available.";
  if (summary.status === "completed" && summary.markdown_status === "missing") return "The transcript is safe in Murmur; its Markdown copy is missing.";
  return null;
}

export function hasUsableRecoveryAudio(summary: CaptureSummary): boolean {
  return summary.has_recovery_audio && summary.retry_unavailable_reason !== "audio_unavailable";
}

export function hasViewableTranscript(summary: CaptureSummary): boolean {
  return summary.status === "completed" || summary.status === "finalization_failed";
}

export function audioRetentionLabel(summary: CaptureSummary, now = Date.now(), translate?: (key: string, named?: Record<string, string | number>) => string, locale = 'en'): string | null {
  if (!hasUsableRecoveryAudio(summary)) return null;
  const size = summary.audio_bytes > 0 ? ` · ${formatBytes(summary.audio_bytes)}` : "";
  if (translate) {
    if (summary.audio_kept || summary.audio_retention === 'keep') return translate('captureAudio.kept', { size });
    if (summary.audio_retention === 'failure_preserved') return translate('captureAudio.preserved', { size });
    if (summary.audio_delete_at) {
      const remaining = summary.audio_delete_at - now;
      if (remaining <= 0) return translate('captureAudio.cleanupPending', { size });
      const hours = Math.max(1, Math.ceil(remaining / 3_600_000));
      const duration = hours < 48
        ? translate('captureAudio.hours', { count: hours, unit: translate(timeUnitKey('hour', hours, locale)) })
        : translate('captureAudio.days', { count: Math.ceil(hours / 24), unit: translate(timeUnitKey('day', Math.ceil(hours / 24), locale)) });
      return translate('captureAudio.recoveryCountdown', { duration, size });
    }
    return translate('captureAudio.retained', { size });
  }
  if (summary.audio_kept || summary.audio_retention === "keep") return `Audio kept${size}`;
  if (summary.audio_retention === "failure_preserved") return `Audio preserved${size}`;
  if (summary.audio_delete_at) {
    const remaining = summary.audio_delete_at - now;
    if (remaining <= 0) return `Audio cleanup pending${size}`;
    const hours = Math.max(1, Math.ceil(remaining / 3_600_000));
    const countdown = hours < 48 ? `${hours} ${hours === 1 ? "hour" : "hours"}` : `${Math.ceil(hours / 24)} days`;
    return `Audio retained for recovery · deletes in ${countdown}${size}`;
  }
  return `Audio retained${size}`;
}

export async function invokeCapture<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw normalizeCaptureError(error);
  }
}
