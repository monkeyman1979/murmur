import { mount, flushPromises } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import en from "@/locales/en.json";

const mock = vi.hoisted(() => ({ invoke: vi.fn(), download: vi.fn(), refresh: vi.fn() }));
const modelId = "whisper-small-multilingual";
const activeSttId = ref(modelId);
const models = ref([{
  entry: { id: modelId, display_name: "Whisper Small", hf_repo_path: "", filename: "", approx_bytes: 488000000, sha256: null },
  info: { path: "", exists: false, size: 0, modified: null, env_override: false, download_url: "", approx_download_bytes: 488000000 },
}]);
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("@/composables/useActiveModels", () => ({ useActiveModels: () => ({ activeSttId }) }));
vi.mock("@/composables/useModelRegistry", () => ({ useModelRegistry: () => ({ models, refresh: mock.refresh }) }));
vi.mock("@/composables/useModelDownload", () => ({
  useModelDownload: () => ({
    download: mock.download, status: ref({ kind: "missing" }), progressPercent: ref(0),
    verifying: ref(false), verifyFailure: ref(null), verifySuccess: ref(false),
    verifyPercent: ref(0), verifyProgress: ref(null),
  }),
}));
import SpeechModelSettings from "./SpeechModelSettings.vue";

beforeEach(() => {
  vi.clearAllMocks();
  activeSttId.value = modelId;
  mock.download.mockResolvedValue(undefined);
  mock.invoke.mockResolvedValue(undefined);
});
function render() {
  return mount(SpeechModelSettings, {
    global: {
      plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })],
      stubs: {
        ModelRow: { name: "ModelRow", template: "<div />" },
        AlertDialog: { template: "<div />" },
      },
    },
  });
}
describe("speech model install recovery", () => {
  it.each(["download", "redownload"])("reactivates the already-selected model after %s without requiring a restart", async (event) => {
    const wrapper = render();
    wrapper.findComponent({ name: "ModelRow" }).vm.$emit(event);
    await flushPromises();
    expect(mock.download).toHaveBeenCalledWith(...(event === "redownload" ? [{ force: true }] : []));
    expect(mock.invoke).toHaveBeenCalledWith("set_active_stt", { modelId });
    expect(mock.refresh).toHaveBeenCalled();
    wrapper.unmount();
  });

  it("does not activate a model when its download fails", async () => {
    mock.download.mockRejectedValueOnce(new Error("download failed"));
    const wrapper = render();
    wrapper.findComponent({ name: "ModelRow" }).vm.$emit("download");
    await flushPromises();
    expect(mock.invoke).not.toHaveBeenCalled();
    expect(mock.refresh).toHaveBeenCalled();
    wrapper.unmount();
  });

  it("does not replace a different selected model after a background download", async () => {
    activeSttId.value = "other-model";
    const wrapper = render();
    wrapper.findComponent({ name: "ModelRow" }).vm.$emit("download");
    await flushPromises();
    expect(mock.invoke).not.toHaveBeenCalled();
    wrapper.unmount();
  });
});
