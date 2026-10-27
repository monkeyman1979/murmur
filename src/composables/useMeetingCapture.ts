import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { computed, ref } from "vue";
import { i18n } from "@/i18n";

import {
  autoMeetingTitle,
  canStartMeeting,
  captureErrorCopy,
  invokeCapture,
  meetingControls,
  meetingRecordingNoticeSeen,
  normalizeCaptureError,
  normalizeMeetingRecorderStatus,
  normalizeSummary,
  type CaptureError,
  type CaptureSummary,
  type MeetingCancelResult,
  type MeetingCaptureCapabilities,
  type MeetingControlAction,
} from "@/lib/captures";

const activeCapture = ref<CaptureSummary | null>(null);
const activeDurationMs = ref(0);
const pausedDurationMs = ref(0);
const capabilities = ref<MeetingCaptureCapabilities | null>(null);
const loading = ref(false);
const action = ref<MeetingControlAction | "start" | null>(null);
const error = ref<CaptureError | null>(null);
const statusError = ref<CaptureError | null>(null);
const warning = ref<string | null>(null);
const startFlowRequested = ref(false);
const startDialogRequested = ref(false);

let initialized = false;
let statusHydrating = false;
let statusRefreshPending = false;

function eventCaptureId(payload: unknown): number | null {
  if (typeof payload !== "object" || payload === null) return null;
  const id = (payload as { capture_id?: unknown }).capture_id;
  return typeof id === "number" && Number.isSafeInteger(id) && id > 0 ? id : null;
}

function applyStatus(value: unknown): void {
  const status = normalizeMeetingRecorderStatus(value);
  const previousId = activeCapture.value?.id ?? null;
  const nextId = status.capture?.id ?? null;
  activeCapture.value = status.capture;
  activeDurationMs.value = status.active_duration_ms;
  pausedDurationMs.value = status.paused_duration_ms;
  statusError.value = null;
  if (previousId !== nextId || !status.capture) warning.value = null;
  if (previousId !== nextId && status.capture) error.value = null;
}

function syncActiveCapture(summary: CaptureSummary): void {
  if (activeCapture.value?.id === summary.id) activeCapture.value = summary;
}

async function hydrate(): Promise<void> {
  if (statusHydrating) {
    statusRefreshPending = true;
    return;
  }
  statusHydrating = true;
  loading.value = true;
  try {
    do {
      statusRefreshPending = false;
      try {
        applyStatus(await invokeCapture("meeting_recorder_status"));
      } catch (reason) {
        statusError.value = normalizeCaptureError(reason);
      }
    } while (statusRefreshPending);
  } finally {
    statusHydrating = false;
    loading.value = false;
  }
}

async function refreshCapabilities(): Promise<MeetingCaptureCapabilities | null> {
  try {
    const result = await invoke<MeetingCaptureCapabilities>("meeting_capture_capabilities");
    capabilities.value = result;
    // A capability refresh must not erase the failure that opened setup.
    return result;
  } catch (reason) {
    error.value = normalizeCaptureError(reason);
    return null;
  }
}

async function runRecorderAction<T>(
  nextAction: MeetingControlAction | "start",
  task: () => Promise<T>,
): Promise<T | null> {
  // Claim the action before the native command so concurrent controller
  // actions cannot start or resume the same capture twice.
  if (action.value !== null) return null;
  action.value = nextAction;
  error.value = null;
  try {
    return await task();
  } catch (reason) {
    error.value = normalizeCaptureError(reason);
    return null;
  } finally {
    action.value = null;
    await hydrate();
  }
}

async function start(title: string): Promise<CaptureSummary | null> {
  return runRecorderAction("start", async () => {
    warning.value = null;
    statusError.value = null;
    const record = normalizeSummary(await invokeCapture("meeting_recorder_start", {
      title: title.trim() || autoMeetingTitle(),
    }));
    activeCapture.value = record;
    activeDurationMs.value = record.active_duration_ms;
    pausedDurationMs.value = record.paused_duration_ms;
    return record;
  });
}

async function pause(): Promise<CaptureSummary | null> {
  const id = activeCapture.value?.id;
  if (!id) return null;
  return runRecorderAction("pause", async () => {
    const record = normalizeSummary(await invokeCapture("meeting_recorder_pause", { captureId: id }));
    activeCapture.value = record;
    return record;
  });
}

async function resume(): Promise<CaptureSummary | null> {
  const id = activeCapture.value?.id;
  if (!id) return null;
  return runRecorderAction("resume", async () => {
    const record = normalizeSummary(await invokeCapture("meeting_recorder_resume", { captureId: id }));
    activeCapture.value = record;
    return record;
  });
}

async function stop(): Promise<CaptureSummary | null> {
  const id = activeCapture.value?.id;
  if (!id) return null;
  return runRecorderAction("stop", async () => normalizeSummary(
    await invokeCapture("meeting_recorder_stop", { captureId: id }),
  ));
}

async function cancel(confirmed = false): Promise<MeetingCancelResult | null> {
  const id = activeCapture.value?.id;
  if (!id) return null;
  return runRecorderAction("cancel", async () => {
    const result = await invokeCapture<MeetingCancelResult>("meeting_recorder_cancel", {
      captureId: id,
      confirmed,
    });
    if (result.status === "confirmation_required" || result.status === "cleanup_failed") {
      result.capture = normalizeSummary(result.capture);
    }
    return result;
  });
}

function requestStartFlow(): void {
  startFlowRequested.value = true;
}

function consumeStartFlowRequest(): void {
  startFlowRequested.value = false;
}

function requestStartDialog(): void {
  startDialogRequested.value = true;
}

function consumeStartDialogRequest(): void {
  startDialogRequested.value = false;
}

export interface RecordingIssue {
  severity: "warning" | "error";
  label: string;
  detail: string;
}

function warningIssue(value: string): RecordingIssue {
  const [key, severity] = (() => {
    switch (value) {
      case "low_disk_space": return ["lowDisk", "warning"] as const;
      case "two_hours": return ["twoHours", "warning"] as const;
      case "three_hours_forty_five": return ["timeLimit", "warning"] as const;
      case "disk_query_failed": return ["diskQuery", "error"] as const;
      default: return ["unknown", "warning"] as const;
    }
  })();
  return {
    severity,
    label: i18n.global.t(`meetingController.issues.${key}.label`),
    detail: i18n.global.t(`meetingController.issues.${key}.detail`),
  };
}

const recordingIssue = computed<RecordingIssue | null>(() => {
  if (!activeCapture.value) return null;
  if (statusError.value) {
    return {
      severity: "error",
      label: i18n.global.t("meetingController.issues.status.label"),
      detail: i18n.global.t("meetingController.issues.status.detail"),
    };
  }
  if (warning.value) return warningIssue(warning.value);
  if (error.value) {
    return {
      severity: "error",
      label: i18n.global.t("meetingController.issues.recording"),
      detail: captureErrorCopy(error.value, i18n.global.t),
    };
  }
  return null;
});

export type StartFlowOutcome = "started" | "dialog";

/**
 * Phase B fast path. Every start request — the tray item, the `/captures`
 * button, and later the meeting-detected toast — goes through here, and the
 * dialog is shown only when a prerequisite is genuinely missing.
 *
 * Capabilities are re-read from Rust rather than trusted from the cached ref:
 * the microphone grant can change between app launch and the click, and
 * starting a capture that immediately fails is worse than one extra IPC call.
 *
 * A failed `start()` falls back to the dialog on purpose — `error` is already
 * populated by then, and the dialog is where the user can read and act on it.
 * Returning "dialog" here is therefore not only the "needs onboarding" answer;
 * it means "put the full surface in front of the user".
 */
async function resolveStartFlow(): Promise<StartFlowOutcome> {
  const current = await refreshCapabilities();
  if (!meetingRecordingNoticeSeen()) return "dialog";
  const eligible = canStartMeeting(
    current,
    current?.microphone ?? "unknown",
    !!activeCapture.value,
  );
  if (!eligible) return "dialog";
  if (await start(autoMeetingTitle())) return "started";
  return "dialog";
}

function discoverInterruptedCaptures(): void {
  // Spec §7.5: a `recording`/`starting`/`paused` row left behind by a
  // killed/crashed process must become `interrupted` on next launch so
  // Captures can offer Recover & Transcribe. The backend command that
  // does this (`capture_recovery_discover`) previously had no caller
  // anywhere — Rust startup nor frontend — so stale rows just sat as
  // `recording` forever. Fire it once per session, before the first
  // `hydrate()`, so a freshly (re)launched app's Captures list reflects
  // reality immediately rather than only after a manual retry/rename
  // touches the row.
  void invoke("capture_recovery_discover").catch((err) => {
    console.error("[murmur] capture_recovery_discover failed", err);
  });
}

function installListeners(enableMainWindowFlows: boolean): void {
  if (initialized) return;
  initialized = true;

  // Recovery discovery mutates persisted capture state and tray start requests
  // control the main application flow. The floating controller needs neither;
  // keeping them out also lets its Tauri capability omit both authorities.
  if (enableMainWindowFlows) {
    discoverInterruptedCaptures();
    void listen("murmur://meeting-start-requested", () => requestStartFlow());
  }
  void listen("murmur://meeting-capture-state", ({ payload }) => {
    if (eventCaptureId(payload)) void hydrate();
  });
  void listen("murmur://meeting-capture-duration", ({ payload }) => {
    const id = eventCaptureId(payload);
    if (!id || id !== activeCapture.value?.id || typeof payload !== "object" || payload === null) return;
    const value = payload as Record<string, unknown>;
    if (typeof value.active_duration_ms === "number") activeDurationMs.value = Math.max(0, value.active_duration_ms);
    if (typeof value.paused_duration_ms === "number") pausedDurationMs.value = Math.max(0, value.paused_duration_ms);
  });
  void listen("murmur://meeting-capture-warning", ({ payload }) => {
    const id = eventCaptureId(payload);
    if (!id || id !== activeCapture.value?.id || typeof payload !== "object" || payload === null) return;
    const value = (payload as { warning?: unknown }).warning;
    warning.value = typeof value === "string" ? value : null;
  });

  void hydrate();
}

export function useMeetingCapture(options: { enableMainWindowFlows?: boolean } = {}) {
  installListeners(options.enableMainWindowFlows ?? true);
  return {
    activeCapture,
    activeDurationMs,
    pausedDurationMs,
    capabilities,
    loading,
    action,
    error,
    warning,
    recordingIssue,
    startFlowRequested,
    startDialogRequested,
    controls: computed(() => meetingControls(activeCapture.value?.status ?? null)),
    hydrate,
    refreshCapabilities,
    syncActiveCapture,
    start,
    pause,
    resume,
    stop,
    cancel,
    requestStartFlow,
    consumeStartFlowRequest,
    requestStartDialog,
    consumeStartDialogRequest,
    resolveStartFlow,
  };
}
