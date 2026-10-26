import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { ref } from "vue";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import en from "@/locales/en.json";
import { MEETING_RECORDING_NOTICE_KEY } from "@/lib/captures";

const mock = vi.hoisted(() => ({ push: vi.fn(), start: vi.fn(), refresh: vi.fn() }));
const capabilities = ref({
  feature_enabled: true, supported: true, minimum_macos_version: "14.4",
  microphone: "granted" as const, persistence_durable: true,
  stt_model_ready: false, settings_ready: false,
});
vi.mock("vue-router", () => ({ useRouter: () => ({ push: mock.push }) }));
vi.mock("@/composables/useMurmurEvents", () => ({
  useMurmurEvents: () => ({ permissions: ref({ microphone: "granted" }) }),
}));
vi.mock("@/composables/useMeetingCapture", () => ({
  useMeetingCapture: () => ({
    activeCapture: ref(null), capabilities, action: ref(null), error: ref(null),
    refreshCapabilities: mock.refresh, start: mock.start,
  }),
}));
import StartMeetingDialog from "./StartMeetingDialog.vue";

let wrapper: VueWrapper;
beforeEach(() => {
  vi.clearAllMocks();
  localStorage.setItem(MEETING_RECORDING_NOTICE_KEY, "true");
  capabilities.value.stt_model_ready = false;
  capabilities.value.settings_ready = false;
});
afterEach(() => { wrapper?.unmount(); });
function render() {
  wrapper = mount(StartMeetingDialog, {
    props: { open: true },
    global: {
      plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })],
      stubs: Object.fromEntries([
        "Dialog", "DialogContent", "DialogDescription", "DialogFooter",
        "DialogHeader", "DialogTitle",
      ].map((name) => [name, { template: "<div><slot /></div>" }])),
    },
  });
  return wrapper;
}
function button(label: string) { return wrapper.findAll("button").find((b) => b.text() === label)!; }

describe("meeting recording setup feedback", () => {
  it("explains the missing speech model even when permissions are already granted", async () => {
    render();
    expect(wrapper.text()).toContain(en.meetingStart.modelRequired);
    expect(wrapper.text()).toContain(en.meetingStart.modelHelp);
    expect(wrapper.text()).not.toContain(en.meetingStart.microphoneRequired);
    expect(button(en.meetingStart.start).attributes("disabled")).toBeDefined();
    await button(en.meetingStart.start).trigger("click");
    expect(mock.start).not.toHaveBeenCalled();
  });

  it("closes setup and opens Settings at the speech model section without recording", async () => {
    render();
    await button(en.meetingStart.setUpModel).trigger("click");
    expect(wrapper.emitted("update:open")).toEqual([[false]]);
    expect(mock.push).toHaveBeenCalledWith({ name: "settings-general", query: { section: "speech-model" } });
    expect(mock.start).not.toHaveBeenCalled();
  });

  it("distinguishes transient settings hydration from a missing model", async () => {
    capabilities.value.stt_model_ready = true;
    render();
    expect(wrapper.text()).not.toContain(en.meetingStart.modelRequired);
    expect(wrapper.text()).toContain(en.captureErrors.settings_not_hydrated);
    expect(button(en.meetingStart.start).attributes("disabled")).toBeDefined();
    capabilities.value.settings_ready = true;
    await flushPromises();
    expect(button(en.meetingStart.start).attributes("disabled")).toBeUndefined();
    expect(wrapper.text()).not.toContain(en.captureErrors.settings_not_hydrated);
  });

  it("keeps first-recording consent when a model is missing", () => {
    localStorage.removeItem(MEETING_RECORDING_NOTICE_KEY);
    render();
    expect(wrapper.text()).toContain(en.meetingStart.consent);
    expect(wrapper.text()).toContain(en.meetingStart.modelRequired);
  });
});
