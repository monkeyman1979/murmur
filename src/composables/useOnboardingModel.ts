import { invoke } from "@tauri-apps/api/core";
import { computed, ref, watch } from "vue";
import { MULTILINGUAL_MODEL_ID, useModelDownload } from "@/composables/useModelDownload";
import { i18n } from "@/i18n";

export type OnboardingModelState = "checking" | "missing" | "downloading" | "verifying" | "activating" | "operational" | "mismatch" | "error";
type Stage = "checking" | "download" | "verify" | "activate";
const state = ref<OnboardingModelState>("checking");
const error = ref<string | null>(null);
const failedStage = ref<Stage>("checking");
let work: Promise<void> | null = null;
let initialized = false;

// Existence-only ModelStatus remains unchanged for Settings/Home consumers.
export function useOnboardingModel() {
  const model = useModelDownload(MULTILINGUAL_MODEL_ID);
  const operational = computed(() => state.value === "operational");
  const busy = computed(() => ["checking", "downloading", "verifying", "activating"].includes(state.value));

  async function activate(): Promise<void> {
    failedStage.value = "activate";
    state.value = "activating";
    await invoke("set_active_stt", { modelId: MULTILINGUAL_MODEL_ID });
    state.value = "operational";
  }

  async function verify(): Promise<void> {
    failedStage.value = "verify";
    state.value = "verifying";
    const report = await model.verify();
    if (!report.ok) { state.value = "mismatch"; return; }
    await activate();
  }

  function run(action: () => Promise<void>): Promise<void> {
    if (work) return work;
    error.value = null;
    work = action().catch(() => {
      const copy: Record<Stage, string> = {
        checking: i18n.global.t('runtimeErrors.modelStatus'),
        download: i18n.global.t('runtimeErrors.modelDownload'),
        verify: i18n.global.t('runtimeErrors.modelVerify'),
        activate: i18n.global.t('runtimeErrors.modelActivate'),
      };
      error.value = copy[failedStage.value];
      state.value = "error";
    }).finally(() => { work = null; });
    return work;
  }

  function prepare(): Promise<void> {
    if (operational.value) return Promise.resolve();
    return run(async () => {
      failedStage.value = "checking";
      state.value = "checking";
      const info = await model.refresh();
      if (!info.exists) { state.value = "missing"; return; }
      // Explicit developer overrides are unverified, read-only inputs. Native
      // activation preserves that dev-only escape hatch; shipping app-managed
      // bytes always take the registry size/SHA path.
      if (info.env_override) { await activate(); return; }
      await verify();
    });
  }

  function download(): Promise<void> {
    return run(async () => {
      failedStage.value = "download";
      state.value = "downloading";
      // Explicit user action only. Force prevents the existence shortcut from
      // being mistaken for the native downloader's inline checksum proof.
      await model.download({ force: true });
      const status = model.status.value;
      if (status.kind !== "ready") throw new Error(status.kind === "error" ? status.message : i18n.global.t('runtimeErrors.modelIncomplete'));
      await activate();
    });
  }

  function retry(): Promise<void> {
    if (state.value === "missing" || state.value === "mismatch" || failedStage.value === "download") return download();
    if (failedStage.value === "activate") return run(activate);
    if (failedStage.value === "verify") return run(verify);
    return prepare();
  }

  if (!initialized) {
    initialized = true;
    watch(model.status, (next, previous) => {
      if (!operational.value) return;
      if (next.kind !== "ready" || previous.kind !== "ready" || next.info.path !== previous.info.path || next.info.size !== previous.info.size || next.info.modified !== previous.info.modified) {
        state.value = next.kind === "missing" ? "missing" : "checking";
        if (!work) void prepare();
      }
    }, { flush: "sync" });
  }
  return { ...model, state, error, failedStage, operational, busy, prepare, download, retry, invalidate: () => { state.value = "checking"; return prepare(); } };
}
