import { describe, expect, it } from "vitest";

import {
  audioRetentionLabel,
  autoMeetingTitle,
  canStartMeeting,
  captureDetailBackOrigin,
  captureErrorCopy,
  captureSection,
  captureStateMessage,
  hasUsableRecoveryAudio,
  hasViewableTranscript,
  isCaptureProgressing,
  primaryCaptureAction,
  meetingControls,
  meetingRecordingNoticeSeen,
  MEETING_RECORDING_NOTICE_KEY,
  MEETING_DELETE_DESCRIPTION,
  normalizeMeetingRecorderStatus,
  normalizeCaptureError,
  normalizeSummary,
  parseCaptureId,
  parseTranscriptBlocks,
  setMeetingRecordingNoticeSeen,
  statePresentation,
} from "@/lib/captures";
import { formatMeetingDuration, formatTranscriptTimestamp } from "@/lib/format";

const states = [
  "draft", "selecting_source", "starting", "recording", "pausing", "paused", "resuming",
  "stopping", "transcribing", "finalizing", "completed", "interrupted", "capture_failed",
  "transcription_failed", "finalization_failed", "deleting", "cleanup_failed", "cancelled",
] as const;

describe("capture presentation contracts", () => {
  it("does not direct users to purchase when encountering an obsolete error code", () => {
    const error = normalizeCaptureError({ code: "activation_required", message: "old build" });
    expect(error.code).toBe("unknown");
    expect(captureErrorCopy(error)).not.toMatch(/license|purchase|activate/i);
  });
  it("maps every persisted state without exposing recorder controls", () => {
    for (const state of states) {
      const presentation = statePresentation(state);
      expect(presentation.label).not.toBe("");
      expect(presentation.actions).not.toContain("start");
      expect(presentation.actions).not.toContain("pause");
      expect(presentation.actions).not.toContain("resume");
      if (["recording", "paused", "transcribing", "finalizing", "deleting"].includes(state)) {
        expect(presentation.actions).not.toContain("delete");
      }
    }
  });

  it("exposes meeting controls only for backend-authoritative active states", () => {
    expect(meetingControls("recording")).toEqual(["pause", "stop", "cancel"]);
    expect(meetingControls("paused")).toEqual(["resume", "stop", "cancel"]);
    expect(meetingControls("pausing")).toEqual([]);
    expect(meetingControls("transcribing")).toEqual([]);
  });

  it("blocks start until permissions and persistence gates pass", () => {
    const capabilities = {
      feature_enabled: true,
      supported: true,
      minimum_macos_version: "14.4",
      microphone: "granted" as const,
      persistence_durable: true,
    };
    expect(canStartMeeting(capabilities, "granted", false)).toBe(true);
    expect(canStartMeeting(capabilities, "denied", false)).toBe(false);
    expect(canStartMeeting(capabilities, "granted", true)).toBe(false);
    expect(canStartMeeting({ ...capabilities, supported: false }, "granted", false)).toBe(false);
    expect(canStartMeeting({ ...capabilities, persistence_durable: false }, "granted", false)).toBe(false);
    expect(canStartMeeting(null, "granted", false)).toBe(false);
  });

  it("normalizes recorder status without inventing an active capture", () => {
    expect(normalizeMeetingRecorderStatus({ capture: null, active_duration_ms: -4 })).toEqual({
      capture: null,
      active_duration_ms: 0,
      paused_duration_ms: 0,
      paused: false,
    });
    const active = normalizeMeetingRecorderStatus({
      capture: { id: 7, status: "paused", title: "Planning" },
      active_duration_ms: 12_000,
      paused_duration_ms: 3_000,
      paused: true,
    });
    expect(active.capture?.status).toBe("paused");
    expect(active.active_duration_ms).toBe(12_000);
  });

  it("offers Copy for AI only when a durable transcript is complete", () => {
    expect(statePresentation("completed", "available").actions).toContain("copy_for_ai");
    expect(statePresentation("completed", "missing").actions).toContain("copy_for_ai");
    expect(statePresentation("transcribing").actions).not.toContain("copy_for_ai");
    expect(statePresentation("transcription_failed").actions).not.toContain("copy_for_ai");
  });

  it("offers Markdown regeneration only when the generated file is missing", () => {
    expect(statePresentation("completed", "missing").actions).toContain("regenerate");
    expect(statePresentation("completed", "available").actions).not.toContain("regenerate");
    expect(statePresentation("completed", "available").label).toBe("Ready");
    expect(statePresentation("completed", "unavailable").actions).not.toContain("regenerate");
  });

  it("normalizes and describes retained audio without inventing content", () => {
    const retained = normalizeSummary({
      id: 9,
      status: "completed",
      has_recovery_audio: true,
      audio_retention: "after_24_hours",
      audio_delete_at: 3_600_000,
      audio_bytes: 92 * 1024 * 1024,
    });
    expect(retained.audio_retention).toBe("after_24_hours");
    expect(audioRetentionLabel(retained, 0)).toBe("Audio retained for recovery · deletes in 1 hour · 92 MB");
    expect(statePresentation("completed", "available", true).actions).toEqual(expect.arrayContaining(["save_audio", "delete_audio"]));
    expect(statePresentation("completed", "available", true).actions).not.toContain("keep_audio");
  });

  it("does not advertise transcript or audio actions for unusable failures", () => {
    const unusable = normalizeSummary({
      id: 16,
      status: "transcription_failed",
      has_recovery_audio: true,
      can_retry_transcription: false,
      retry_unavailable_reason: "audio_unavailable",
    });
    expect(hasViewableTranscript(unusable)).toBe(false);
    expect(hasUsableRecoveryAudio(unusable)).toBe(false);
    expect(audioRetentionLabel(unusable)).toBeNull();

    const olderMeeting = normalizeSummary({
      ...unusable,
      id: 17,
      retry_unavailable_reason: "processing_profile_missing",
    });
    expect(hasUsableRecoveryAudio(olderMeeting)).toBe(true);
    expect(audioRetentionLabel(olderMeeting)).toContain("Audio retained");
    expect(hasViewableTranscript(normalizeSummary({ id: 18, status: "completed" }))).toBe(true);
    expect(hasViewableTranscript(normalizeSummary({ id: 19, status: "finalization_failed" }))).toBe(true);
  });

  it("labels completed processing with no words separately from a real failure", () => {
    const quiet = normalizeSummary({
      id: 23,
      status: "transcription_failed",
      error_code: "no_speech_recognized",
      has_recovery_audio: true,
      can_retry_transcription: true,
    });
    expect(captureSection(quiet.status, quiet.error_code)).toBe("other");
    expect(captureSection("transcription_failed")).toBe("attention");
    expect(statePresentation(quiet.status, quiet.markdown_status, true, quiet.error_code)).toMatchObject({ label: "No speech recognized", tone: "neutral" });
    expect(statePresentation(quiet.status, quiet.markdown_status, true, quiet.error_code, true).actions).toContain("retry_transcription");
    expect(primaryCaptureAction(quiet)).toBeNull();
    expect(captureStateMessage(quiet)).toContain("No words were recognized");
    expect(normalizeCaptureError({ code: "no_speech_recognized" }).code).toBe("no_speech_recognized");
    expect(captureErrorCopy(normalizeCaptureError({ code: "recording_preparation_timed_out" }))).toContain("previous meeting is safe");
  });

  it("warns that deletion is irreversible without implying exported files are removed", () => {
    expect(MEETING_DELETE_DESCRIPTION).toContain("This can't be undone");
    expect(MEETING_DELETE_DESCRIPTION).toContain("Markdown and other saved copies remain on your Mac");
  });

  it("uses one honest state classifier for library and detail", () => {
    expect(isCaptureProgressing("transcribing")).toBe(true);
    expect(statePresentation("transcribing").actions).toEqual(["pause_transcription", "cancel_transcription"]);
    expect(primaryCaptureAction(normalizeSummary({ id: 8, status: "transcribing" }))).toBe("pause_transcription");
    const paused = normalizeSummary({ id: 9, status: "transcribing", transcription_paused: true });
    expect(statePresentation("transcribing", "not_applicable", false, null, false, true).label).toBe("Transcription paused");
    expect(primaryCaptureAction(paused)).toBe("resume_transcription");
    expect(statePresentation("transcribing", "not_applicable", false, null, false, false, true).label).toBe("Queued during recording");
    expect(statePresentation("transcribing", "not_applicable", false, null, false, false, false, true).label).toBe("Queued for transcription");
    expect(isCaptureProgressing("transcription_failed")).toBe(false);
    expect(captureSection("interrupted")).toBe("attention");
    expect(captureSection("recording")).toBe("progress");
    expect(captureSection("completed")).toBe("completed");
    const failed = normalizeSummary({ id: 10, status: "transcription_failed", has_recovery_audio: true, can_retry_transcription: true });
    expect(primaryCaptureAction(failed)).toBe("retry_transcription");
    expect(captureStateMessage(failed)).toBe("Transcription did not finish. Your audio is saved on this Mac, so you can retry transcription.");

    const stopped = normalizeSummary({ id: 12, status: "transcription_failed", error_code: "transcription_cancelled", has_recovery_audio: true, can_retry_transcription: true });
    expect(statePresentation("transcription_failed", "not_applicable", true, "transcription_cancelled").label).toBe("Transcription cancelled");
    expect(captureStateMessage(stopped)).toBe("Transcription was cancelled. Your audio is saved on this Mac, so you can retry transcription.");

    const audioDeleted = normalizeSummary({ id: 11, status: "transcription_failed", has_recovery_audio: false });
    expect(primaryCaptureAction(audioDeleted)).toBeNull();
    expect(primaryCaptureAction(normalizeSummary({ id: 13, status: "completed", markdown_status: "available" }))).toBe("copy_for_ai");
    expect(primaryCaptureAction(normalizeSummary({ id: 14, status: "completed", markdown_status: "missing" }))).toBe("copy_for_ai");
    expect(statePresentation("transcription_failed", "not_applicable", false).actions).not.toContain("retry_transcription");
    expect(captureStateMessage(audioDeleted)).toBe("Transcription did not finish. No usable audio is available, so this meeting can only be deleted.");

    const olderMeeting = normalizeSummary({
      id: 15,
      status: "transcription_failed",
      has_recovery_audio: true,
      can_retry_transcription: false,
      retry_unavailable_reason: "processing_profile_missing",
    });
    expect(primaryCaptureAction(olderMeeting)).toBeNull();
    expect(captureStateMessage(olderMeeting)).toBe(
      "Transcription did not finish. Your audio is saved on this Mac, but the settings needed to retry this older meeting aren't available.",
    );
  });

  it("maps every recoverable failure to plain local-retry copy and a transcription action", () => {
    const cases = [
      {
        status: "interrupted" as const,
        errorCode: null,
        action: "retry_transcription",
        message: "Recording did not finish. Your audio is saved on this Mac, so you can retry transcription.",
        unavailableMessage: "Recording did not finish. No usable audio is available, so this meeting can only be deleted.",
      },
      {
        status: "capture_failed" as const,
        errorCode: null,
        action: "retry_transcription",
        message: "Recording did not finish. Your audio so far is saved on this Mac, so you can retry transcription.",
        unavailableMessage: "Recording did not finish. No usable audio is available, so this meeting can only be deleted.",
      },
      {
        status: "transcription_failed" as const,
        errorCode: null,
        action: "retry_transcription",
        message: "Transcription did not finish. Your audio is saved on this Mac, so you can retry transcription.",
        unavailableMessage: "Transcription did not finish. No usable audio is available, so this meeting can only be deleted.",
      },
      {
        status: "transcription_failed" as const,
        errorCode: "transcription_cancelled",
        action: "retry_transcription",
        message: "Transcription was cancelled. Your audio is saved on this Mac, so you can retry transcription.",
        unavailableMessage: "Transcription was cancelled. No usable audio is available, so this meeting can only be deleted.",
      },
    ];

    for (const { status, errorCode, action, message, unavailableMessage } of cases) {
      const recoverable = normalizeSummary({
        id: 20,
        status,
        error_code: errorCode,
        has_recovery_audio: true,
        can_retry_transcription: true,
      });
      expect(primaryCaptureAction(recoverable)).toBe(action);
      expect(statePresentation(status, "not_applicable", true, errorCode, true).actions).toContain("retry_transcription");
      expect(captureStateMessage(recoverable)).toBe(message);
      expect(captureStateMessage(recoverable)).not.toMatch(/preserv|retain/i);

      // A staging directory may still exist for inspection/export without
      // containing audio that can truthfully be retried.
      const unavailable = normalizeSummary({
        id: 21,
        status,
        error_code: errorCode,
        has_recovery_audio: true,
        can_retry_transcription: false,
      });
      expect(primaryCaptureAction(unavailable)).toBeNull();
      expect(statePresentation(status, "not_applicable", true, errorCode, false).actions).not.toContain("retry_transcription");
      expect(captureStateMessage(unavailable)).toBe(unavailableMessage);
    }
  });

  it("degrades unknown wire values safely", () => {
    expect(normalizeSummary({ id: 4, status: "future_state" }).status).toBe("unknown");
    expect(statePresentation("unknown").label).toBe("Unknown state");
  });

  it("formats short, minute, and four-hour durations", () => {
    expect(formatMeetingDuration(45_000)).toBe("00:45");
    expect(formatMeetingDuration(65_000)).toBe("01:05");
    expect(formatMeetingDuration(4 * 60 * 60 * 1000)).toBe("04:00:00");
  });

  it("compacts visual transcript timestamps without dropping meaningful hours", () => {
    expect(formatTranscriptTimestamp("00:00:00")).toBe("0:00");
    expect(formatTranscriptTimestamp("00:01:09")).toBe("1:09");
    expect(formatTranscriptTimestamp("00:53:14")).toBe("53:14");
    expect(formatTranscriptTimestamp("01:02:03")).toBe("1:02:03");
    expect(formatTranscriptTimestamp("12:34:56")).toBe("12:34:56");
    expect(formatTranscriptTimestamp("unknown")).toBe("unknown");
  });

  it("rejects invalid detail IDs before invoke", () => {
    expect(parseCaptureId("0")).toBeNull();
    expect(parseCaptureId("-2")).toBeNull();
    expect(parseCaptureId("not-an-id")).toBeNull();
    expect(parseCaptureId("42")).toBe(42);
  });

  it("recognizes the list location behind a meeting detail route", () => {
    expect(captureDetailBackOrigin("/")).toBe("home");
    expect(captureDetailBackOrigin("/?source=recent")).toBe("home");
    expect(captureDetailBackOrigin("/captures")).toBe("meetings");
    expect(captureDetailBackOrigin("/captures/?query=planning")).toBe("meetings");
    expect(captureDetailBackOrigin("/captures/42")).toBeNull();
    expect(captureDetailBackOrigin("/history")).toBeNull();
    expect(captureDetailBackOrigin(null)).toBeNull();
  });

  it("separates transcript metadata from spoken text without losing punctuation", () => {
    expect(parseTranscriptBlocks(
      "[00:00:04] You: Let's begin: the first item.\n\n[00:00:09] Meeting audio: Sounds good.",
    )).toEqual([
      { timestamp: "00:00:04", speaker: "You", text: "Let's begin: the first item.", overlapping: false },
      { timestamp: "00:00:09", speaker: "Meeting audio", text: "Sounds good.", overlapping: false },
    ]);
  });

  it("marks only the timed cross-track interjection as overlapping", () => {
    expect(parseTranscriptBlocks(
      "[00:12:04] Meeting audio: The longer passage continues.\n\n> [00:12:06] You (overlapping): So, I mean.\n\n[00:12:08] You: Here is the continuation.",
    )).toEqual([
      { timestamp: "00:12:04", speaker: "Meeting audio", text: "The longer passage continues.", overlapping: false },
      { timestamp: "00:12:06", speaker: "You", text: "So, I mean.", overlapping: true },
      { timestamp: "00:12:08", speaker: "You", text: "Here is the continuation.", overlapping: false },
    ]);
  });

  it("preserves an unfamiliar transcript format as one readable block", () => {
    expect(parseTranscriptBlocks("A future transcript format")).toEqual([
      { timestamp: "", speaker: "", text: "A future transcript format", overlapping: false },
    ]);
    expect(parseTranscriptBlocks(null)).toEqual([]);
  });

  it("maps typed backend errors to actionable copy", () => {
    expect(captureErrorCopy({ code: "output_file_exists", message: "path" })).toContain("edits");
    expect(captureErrorCopy({ code: "audio_format_changed", message: "" })).toContain("recovery audio");
    expect(captureErrorCopy({ code: "chunk_invalid", message: "" })).toContain("audio timing or chunk data");
    expect(captureErrorCopy({ code: "audio_unavailable", message: "" })).toBe("No recoverable audio is available.");
    expect(captureErrorCopy({ code: "feature_disabled", message: "" })).toContain("unavailable in this build");
  });
});

describe("auto meeting title", () => {
  it("formats local wall-clock time as Meeting YYYY-MM-DD HH:mm", () => {
    // Constructed from local components on purpose: the title is what the user
    // reads in the library, so it must track their clock, not UTC.
    expect(autoMeetingTitle(new Date(2026, 7, 2, 15, 4))).toBe("Meeting 2026-08-02 15:04");
  });

  it("zero-pads every component so titles sort lexicographically", () => {
    expect(autoMeetingTitle(new Date(2026, 0, 9, 9, 7))).toBe("Meeting 2026-01-09 09:07");
    expect(autoMeetingTitle(new Date(2026, 11, 31, 23, 59))).toBe("Meeting 2026-12-31 23:59");
    expect(autoMeetingTitle(new Date(2026, 0, 1, 0, 0))).toBe("Meeting 2026-01-01 00:00");
  });

  it("never produces the old placeholder", () => {
    expect(autoMeetingTitle(new Date(2026, 7, 2, 15, 4))).not.toBe("Untitled Meeting");
  });

  it("stays within the title input's 160-character limit", () => {
    expect(autoMeetingTitle(new Date(2026, 7, 2, 15, 4)).length).toBeLessThanOrEqual(160);
  });
});

describe("meeting recording notice persistence", () => {
  it("round-trips through the shared key the fast path reads", () => {
    localStorage.removeItem(MEETING_RECORDING_NOTICE_KEY);
    localStorage.removeItem("murmur:meeting-consent-acknowledged");
    expect(meetingRecordingNoticeSeen()).toBe(false);
    setMeetingRecordingNoticeSeen(true);
    expect(localStorage.getItem(MEETING_RECORDING_NOTICE_KEY)).toBe("true");
    expect(meetingRecordingNoticeSeen()).toBe(true);
    setMeetingRecordingNoticeSeen(false);
    expect(meetingRecordingNoticeSeen()).toBe(false);
  });

  it("migrates the legacy consent acknowledgement without repeating onboarding", () => {
    localStorage.removeItem(MEETING_RECORDING_NOTICE_KEY);
    localStorage.setItem("murmur:meeting-consent-acknowledged", "true");
    expect(meetingRecordingNoticeSeen()).toBe(true);
    expect(localStorage.getItem(MEETING_RECORDING_NOTICE_KEY)).toBe("true");
    expect(localStorage.getItem("murmur:meeting-consent-acknowledged")).toBeNull();
  });
});
