import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
import type { AudioInputDevice, InputDevicesState } from "@/composables/useInputDevices";
const native = vi.hoisted(() => ({
  invoke: vi.fn(), listen: vi.fn(), stopListener: vi.fn(), refresh: vi.fn(), syncSelection: vi.fn(),
  permissions: null as unknown as Ref<PermissionSnapshot>, devices: null as unknown as Ref<AudioInputDevice[]>,
  selected: null as unknown as Ref<string | null>, state: null as unknown as Ref<InputDevicesState>,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
vi.mock("@/composables/useInputDevices", () => ({ useInputDevices: () => native }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => native }));
let wrapper: VueWrapper;
beforeEach(() => {
  vi.resetModules(); vi.resetAllMocks(); native.invoke.mockResolvedValue(undefined); native.listen.mockResolvedValue(native.stopListener); native.syncSelection.mockResolvedValue(undefined);
  native.permissions = ref({ microphone: "granted", input_monitoring: "granted", accessibility: "granted" });
  native.devices = ref([{ name: "Built-in", is_default: true }]); native.selected = ref(null); native.state = ref("ready");
});
afterEach(async () => { wrapper?.unmount(); await flushPromises(); });
async function render() {
  const Step = (await import("./StepMicrophone.vue")).default;
  wrapper = mount(Step, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
}
function available() { const events = wrapper.emitted("availability") ?? []; return events[events.length - 1]?.[0]; }

describe("StepMicrophone and useMicrophoneMonitor", () => {
  it.each(["loading", "empty", "error"] as const)("distinguishes %s enumeration and blocks Continue", async (status) => {
    native.state.value = status; native.devices.value = [];
    await render(); await flushPromises();
    expect(available()).toBe(false);
    expect(native.invoke).not.toHaveBeenCalledWith("start_mic_monitor");
    expect(wrapper.text()).toContain(status === "loading" ? "Checking input devices" : status === "error" ? "Could not list input devices" : en.onboarding.microphone.noDevice);
  });
  it("gates on successful monitoring, never on minimum amplitude", async () => {
    await render(); await flushPromises();
    expect(available()).toBe(true);
    expect(wrapper.text()).toContain(en.onboarding.microphone.ready);
    expect(wrapper.text()).not.toContain("0%");
    expect(native.invoke).toHaveBeenCalledWith("start_mic_monitor");
    wrapper.unmount(); await flushPromises();
    expect(native.invoke).toHaveBeenLastCalledWith("stop_mic_monitor");
    expect(native.stopListener).toHaveBeenCalledOnce();
  });
  it("announces monitor status without announcing continuously changing percentages", async () => {
    await render(); await flushPromises();
    await wrapper.get('[aria-controls="microphone-options"]').trigger("click");
    const callback = native.listen.mock.calls[0][1] as (event: { payload: number }) => void;
    callback({ payload: 0.72 });
    await flushPromises();
    const meter = wrapper.find(".tabular-nums");
    expect(meter.text()).toBe("72%");
    expect(meter.element.closest('[aria-live="polite"], [role="status"]')).toBeNull();
    expect(wrapper.findAll('[role="status"]').map((status) => status.text())).toContain(en.onboarding.microphone.meterLive);
  });
  it("offers optional controls on a healthy default without changing selection or blocking Continue", async () => {
    await render(); await flushPromises();
    expect(wrapper.find("#microphone-options").exists()).toBe(false);
    const toggle = wrapper.get('[aria-controls="microphone-options"]');
    expect(toggle.attributes("aria-expanded")).toBe("false");
    expect(native.selected.value).toBeNull();
    await toggle.trigger("click");
    expect(wrapper.find("#microphone-options").exists()).toBe(true);
    expect(toggle.attributes("aria-expanded")).toBe("true");
    await toggle.trigger("click");
    expect(wrapper.find("#microphone-options").exists()).toBe(false);
    expect(available()).toBe(true);
    expect(native.selected.value).toBeNull();
  });
  it("blocks startup failure and succeeds on Retry", async () => {
    let fail = true;
    native.invoke.mockImplementation(async (command) => { if (command === "start_mic_monitor" && fail) throw new Error("input"); });
    await render(); await flushPromises();
    expect(available()).toBe(false);
    expect(wrapper.text()).toContain("Could not start the microphone check");
    fail = false;
    await wrapper.findAll("button").find((button) => button.text() === "Retry")!.trigger("click");
    await flushPromises();
    expect(available()).toBe(true);
  });
  it("reinstalls a failed level listener on Retry", async () => {
    native.listen.mockRejectedValueOnce(new Error("listen"));
    await render(); await flushPromises();
    expect(available()).toBe(false);
    await wrapper.findAll("button").find((button) => button.text() === "Retry")!.trigger("click");
    await flushPromises();
    expect(native.listen).toHaveBeenCalledTimes(2);
    expect(available()).toBe(true);
  });
  it("does not leak a listener resolving after unmount or start a monitor", async () => {
    let finish!: (stop: () => void) => void;
    native.listen.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    await render(); await flushPromises(); wrapper.unmount(); finish(native.stopListener); await flushPromises();
    expect(native.stopListener).toHaveBeenCalledOnce();
    expect(native.invoke).not.toHaveBeenCalledWith("start_mic_monitor");
  });
  it("serializes pending start, device changes, and unmount without granting stale availability", async () => {
    let finish!: () => void;
    native.invoke.mockImplementation((command) => command === "start_mic_monitor" ? new Promise<void>((resolve) => { finish = resolve; }) : Promise.resolve());
    await render(); await flushPromises();
    expect(available()).toBe(false);
    native.selected.value = "External";
    await flushPromises(); wrapper.unmount(); finish(); await flushPromises();
    expect(native.invoke.mock.calls.filter(([command]) => command === "start_mic_monitor")).toHaveLength(1);
    expect(native.invoke).toHaveBeenLastCalledWith("stop_mic_monitor");
  });
  it("restarts on device selection and stops when permission disappears", async () => {
    await render(); await flushPromises();
    native.selected.value = "External"; await flushPromises();
    expect(native.syncSelection).toHaveBeenCalledTimes(2);
    native.permissions.value = { ...native.permissions.value, microphone: "denied" };
    await flushPromises();
    expect(available()).toBe(false);
    expect(native.invoke).toHaveBeenLastCalledWith("stop_mic_monitor");
  });
});
