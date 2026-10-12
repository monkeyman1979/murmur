import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";

const native = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  permissions: null as unknown as Ref<PermissionSnapshot | null>,
  microphones: [] as { name: string; is_default: boolean }[],
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
vi.mock("@/composables/useMurmurEvents", () => ({
  useMurmurEvents: () => ({ permissions: native.permissions }),
}));

let wrapper: VueWrapper | undefined;
const granted: PermissionSnapshot = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  localStorage.clear();
  native.permissions = ref({ ...granted, microphone: "not-determined" });
  native.microphones = [{ name: "Built-in Microphone", is_default: true }];
  native.listen.mockResolvedValue(vi.fn());
  native.invoke.mockImplementation(async (command) => {
    if (command === "list_input_devices") {
      // Match Rust: input enumeration is intentionally empty before consent.
      return native.permissions.value?.microphone === "granted" ? native.microphones : [];
    }
  });
});
afterEach(async () => {
  wrapper?.unmount();
  wrapper = undefined;
  await flushPromises();
});
async function render() {
  const Step = (await import("./StepMicrophone.vue")).default;
  wrapper = mount(Step, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
  await flushPromises();
  return wrapper;
}
function lastAvailability(step: VueWrapper) {
  const events = step.emitted("availability") ?? [];
  return events[events.length - 1];
}

describe("first-run microphone permission lifecycle", () => {
  it("detects the default input without Retry after app-root initialization precedes consent", async () => {
    // App.vue's menu-bar device hydration initializes the shared list before
    // the user reaches Permissions. Keep the real composable, not a picker mock.
    const { useInputDevices } = await import("@/composables/useInputDevices");
    const input = useInputDevices();
    await flushPromises();
    native.permissions.value = { ...granted };
    await flushPromises();
    const step = await render();
    expect(input.state.value).toBe("ready");
    expect(input.devices.value).toEqual(native.microphones);
    expect(step.text()).toContain("Built-in");
    expect(step.text()).toContain(en.onboarding.microphone.ready);
    expect(step.text()).not.toContain(en.onboarding.microphone.noDevice);
    expect(lastAvailability(step)).toEqual([true]);
    expect(native.invoke).toHaveBeenCalledWith("start_mic_monitor");
  });

  it("automatically starts the meter if consent arrives while the step is mounted", async () => {
    const step = await render();
    expect(lastAvailability(step)).toEqual([false]);
    expect(native.invoke).not.toHaveBeenCalledWith("start_mic_monitor");
    native.permissions.value = { ...granted };
    await flushPromises();
    expect(lastAvailability(step)).toEqual([true]);
    expect(step.text()).toContain(en.onboarding.microphone.ready);
    expect(native.invoke.mock.calls.filter(([command]) => command === "start_mic_monitor")).toHaveLength(1);
  });

  it("still blocks Continue for a genuinely absent input after consent", async () => {
    native.microphones = [];
    native.permissions.value = { ...granted };
    const step = await render();
    expect(step.text()).toContain(en.onboarding.microphone.noDevice);
    expect(lastAvailability(step)).toEqual([false]);
    expect(native.invoke).not.toHaveBeenCalledWith("start_mic_monitor");
  });
});
