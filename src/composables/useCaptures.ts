import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { computed, reactive, ref } from "vue";
import { toast } from "vue-sonner";
import { i18n } from "@/i18n";

import {
  buildMeetingAiHandoffPayload,
  readMeetingAiHandoffPrompt,
  validateMeetingAiHandoffPrompt,
} from "@/composables/useMeetingAiHandoff";
import {
  captureErrorCopy,
  captureSection,
  invokeCapture,
  isCaptureProgressing,
  normalizeCaptureError,
  normalizeDetail,
  normalizeSummary,
  type CaptureAction,
  type CaptureDetail,
  type CaptureError,
  type CaptureProcessingSnapshot,
  type CaptureSummary,
} from "@/lib/captures";

const PAGE_SIZE = 25;
const SEARCH_DEBOUNCE_MS = 200;
const PROCESSING_RECONCILE_MS = 15_000;

const baseSummaries = ref<CaptureSummary[]>([]);
const unfinishedSummaries = ref<CaptureSummary[]>([]);
const searchSummaries = ref<CaptureSummary[]>([]);
const query = ref("");
const loading = ref(false);
const refreshing = ref(false);
const loadingMore = ref(false);
const hasMore = ref(true);
const listError = ref<CaptureError | null>(null);
const outputDisclosure = ref<string | null>(null);
const completedCount = ref(0);
const deletableCount = ref(0);
const clearingAll = ref(false);
const clearAllError = ref<CaptureError | null>(null);
const details = reactive(new Map<number, CaptureDetail>());
const runtimeProgress = reactive(new Map<number, CaptureProcessingSnapshot>());
const actionStates = reactive(new Map<string, ActionState>());

export interface ActionState {
  loading: boolean;
  error: CaptureError | null;
  success: string | null;
}

const hasActiveMutation = computed(() => clearingAll.value
  || Array.from(actionStates.values()).some((state) => state.loading));

interface MeetingAiHandoffContext {
  title: string;
  transcript: string;
}

function showToast(key: string, named: Record<string, string | number> = {}): void {
  // Keep action feedback to one concise, neutral line with no status glyph.
  toast.message(i18n.global.t(key, named));
}

let initialized = false;
let searchGeneration = 0;
let refreshRun = 0;
let debounceTimer: number | undefined;

function withProgress(summary: CaptureSummary): CaptureSummary {
  if (!isCaptureProgressing(summary.status)) return summary;
  const progress = runtimeProgress.get(summary.id);
  return progress ? { ...summary, processing: progress } : summary;
}

function reconcileProgress(summary: CaptureSummary): void {
  if (!isCaptureProgressing(summary.status)) {
    runtimeProgress.delete(summary.id);
    return;
  }
  if (summary.processing) runtimeProgress.set(summary.id, summary.processing);
}

const visibleBase = computed(() => baseSummaries.value.map(withProgress));
const visibleSearch = computed(() => searchSummaries.value.map(withProgress));
const visibleUnfinished = computed(() => unfinishedSummaries.value.map(withProgress));
const sectionFor = (row: CaptureSummary) => captureSection(row.status, row.error_code);
const needsAttention = computed(() => visibleUnfinished.value.filter((row) => sectionFor(row) === "attention"));
const inProgress = computed(() => visibleUnfinished.value.filter((row) => sectionFor(row) === "progress"));
const completed = computed(() => query.value.trim()
  ? visibleSearch.value
  : visibleBase.value.filter((row) => {
      const section = sectionFor(row);
      return section !== "attention" && section !== "progress";
    }));
const summaries = computed(() => [...needsAttention.value, ...inProgress.value, ...completed.value]);

function actionKey(id: number, action: CaptureAction): string {
  return `${id}:${action}`;
}

function stateFor(id: number, action: CaptureAction): ActionState {
  const key = actionKey(id, action);
  const existing = actionStates.get(key);
  if (existing) return existing;
  const state = reactive<ActionState>({ loading: false, error: null, success: null });
  actionStates.set(key, state);
  return state;
}

function mergeInto(rows: CaptureSummary[], summary: CaptureSummary): CaptureSummary[] {
  const index = rows.findIndex((row) => row.id === summary.id);
  if (index === -1) return [summary, ...rows];
  const next = [...rows];
  next[index] = summary;
  return next;
}

function mergeSummary(input: unknown): CaptureSummary {
  const summary = normalizeSummary(input);
  baseSummaries.value = mergeInto(baseSummaries.value, summary);
  unfinishedSummaries.value = sectionFor(summary) === "attention" || sectionFor(summary) === "progress"
    ? mergeInto(unfinishedSummaries.value, summary)
    : unfinishedSummaries.value.filter((row) => row.id !== summary.id);
  if (searchSummaries.value.some((row) => row.id === summary.id)) {
    searchSummaries.value = mergeInto(searchSummaries.value, summary);
  }
  reconcileProgress(summary);
  const detail = details.get(summary.id);
  if (detail) details.set(summary.id, { ...detail, summary });
  return summary;
}

function removeSummary(id: number): void {
  baseSummaries.value = baseSummaries.value.filter((summary) => summary.id !== id);
  unfinishedSummaries.value = unfinishedSummaries.value.filter((summary) => summary.id !== id);
  searchSummaries.value = searchSummaries.value.filter((summary) => summary.id !== id);
  details.delete(id);
  runtimeProgress.delete(id);
}

function eventId(payload: unknown): number | null {
  if (typeof payload !== "object" || payload === null) return null;
  const value = (payload as { capture_id?: unknown }).capture_id;
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 ? value : null;
}

async function refreshDetailIfOpen(id: number): Promise<void> {
  if (!details.has(id)) return;
  try {
    details.set(id, normalizeDetail(await invokeCapture("capture_detail_get", { captureId: id })));
  } catch {
    // Collection refresh remains useful if a detail disappeared during a race.
  }
}

async function refreshOne(id: number): Promise<void> {
  try {
    mergeSummary(await invokeCapture("capture_summary_get", { captureId: id }));
    await refreshDetailIfOpen(id);
  } catch (error) {
    const captureError = error as CaptureError;
    if (captureError.code === "capture_not_found") removeSummary(id);
    else listError.value = captureError;
  }
}

interface CaptureStatsWire {
  total_completed: number;
  total_deletable?: number;
}

function applyCaptureStats(stats: CaptureStatsWire): void {
  completedCount.value = Math.max(0, stats.total_completed);
  deletableCount.value = Math.max(0, stats.total_deletable ?? stats.total_completed);
}

async function refreshCaptureStats(): Promise<void> {
  try {
    applyCaptureStats(await invokeCapture<CaptureStatsWire>("capture_stats"));
  } catch (error) {
    console.error("[murmur] capture_stats failed", error);
  }
}

async function refreshCurrentList(): Promise<void> {
  const currentQuery = query.value.trim();
  const generation = ++searchGeneration;
  try {
    const [baseRows, scopeRows, resultRows, stats] = await Promise.all([
      invokeCapture<unknown[]>("capture_summary_list", { limit: PAGE_SIZE, offset: 0 }),
      invokeCapture<unknown[]>("capture_summary_list", { limit: 500, offset: 0 }),
      currentQuery
        ? invokeCapture<unknown[]>("capture_summary_search", { query: currentQuery, limit: PAGE_SIZE })
        : Promise.resolve([]),
      invokeCapture<CaptureStatsWire>("capture_stats"),
    ]);
    if (generation !== searchGeneration) return;
    const normalizedBase = baseRows.map(normalizeSummary);
    const normalizedScope = scopeRows.map(normalizeSummary);
    const normalizedSearch = resultRows.map(normalizeSummary);
    for (const summary of [...normalizedBase, ...normalizedScope, ...normalizedSearch]) {
      reconcileProgress(summary);
    }
    baseSummaries.value = normalizedBase;
    unfinishedSummaries.value = normalizedScope.filter((row) => {
      const section = sectionFor(row);
      return section === "attention" || section === "progress";
    });
    searchSummaries.value = normalizedSearch;
    applyCaptureStats(stats);
    hasMore.value = !currentQuery && baseRows.length === PAGE_SIZE;
    listError.value = null;
  } catch (error) {
    if (generation === searchGeneration) listError.value = error as CaptureError;
  }
}

async function hydrate(): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  await refreshCurrentList();
  loading.value = false;
}

async function refresh(): Promise<void> {
  const run = ++refreshRun;
  refreshing.value = true;
  await refreshCurrentList();
  if (run === refreshRun) refreshing.value = false;
}

async function loadMore(): Promise<void> {
  if (loadingMore.value || !hasMore.value || query.value.trim()) return;
  loadingMore.value = true;
  try {
    const rows = (await invokeCapture<unknown[]>("capture_summary_list", {
      limit: PAGE_SIZE,
      offset: baseSummaries.value.length,
    })).map(normalizeSummary);
    const existing = new Set(baseSummaries.value.map((summary) => summary.id));
    baseSummaries.value = [...baseSummaries.value, ...rows.filter((row) => !existing.has(row.id))];
    hasMore.value = rows.length === PAGE_SIZE;
  } catch (error) {
    listError.value = error as CaptureError;
  } finally {
    loadingMore.value = false;
  }
}

async function setQuery(value: string): Promise<void> {
  query.value = value;
  window.clearTimeout(debounceTimer);
  debounceTimer = window.setTimeout(() => void refresh(), SEARCH_DEBOUNCE_MS);
}

async function getDetail(id: number): Promise<CaptureDetail> {
  const cached = details.get(id);
  if (cached) return cached;
  const detail = normalizeDetail(await invokeCapture("capture_detail_get", { captureId: id }));
  details.set(id, detail);
  mergeSummary(detail.summary);
  return detail;
}

function setRuntimeProgress(id: number, progress: CaptureProcessingSnapshot): void {
  runtimeProgress.set(id, progress);
}

function clearAction(id: number, action: CaptureAction): void {
  const state = stateFor(id, action);
  state.error = null;
  state.success = null;
}

async function runAction<T>(id: number, action: CaptureAction, task: () => Promise<T>, success?: string): Promise<T | null> {
  const state = stateFor(id, action);
  if (state.loading) return null;
  state.loading = true;
  state.error = null;
  state.success = null;
  try {
    const result = await task();
    state.success = success ?? null;
    return result;
  } catch (error) {
    state.error = normalizeCaptureError(error);
    // This code means “ask before replacing the existing file,” not that the
    // restore failed. The view opens the overwrite confirmation dialog.
    if (!(action === "regenerate" && state.error.code === "output_file_exists")) {
      showToast(`captureToasts.errors.${action}`);
    }
    return null;
  } finally {
    state.loading = false;
  }
}

async function mergeMutation(
  id: number,
  action: CaptureAction,
  command: string,
  args: Record<string, unknown>,
  success: string,
  toastTitle: string,
): Promise<CaptureSummary | null> {
  const result = await runAction(id, action, () => invokeCapture(command, args), success);
  if (result === null) return null;
  showToast(toastTitle);
  return mergeSummary(result);
}

async function rename(id: number, title: string): Promise<CaptureSummary | null> {
  const result = await runAction(id, "rename", () => invokeCapture("capture_ui_rename", { captureId: id, title }));
  if (result === null) return null;
  const summary = mergeSummary(result);
  showToast('captureToasts.titleUpdated');
  return summary;
}
const retryTranscription = (id: number) => mergeMutation(id, "retry_transcription", "capture_ui_retry_transcription", { captureId: id }, "Transcription retry started.", 'captureToasts.retryStarted');
const pauseTranscription = (id: number) => mergeMutation(id, "pause_transcription", "capture_ui_pause_transcription", { captureId: id }, "Transcription paused.", 'captureToasts.transcriptionPaused');
const resumeTranscription = (id: number) => mergeMutation(id, "resume_transcription", "capture_ui_resume_transcription", { captureId: id }, "Transcription resumed.", 'captureToasts.transcriptionResumed');
const cancelTranscription = (id: number) => mergeMutation(id, "cancel_transcription", "capture_ui_cancel_transcription", { captureId: id }, "Transcription stopped.", 'captureToasts.transcriptionStopped');
const retryFinalization = (id: number) => mergeMutation(id, "retry_finalization", "capture_ui_retry_finalization", { captureId: id }, "Saving retry started.", 'captureToasts.savingRetry');
const regenerate = (id: number, overwrite = false) => mergeMutation(id, "regenerate", "capture_ui_regenerate_markdown", { captureId: id, overwrite }, "Markdown restored.", 'captureToasts.markdownRestored');
const retryAudioCleanup = (id: number) => mergeMutation(id, "cleanup_audio", "capture_ui_retry_audio_cleanup", { captureId: id }, "Temporary audio cleaned up.", 'captureToasts.audioCleaned');
const keepAudio = (id: number) => mergeMutation(id, "keep_audio", "capture_keep_audio", { captureId: id }, "Audio kept until you delete it.", 'captureToasts.audioKept');
const deleteAudio = (id: number) => mergeMutation(id, "delete_audio", "capture_delete_audio", { captureId: id }, "Retained audio deleted. The transcript remains in Murmur.", 'captureToasts.audioDeleted');

async function chooseFolderAndRetry(id: number): Promise<CaptureSummary | null> {
  clearAction(id, "choose_folder");
  const folder = await open({ directory: true, multiple: false, title: i18n.global.t('captureToasts.folderPicker') });
  // Cancelling a native picker is not an action result and needs no feedback.
  if (!folder || Array.isArray(folder)) return null;
  const result = await runAction(id, "choose_folder", async () => {
    const configured = await invokeCapture<unknown>("set_capture_output_directory", { path: folder });
    if (typeof configured === "object" && configured !== null) {
      const disclosure = (configured as { sync_disclosure?: unknown }).sync_disclosure;
      if (typeof disclosure === "string") outputDisclosure.value = disclosure;
    }
    return invokeCapture("capture_ui_retry_finalization", { captureId: id });
  }, "Saving retry started.");
  if (result === null) return null;
  showToast('captureToasts.folderUpdated');
  return mergeSummary(result);
}

async function saveAudioCopy(summary: CaptureSummary): Promise<boolean> {
  const safeTitle = summary.title.replace(/[\\/:*?"<>|]/g, " ").trim() || `Meeting ${summary.id}`;
  const result = await runAction(summary.id, "save_audio", async () => {
    const destination = await save({
      title: i18n.global.t('captureToasts.audioPicker'),
      defaultPath: `${safeTitle}.m4a`,
      filters: [{ name: i18n.global.t('captureToasts.audioFormat'), extensions: ["m4a"] }],
    });
    if (!destination) return false;
    await invokeCapture("capture_save_audio_copy", {
      captureId: summary.id,
      destination,
    });
    return true;
  });
  const saved = result === true;
  const state = stateFor(summary.id, "save_audio");
  state.success = saved
    ? "Audio copy saved. Murmur's retained source audio is unchanged."
    : null;
  if (saved) showToast('captureToasts.audioSaved');
  return saved;
}

async function deleteCapture(id: number): Promise<boolean> {
  const result = await runAction(id, "delete", () => invokeCapture<boolean>("capture_delete_with_cleanup", { captureId: id }), "Capture deleted.");
  if (result) {
    removeSummary(id);
    showToast('captureToasts.captureDeleted');
    await refreshCaptureStats();
  }
  return result === true;
}

async function clearAllMeetings(): Promise<boolean> {
  if (clearingAll.value) return false;
  clearingAll.value = true;
  clearAllError.value = null;
  try {
    const deleted = await invokeCapture<number>("capture_clear_all_meetings");
    await refreshCurrentList();
    showToast(deleted === 1 ? 'captureToasts.meetingDeleted' : 'captureToasts.meetingsDeleted', { count: deleted });
    return true;
  } catch (error) {
    clearAllError.value = error as CaptureError;
    showToast('captureToasts.meetingHistoryFailed');
    return false;
  } finally {
    clearingAll.value = false;
  }
}

async function retryDeleteCleanup(id: number): Promise<boolean> {
  const result = await runAction(id, "retry_cleanup", () => invokeCapture<boolean>("capture_retry_cleanup", { captureId: id }), "Capture deleted.");
  if (result) {
    removeSummary(id);
    showToast('captureToasts.captureDeleted');
    await refreshCaptureStats();
  }
  return result === true;
}

async function copyTranscript(id: number): Promise<string | null> {
  const result = await runAction(id, "copy_transcript", async () => {
    const transcript = await invokeCapture<string>("capture_read_transcript", { captureId: id });
    await navigator.clipboard.writeText(transcript);
    return transcript;
  }, "Transcript copied.");
  if (result !== null) showToast('captureToasts.transcriptCopied');
  return result;
}

async function copyForAi(id: number): Promise<string | null> {
  const result = await runAction(id, "copy_for_ai", async () => {
    let template: string;
    try {
      template = readMeetingAiHandoffPrompt();
    } catch (error) {
      throw {
        code: "invalid_request",
        message: error instanceof Error ? error.message : "The AI handoff prompt could not be loaded.",
      };
    }
    const validationError = validateMeetingAiHandoffPrompt(template);
    if (validationError) throw { code: "invalid_request", message: validationError };
    const context = await invokeCapture<MeetingAiHandoffContext>(
      "capture_read_ai_handoff_context",
      { captureId: id },
    );
    const payload = buildMeetingAiHandoffPayload(template, context.transcript, context.title);
    await navigator.clipboard.writeText(payload);
    return payload;
  }, "Prompt and transcript copied.");
  if (result !== null) showToast('captureToasts.forAiCopied');
  return result;
}

async function copyPath(id: number): Promise<string | null> {
  const result = await runAction(id, "copy_path", async () => {
    const path = await invokeCapture<string>("capture_get_transcript_path", { captureId: id });
    await navigator.clipboard.writeText(path);
    return path;
  }, "Path copied.");
  if (result !== null) showToast('captureToasts.pathCopied');
  return result;
}

// Opening or revealing a file is its own visible confirmation. Only failures
// need a toast; a second persistent “opened/revealed” message adds noise.
const revealTranscript = (id: number) => runAction(id, "reveal", () => invokeCapture("capture_reveal", { captureId: id }));

function refreshProgressingCaptures(): void {
  if (document.visibilityState === "hidden") return;
  const ids = new Set(
    unfinishedSummaries.value
      .filter((summary) => isCaptureProgressing(summary.status))
      .map((summary) => summary.id),
  );
  for (const id of ids) void refreshOne(id);
}

function installListeners(): void {
  if (initialized) return;
  initialized = true;
  window.addEventListener("focus", refreshProgressingCaptures);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") refreshProgressingCaptures();
  });
  window.setInterval(refreshProgressingCaptures, PROCESSING_RECONCILE_MS);
  const refreshEvent = async (payload: unknown, clearProgress = false) => {
    const id = eventId(payload);
    if (!id) return;
    if (clearProgress) runtimeProgress.delete(id);
    await refreshOne(id);
    if (query.value.trim()) await refresh();
  };
  void listen("murmur://meeting-capture-state", ({ payload }) => refreshEvent(payload));
  void listen("murmur://meeting-capture-duration", ({ payload }) => refreshEvent(payload));
  void listen("murmur://meeting-transcription-progress", ({ payload }) => {
    const id = eventId(payload);
    if (!id || typeof payload !== "object" || payload === null) return;
    const value = payload as Record<string, unknown>;
    if (typeof value.completed_units !== "number" || typeof value.total_units !== "number" || typeof value.fraction !== "number") return;
    setRuntimeProgress(id, {
      completed_units: value.completed_units,
      total_units: value.total_units,
      fraction: value.fraction,
      estimated_remaining_ms: null,
    });
    void refreshOne(id);
  });
  void listen("murmur://meeting-capture-completed", ({ payload }) => {
    void refreshEvent(payload, true);
    void refreshCaptureStats();
  });
  void listen("murmur://meeting-capture-warning", ({ payload }) => refreshEvent(payload));
  void listen("murmur://meeting-history-changed", () => {
    void refresh();
  });
}

function init(): void {
  if (initialized) return;
  installListeners();
  void hydrate();
}

export function useCaptures() {
  init();
  return {
    summaries, needsAttention, inProgress, completed,
    rawSummaries: baseSummaries,
    completedCount, deletableCount, clearingAll, clearAllError,
    query, loading, refreshing, loadingMore, hasMore, listError, outputDisclosure,
    details, actionStates, hasActiveMutation, stateFor, clearAction, getDetail, hydrate, refresh, refreshOne,
    loadMore, setQuery, rename, retryTranscription, retryFinalization, chooseFolderAndRetry,
    regenerate, retryAudioCleanup, keepAudio, deleteAudio, saveAudioCopy, deleteCapture,
    clearAllMeetings, pauseTranscription, resumeTranscription, cancelTranscription,
    retryDeleteCleanup, copyTranscript, copyForAi, copyPath, revealTranscript,
    captureErrorCopy,
  };
}
