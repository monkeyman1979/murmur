import { beforeEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import type { UpdateErrorSource, UpdatePhase } from "@/composables/useUpdater";

const state = vi.hoisted(() => ({
  updater: null as unknown as Record<string, unknown>,
  events: null as unknown as Record<string, unknown>,
  captures: null as unknown as Record<string, unknown>,
}));
vi.mock("@/composables/useUpdater", () => ({ useUpdater: () => state.updater }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => state.events }));
vi.mock("@/composables/useCaptures", () => ({ useCaptures: () => state.captures }));

const phase = ref<UpdatePhase>("idle");
const errorSource = ref<UpdateErrorSource | null>(null);
const isRecording = ref(false);
const updater = {
  enabled: true,
  phase,
  version: ref<string | null>("0.1.3"),
  errorMessage: ref<string | null>(null),
  errorSource,
  checkForUpdates: vi.fn(),
  downloadUpdate: vi.fn(),
  installAndRestart: vi.fn(),
  retryRelaunch: vi.fn(),
  resumeDismissedUpdate: vi.fn(),
};

beforeEach(() => {
  vi.clearAllMocks();
  phase.value = "idle";
  errorSource.value = null;
  isRecording.value = false;
  state.updater = updater;
  state.events = { isRecording, isProcessingDictation: ref(false) };
  state.captures = { inProgress: ref([]), hasActiveMutation: ref(false) };
});

async function actions() {
  const { useUpdateActions } = await import("./useUpdateActions");
  return useUpdateActions();
}

describe("useUpdateActions settings flow", () => {
  it("starts with a manual check", async () => {
    const a = await actions();
    expect(a.settingsStatus.value).toBe("Murmur checks at launch at most once per day. Downloads and installation remain your choice.");
    expect(a.settingsActionLabel.value).toBe("Check for updates");
    a.runSettingsAction();
    expect(updater.checkForUpdates).toHaveBeenCalledWith({ manual: true });
  });

  it("downloads a found update in place", async () => {
    phase.value = "available";
    const a = await actions();
    expect(a.settingsStatus.value).toBe("Murmur 0.1.3 is available.");
    expect(a.settingsActionLabel.value).toBe("Update");
    a.runSettingsAction();
    expect(updater.downloadUpdate).toHaveBeenCalled();
  });

  it("still installs a version dismissed from the sidebar", async () => {
    phase.value = "dismissed";
    const a = await actions();
    expect(a.settingsStatus.value).toBe("Murmur 0.1.3 is available.");
    expect(a.settingsActionLabel.value).toBe("Update");
    a.runSettingsAction();
    expect(updater.resumeDismissedUpdate).toHaveBeenCalled();
    expect(updater.downloadUpdate).toHaveBeenCalled();
  });

  it("shows progress instead of a button while downloading", async () => {
    phase.value = "downloading";
    const a = await actions();
    expect(a.settingsStatus.value).toBe("Downloading update");
    expect(a.settingsActionLabel.value).toBeNull();
  });

  it("holds restart until in-progress work finishes", async () => {
    phase.value = "ready";
    isRecording.value = true;
    const a = await actions();
    expect(a.settingsStatus.value).toBe("Update ready — restart when finished");
    expect(a.settingsActionLabel.value).toBe("Finish current work first");
    expect(a.settingsActionDisabled.value).toBe(true);

    isRecording.value = false;
    expect(a.settingsActionLabel.value).toBe("Restart to update");
    expect(a.settingsActionDisabled.value).toBe(false);
    a.runSettingsAction();
    expect(updater.installAndRestart).toHaveBeenCalledWith(false);
  });

  it("retries a failed check", async () => {
    phase.value = "error";
    errorSource.value = "check";
    const a = await actions();
    expect(a.settingsActionLabel.value).toBe("Retry");
    a.runSettingsAction();
    expect(updater.checkForUpdates).toHaveBeenCalledWith({ manual: true });
  });
});
