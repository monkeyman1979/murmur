import { invoke } from "@tauri-apps/api/core";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { computed, ref } from "vue";
import { i18n } from "@/i18n";

// Release builds check the GitHub release feed (tauri.conf.json). Each update
// is verified against the bundled updater public key before installation.
// Development builds stay quiet so `tauri dev` never offers to replace itself.
export const UPDATES_ENABLED = import.meta.env.PROD || import.meta.env.MODE === "test";
const LAST_CHECK_KEY = "murmur:update-last-check";
const DISMISSED_VERSION_KEY = "murmur:update-dismissed-version";
const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1_000;
const REQUEST_TIMEOUT_MS = 15_000;

export type UpdateErrorSource = "check" | "download" | "install" | "restart";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "up-to-date"
  | "available"
  | "downloading"
  | "ready"
  | "installing"
  | "error"
  | "dismissed";

const phase = ref<UpdatePhase>("idle");
const version = ref<string | null>(null);
const notes = ref<string | null>(null);
const downloadedBytes = ref(0);
const totalBytes = ref<number | null>(null);
const errorMessage = ref<string | null>(null);
const errorSource = ref<UpdateErrorSource | null>(null);
const sidebarVisible = computed(() =>
  ["available", "downloading", "ready", "installing"].includes(phase.value)
  || (phase.value === "error" && errorSource.value !== "check"),
);
let availableUpdate: Update | null = null;
let initialized = false;
let automaticCheckAttempted = false;

function readStorage(key: string): string | null {
  if (typeof localStorage === "undefined") return null;
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string | null): void {
  if (typeof localStorage === "undefined") return;
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    // Preferences remain valid for this session when WebView storage is unavailable.
  }
}

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function closeAvailableUpdate(): Promise<void> {
  const previous = availableUpdate;
  availableUpdate = null;
  if (previous) await previous.close().catch(() => {});
}

async function checkForUpdates(options: { manual?: boolean } = {}): Promise<void> {
  if (!UPDATES_ENABLED) return;
  if (phase.value === "checking" || phase.value === "downloading" || phase.value === "installing") return;
  if (!isTauriRuntime()) {
    if (options.manual) {
      phase.value = "error";
      errorMessage.value = i18n.global.t('runtimeErrors.updateInstalledOnly');
    }
    return;
  }

  await closeAvailableUpdate();
  phase.value = "checking";
  errorMessage.value = null;
  errorSource.value = null;
  downloadedBytes.value = 0;
  totalBytes.value = null;
  // Rate-limit attempts, not only successful responses. An unavailable update
  // service must not trigger a network request on every app relaunch.
  writeStorage(LAST_CHECK_KEY, String(Date.now()));

  try {
    const update = await check({ timeout: REQUEST_TIMEOUT_MS });
    if (!update) {
      version.value = null;
      notes.value = null;
      phase.value = options.manual ? "up-to-date" : "idle";
      return;
    }

    availableUpdate = update;
    version.value = update.version;
    notes.value = update.body?.trim() || null;
    const dismissed = readStorage(DISMISSED_VERSION_KEY) === update.version;
    phase.value = dismissed && !options.manual ? "dismissed" : "available";
  } catch {
    if (options.manual) {
      phase.value = "error";
      errorMessage.value = i18n.global.t('runtimeErrors.updateCheck');
      errorSource.value = "check";
    } else {
      // Background checks are best-effort. Keep routine use quiet when the
      // device is offline or the release service is temporarily unavailable;
      // an explicit Settings check still receives actionable feedback.
      phase.value = "idle";
      errorMessage.value = null;
      errorSource.value = null;
    }
  }
}

async function downloadUpdate(): Promise<void> {
  if (!availableUpdate || phase.value !== "available") return;
  phase.value = "downloading";
  errorMessage.value = null;
  errorSource.value = null;
  downloadedBytes.value = 0;
  totalBytes.value = null;

  try {
    await availableUpdate.download((event: DownloadEvent) => {
      if (event.event === "Started") {
        totalBytes.value = event.data.contentLength ?? null;
      } else if (event.event === "Progress") {
        downloadedBytes.value += event.data.chunkLength;
      }
    }, { timeout: 120_000 });
    phase.value = "ready";
  } catch {
    phase.value = "error";
    errorMessage.value = i18n.global.t('runtimeErrors.updateDownload');
    errorSource.value = "download";
  }
}

async function installAndRestart(blocked: boolean): Promise<void> {
  if (!availableUpdate || phase.value !== "ready" || blocked) return;
  errorMessage.value = null;
  errorSource.value = null;

  let claimed = false;
  let installed = false;
  try {
    claimed = await invoke<boolean>("claim_update_install");
    if (!claimed) {
      errorMessage.value = i18n.global.t('runtimeErrors.updateFinishWork');
      return;
    }

    phase.value = "installing";
    await availableUpdate.install();
    installed = true;
    await relaunch();
    // Keep native admission claimed even if the relaunch request happens to
    // resolve before process exit. Reopening work in that narrow window would
    // permit writes after the application bundle was replaced.
  } catch {
    errorMessage.value = installed
      ? i18n.global.t('runtimeErrors.updateReopen')
      : i18n.global.t('runtimeErrors.updateInstall');
    errorSource.value = installed ? "restart" : "install";
    // A failed install did not modify the application bundle, so ordinary work
    // can safely resume and retry from the already-downloaded update. After a
    // successful install, only process exit/relaunch may clear the claim.
    if (claimed && !installed) {
      await invoke("release_update_install").catch(() => {});
      phase.value = "ready";
    } else {
      phase.value = "error";
    }
  }
}

async function retryRelaunch(): Promise<void> {
  if (phase.value !== "error" || errorSource.value !== "restart") return;
  phase.value = "installing";
  errorMessage.value = null;
  errorSource.value = null;
  try {
    await relaunch();
  } catch {
    phase.value = "error";
    errorMessage.value = i18n.global.t('runtimeErrors.updateReopen');
    errorSource.value = "restart";
  }
}

function dismissUpdate(): void {
  if (!version.value || phase.value !== "available") return;
  writeStorage(DISMISSED_VERSION_KEY, version.value);
  phase.value = "dismissed";
}

function initializeUpdater(): void {
  if (initialized) return;
  initialized = true;
  if (!UPDATES_ENABLED) return;
  if (automaticCheckAttempted || !isTauriRuntime()) return;
  automaticCheckAttempted = true;
  const lastCheck = Number(readStorage(LAST_CHECK_KEY) ?? "0");
  if (Number.isFinite(lastCheck) && Date.now() - lastCheck < CHECK_INTERVAL_MS) return;
  void checkForUpdates();
}

export function useUpdater() {
  initializeUpdater();
  return {
    enabled: UPDATES_ENABLED,
    phase,
    version,
    notes,
    downloadedBytes,
    totalBytes,
    errorMessage,
    errorSource,
    sidebarVisible,
    progress: computed(() => totalBytes.value && totalBytes.value > 0
      ? Math.min(1, downloadedBytes.value / totalBytes.value)
      : null),
    checkForUpdates,
    downloadUpdate,
    installAndRestart,
    retryRelaunch,
    dismissUpdate,
  };
}
