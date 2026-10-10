import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { computed, ref, type Ref } from "vue";
import { i18n } from "@/i18n";

export interface ModelInfo {
  path: string;
  exists: boolean;
  size: number;
  /** Unix millis of the model file's last-modified time, or `null` when
   *  the file doesn't exist or its mtime can't be read. */
  modified: number | null;
  env_override: boolean;
  download_url: string;
  approx_download_bytes: number;
}

interface DownloadProgress {
  model_id: string;
  downloaded: number;
  total: number | null;
  done: boolean;
}

/** Payload of the `murmur://model-verify` event — parallels
 *  `DownloadProgress` but `total` is known upfront (file size) so the
 *  row can render a determinate bar from the first event. */
interface VerifyProgressEvent {
  model_id: string;
  hashed: number;
  total: number;
  done: boolean;
}

/** Per-row verify-progress snapshot used by `ModelRow`. `hashed` grows
 *  from 0 to `total` across the lifetime of one verify pass and is
 *  cleared on the `done=true` event. */
export interface VerifyProgressSnapshot {
  hashed: number;
  total: number;
}

export type ModelStatus =
  | { kind: "unknown" }
  | { kind: "ready"; info: ModelInfo }
  | { kind: "missing"; info: ModelInfo }
  | { kind: "downloading"; downloaded: number; total: number | null }
  | { kind: "error"; message: string };

// Stable ids mirror src-tauri/src/model_download.rs. Kept here as string
// literals so callsites pass a typo-checked value rather than a raw string.
export const MULTILINGUAL_MODEL_ID = "whisper-small-multilingual";

// One shared reactive status per model id. Re-calling useModelDownload
// with the same id from two components returns the *same* ref, so both
// views see the same download progress without duplicate listeners.
const statuses = new Map<string, Ref<ModelStatus>>();

// One in-flight download promise per model id. Without this guard, two
// surfaces can start the same model at once (for example Home banner +
// Settings → Speech model after a navigation/reload), which makes both Rust
// tasks write to the same `.part` file. Their progress events then race
// (the bar appears to move backward) and the loser can fail at the final
// `.part` → final rename because the winner already moved it.
const downloadPromises = new Map<string, Promise<void>>();
// Manual verification is likewise one native hash pass per model id. Multiple
// views receive the same promise instead of racing duplicate lifecycle work.
const verifyPromises = new Map<string, Promise<VerifyReport>>();

function statusFor(id: string): Ref<ModelStatus> {
  let s = statuses.get(id);
  if (!s) {
    s = ref<ModelStatus>({ kind: "unknown" });
    statuses.set(id, s);
  }
  return s;
}

/** Outcome of a manual verify pass — shape mirrors `VerifyReport` on
 *  the Rust side. `bytes_hashed` is informational; the UI only branches
 *  on `ok`. */
export interface VerifyReport {
  ok: boolean;
  expected: string;
  actual: string;
  bytes_hashed: number;
}

// Transient per-id verify state. Kept separate from `ModelStatus` so a
// verify pass doesn't nuke the underlying ready/missing status while
// running. `failure` persists until the next successful verify or a
// re-download completes so the row can keep surfacing "hash mismatch"
// to the user. `success` is the opposite — a brief confirmation that
// auto-clears so it doesn't pile up stale ✓'s on every row the user
// has ever verified.
const verifyingFlags = new Map<string, Ref<boolean>>();
const verifyFailures = new Map<string, Ref<string | null>>();
const verifySuccesses = new Map<string, Ref<boolean>>();
const verifySuccessTimers = new Map<string, ReturnType<typeof setTimeout>>();
const verifyProgresses = new Map<string, Ref<VerifyProgressSnapshot | null>>();

/** How long the "Verified ✓" subtitle stays visible after a passing
 *  verify before auto-clearing. Long enough to register as feedback
 *  without parking stale success state on the row. */
const VERIFY_SUCCESS_FLASH_MS = 3000;

function verifyingFor(id: string): Ref<boolean> {
  let v = verifyingFlags.get(id);
  if (!v) {
    v = ref(false);
    verifyingFlags.set(id, v);
  }
  return v;
}

function verifyFailureFor(id: string): Ref<string | null> {
  let v = verifyFailures.get(id);
  if (!v) {
    v = ref<string | null>(null);
    verifyFailures.set(id, v);
  }
  return v;
}

function verifySuccessFor(id: string): Ref<boolean> {
  let v = verifySuccesses.get(id);
  if (!v) {
    v = ref(false);
    verifySuccesses.set(id, v);
  }
  return v;
}

function verifyProgressFor(id: string): Ref<VerifyProgressSnapshot | null> {
  let v = verifyProgresses.get(id);
  if (!v) {
    v = ref<VerifyProgressSnapshot | null>(null);
    verifyProgresses.set(id, v);
  }
  return v;
}

function flashVerifySuccess(id: string): void {
  // Re-entrant: if an earlier success flash is still visible, reset
  // its timer so the new pass gets the full window.
  const existing = verifySuccessTimers.get(id);
  if (existing) clearTimeout(existing);
  verifySuccessFor(id).value = true;
  const handle = setTimeout(() => {
    verifySuccessFor(id).value = false;
    verifySuccessTimers.delete(id);
  }, VERIFY_SUCCESS_FLASH_MS);
  verifySuccessTimers.set(id, handle);
}

function clearVerifySuccess(id: string): void {
  const existing = verifySuccessTimers.get(id);
  if (existing) {
    clearTimeout(existing);
    verifySuccessTimers.delete(id);
  }
  verifySuccessFor(id).value = false;
}

let progressUnlistenPromise: Promise<() => void> | null = null;
let verifyProgressUnlistenPromise: Promise<() => void> | null = null;

async function ensureProgressListener(): Promise<void> {
  if (!progressUnlistenPromise) progressUnlistenPromise = listen<DownloadProgress>(
    "murmur://model-download",
    (event) => {
      const { model_id, downloaded, total, done } = event.payload;
      if (done) {
        // Final state is resolved by refresh() at the caller — the event
        // just drives mid-download progress updates.
        return;
      }
      const s = statuses.get(model_id);
      if (!s) return;
      if (s.value.kind === "downloading") {
        s.value = { kind: "downloading", downloaded, total };
      }
    },
  );
  try { await progressUnlistenPromise; }
  catch (error) { progressUnlistenPromise = null; throw error; }
}

async function ensureVerifyProgressListener(): Promise<void> {
  if (!verifyProgressUnlistenPromise) verifyProgressUnlistenPromise = listen<VerifyProgressEvent>(
    "murmur://model-verify",
    (event) => {
      const { model_id, hashed, total, done } = event.payload;
      const progress = verifyProgressFor(model_id);
      if (done) {
        // Caller (`verify`) resolves the final status — the `done`
        // event just releases the subtitle slot so the Verified ✓ /
        // Hash mismatch branches can take over cleanly.
        progress.value = null;
        return;
      }
      progress.value = { hashed, total };
    },
  );
  try { await verifyProgressUnlistenPromise; }
  catch (error) { verifyProgressUnlistenPromise = null; throw error; }
}

async function refresh(id: string): Promise<ModelInfo> {
  try {
    const info = await invoke<ModelInfo>("model_info", { id });
    statusFor(id).value = info.exists
      ? { kind: "ready", info }
      : { kind: "missing", info };
    return info;
  } catch (e) {
    // A failed `model_info` call (unknown id, registry mismatch after a
    // dev-time Rust/JS desync, or any backend error) used to leave the
    // status stuck at `unknown` forever — the Speech model card would hang
    // on "Checking model state…". Surface it as an error state so the
    // UI has something to render.
    console.error("[murmur] model_info failed");
    statusFor(id).value = {
      kind: "error",
      message: i18n.global.t('runtimeErrors.modelStatus'),
    };
    throw e;
  }
}

async function download(
  id: string,
  options: { force?: boolean } = {},
): Promise<void> {
  const existing = downloadPromises.get(id);
  if (existing) return existing;

  const promise = runDownload(id, options).finally(() => {
    if (downloadPromises.get(id) === promise) {
      downloadPromises.delete(id);
    }
  });
  downloadPromises.set(id, promise);
  return promise;
}

async function runDownload(
  id: string,
  options: { force?: boolean } = {},
): Promise<void> {
  try {
    await ensureProgressListener();
    const info = await refresh(id);
    // Early-return when the model is already on disk unless the caller
    // (e.g. a Re-download button) explicitly forces a fresh fetch.
    if (info.exists && !options.force) return;
    statusFor(id).value = {
      kind: "downloading",
      downloaded: 0,
      total: info.approx_download_bytes,
    };
    await invoke<void>("download_model", { id });
    await refresh(id);
    // Download path re-verifies SHA inline; a successful fetch means
    // any previously-surfaced verify failure is resolved.
    verifyFailureFor(id).value = null;
  } catch {
    console.error("[murmur] model download failed");
    const message = i18n.global.t('runtimeErrors.modelDownload');
    statusFor(id).value = { kind: "error", message };
    // Callers must not mistake a sanitized error state for successful
    // completion. Reject with fixed copy, never the opaque native value.
    throw new Error(message);
  }
}

function verify(id: string): Promise<VerifyReport> {
  const existing = verifyPromises.get(id);
  if (existing) return existing;
  const promise = runVerify(id).finally(() => {
    if (verifyPromises.get(id) === promise) verifyPromises.delete(id);
  });
  verifyPromises.set(id, promise);
  return promise;
}

async function runVerify(id: string): Promise<VerifyReport> {
  const flag = verifyingFor(id);
  const failure = verifyFailureFor(id);
  flag.value = true;
  // Any in-flight success flash belongs to a prior pass — clear it so
  // the spinner isn't competing with a stale ✓ for the subtitle slot.
  clearVerifySuccess(id);
  // Reset any leftover progress snapshot from an earlier pass. The
  // backend fires an initial zeroed event before starting the hash,
  // so the bar re-renders cleanly at 0 % rather than briefly showing
  // the last pass's final % on restart.
  verifyProgressFor(id).value = null;
  try {
    await ensureVerifyProgressListener();
    const report = await invoke<VerifyReport>("verify_model", { id });
    if (report.ok) {
      failure.value = null;
      flashVerifySuccess(id);
    } else {
      failure.value = i18n.global.t('runtimeErrors.modelMismatch');
    }
    return report;
  } catch {
    // A failed invoke is distinct from a confirmed hash mismatch, but opaque
    // native details (including possible local paths) must not reach the UI.
    console.error("[murmur] model verification failed");
    const message = i18n.global.t('runtimeErrors.modelVerify');
    failure.value = message;
    throw new Error(message);
  } finally {
    flag.value = false;
    // Safety net: if the backend never emitted a `done` event (crash,
    // early error before the final emit), make sure the progress ref
    // clears so the row doesn't hang showing a stale percentage.
    verifyProgressFor(id).value = null;
  }
}

export function useModelDownload(id: string) {
  void ensureProgressListener().catch(() => console.error("[murmur] model progress listener failed"));
  void ensureVerifyProgressListener().catch(() => console.error("[murmur] verification progress listener failed"));
  const status = statusFor(id);
  const verifying = verifyingFor(id);
  const verifyFailure = verifyFailureFor(id);
  const verifySuccess = verifySuccessFor(id);
  const verifyProgress = verifyProgressFor(id);

  const needsDownload = computed(
    () =>
      status.value.kind === "missing" ||
      status.value.kind === "downloading",
  );

  const progressPercent = computed(() => {
    if (status.value.kind !== "downloading") return 0;
    const total = status.value.total ?? 0;
    if (total <= 0) return 0;
    return Math.min(100, (status.value.downloaded / total) * 100);
  });

  const verifyPercent = computed(() => {
    const p = verifyProgress.value;
    if (!p || p.total <= 0) return 0;
    return Math.min(100, (p.hashed / p.total) * 100);
  });

  return {
    status,
    verifying,
    verifyFailure,
    verifySuccess,
    verifyProgress,
    needsDownload,
    progressPercent,
    verifyPercent,
    refresh: () => refresh(id),
    download: (opts?: { force?: boolean }) => download(id, opts),
    verify: () => verify(id),
  };
}
