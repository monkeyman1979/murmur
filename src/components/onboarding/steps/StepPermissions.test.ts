import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
const state = vi.hoisted(() => ({ permissions: null as unknown as Ref<PermissionSnapshot | null>, invoke: vi.fn(), isEnabled: vi.fn(), enable: vi.fn(), disable: vi.fn() }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => ({ permissions: state.permissions }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
vi.mock("@tauri-apps/plugin-autostart", () => ({ isEnabled: state.isEnabled, enable: state.enable, disable: state.disable }));
let wrapper: VueWrapper;
beforeEach(() => { vi.resetModules(); vi.resetAllMocks(); state.permissions = ref(null); state.isEnabled.mockResolvedValue(false); state.invoke.mockResolvedValue(undefined); });
afterEach(() => { wrapper?.unmount(); vi.restoreAllMocks(); });
async function render() {
  const Step = (await import("./StepPermissions.vue")).default;
  wrapper = mount(Step, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
}

describe("StepPermissions", () => {
  it("shows neutral checking for all three permissions and login hydration", async () => {
    state.isEnabled.mockReturnValue(new Promise(() => {}));
    await render();
    expect(wrapper.findAll("button").filter((button) => button.text() === "Allow")).toHaveLength(0);
    expect(wrapper.text().match(/Checking/g)).toHaveLength(4);
    expect(wrapper.get('[role="switch"]').attributes("disabled")).toBeDefined();
  });
  it("requests native access first and offers Settings only on a second, explicit click", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "not-determined", accessibility: "denied" };
    await render(); await flushPromises();
    expect(wrapper.text()).not.toContain("Relaunch");
    await wrapper.get('[aria-label="Allow: Keyboard shortcut · Input Monitoring"]').trigger("click");
    await wrapper.get('[aria-label="Allow: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([
      ["request_input_monitoring_prompt"],
      ["request_accessibility_prompt"],
    ]);
    const help = wrapper.findAll('[data-testid="permission-row"] details');
    expect(help).toHaveLength(2);
    expect(help.map((item) => item.get('summary').text())).toEqual(["Need help?", "Need help?"]);
    expect(help.every((item) => !(item.element as HTMLDetailsElement).open)).toBe(true);
    expect(help[0].text()).toContain("click + and choose the app you opened");
    expect(help[1].text()).toContain("turn on Murmur, then come back");
    await help[0].get('summary').trigger('click');
    expect((help[0].element as HTMLDetailsElement).open).toBe(true);
    expect(wrapper.text()).not.toContain("Relaunch");
    await wrapper.get('[aria-label="Open Settings: Keyboard shortcut · Input Monitoring"]').trigger("click");
    await wrapper.get('[aria-label="Open Settings: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([
      ["request_input_monitoring_prompt"],
      ["request_accessibility_prompt"],
      ["open_privacy_pane", { pane: "input-monitoring" }],
      ["open_privacy_pane", { pane: "accessibility" }],
    ]);
    expect(wrapper.text()).toContain("Relaunch");
    state.permissions.value = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
    await flushPromises();
    expect(wrapper.text().match(/Granted/g)).toHaveLength(3);
    expect(wrapper.findAll('[data-testid="permission-row"] details')).toHaveLength(0);
    expect(wrapper.text()).not.toContain("Relaunch");
  });
  it("tries native Input Monitoring registration even if macOS reports denied before the first click", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
    await render(); await flushPromises();
    await wrapper.get('[aria-label="Allow: Keyboard shortcut · Input Monitoring"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([["request_input_monitoring_prompt"]]);
    expect(wrapper.find('[aria-label="Open Settings: Keyboard shortcut · Input Monitoring"]').exists()).toBe(true);
    expect(wrapper.text()).not.toContain("Relaunch");
  });
  it("shows only the three setup permissions, not the optional meeting-audio Settings link", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
    await render(); await flushPromises();
    expect(wrapper.findAll('[data-testid="permission-row"]')).toHaveLength(3);
    expect(wrapper.text()).not.toContain("System Audio Recording");
    expect(wrapper.find('[data-testid="meeting-audio-info"]').exists()).toBe(false);
    expect(state.invoke).not.toHaveBeenCalled();
  });
  it("uses only the native microphone dialog on a first request and reflects its grant", async () => {
    state.permissions.value = { microphone: "not-determined", input_monitoring: "granted", accessibility: "granted" };
    await render(); await flushPromises();
    await wrapper.get('[aria-label="Allow: Microphone"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([["request_microphone_prompt"]]);
    state.permissions.value = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
    await flushPromises();
    expect(wrapper.text().match(/Granted/g)).toHaveLength(3);
    expect(state.invoke).not.toHaveBeenCalledWith("open_privacy_pane", { pane: "microphone" });
  });
  it("offers Settings after a microphone denial without repeating the native prompt", async () => {
    state.permissions.value = { microphone: "not-determined", input_monitoring: "granted", accessibility: "granted" };
    await render(); await flushPromises();
    await wrapper.get('[aria-label="Allow: Microphone"]').trigger("click");
    await flushPromises();
    state.permissions.value = { microphone: "denied", input_monitoring: "granted", accessibility: "granted" };
    await flushPromises();
    // Denial alone must not open Settings; the user explicitly chooses it.
    expect(state.invoke.mock.calls).toEqual([["request_microphone_prompt"]]);
    await wrapper.get('[aria-label="Open Settings: Microphone"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([
      ["request_microphone_prompt"],
      ["open_privacy_pane", { pane: "microphone" }],
    ]);
  });
  it("offers Settings explicitly if the native microphone request fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    state.permissions.value = { microphone: "not-determined", input_monitoring: "granted", accessibility: "granted" };
    state.invoke.mockRejectedValueOnce(new Error("prompt unavailable"));
    await render(); await flushPromises();
    await wrapper.get('[aria-label="Allow: Microphone"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([["request_microphone_prompt"]]);
    await wrapper.get('[aria-label="Open Settings: Microphone"]').trigger("click");
    await flushPromises();
    expect(state.invoke.mock.calls).toEqual([
      ["request_microphone_prompt"],
      ["open_privacy_pane", { pane: "microphone" }],
    ]);
  });
  it("shows write failure, rolls back the optional switch, and supports Relaunch", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "denied" };
    state.enable.mockRejectedValue(new Error("plugin"));
    await render(); await flushPromises();
    await wrapper.get('[role="switch"]').trigger("click");
    await flushPromises();
    expect(wrapper.get('[role="switch"]').attributes("aria-checked")).toBe("false");
    expect(wrapper.text()).toContain("Could not update Launch at login");
    expect(wrapper.text()).not.toContain("Relaunch");
    await wrapper.get('[aria-label="Allow: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    await wrapper.get('[aria-label="Open Settings: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    const relaunch = wrapper.findAll("button").find((button) => button.text().includes("Relaunch"))!;
    await relaunch.trigger("click");
    expect(state.invoke).toHaveBeenCalledWith("restart_murmur");
  });
  it("does not open Settings or stack permission requests while a native request is pending", async () => {
    let complete!: (value: string) => void;
    state.permissions.value = { microphone: "granted", input_monitoring: "not-determined", accessibility: "denied" };
    state.invoke.mockImplementationOnce(() => new Promise<string>((resolve) => { complete = resolve; }));
    await render(); await flushPromises();
    await wrapper.get('[aria-label="Allow: Keyboard shortcut · Input Monitoring"]').trigger("click");
    await flushPromises();
    expect(wrapper.get('[aria-label="Open Settings: Keyboard shortcut · Input Monitoring"]').attributes("disabled")).toBeDefined();
    expect(wrapper.get('[aria-label="Allow: Text insertion · Accessibility"]').attributes("disabled")).toBeDefined();
    expect(wrapper.findAll('[data-testid="permission-row"] details')).toHaveLength(0);
    expect(state.invoke.mock.calls).toEqual([["request_input_monitoring_prompt"]]);
    complete("not-determined"); await flushPromises();
    expect(wrapper.get('[aria-label="Open Settings: Keyboard shortcut · Input Monitoring"]').attributes("disabled")).toBeUndefined();
    expect(state.invoke).not.toHaveBeenCalledWith("open_privacy_pane", { pane: "input-monitoring" });
  });
  it("does not suggest relaunching if opening Settings fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    state.permissions.value = { microphone: "granted", input_monitoring: "granted", accessibility: "denied" };
    state.invoke.mockResolvedValueOnce(false).mockRejectedValueOnce(new Error("Settings unavailable"));
    await render(); await flushPromises();
    await wrapper.get('[aria-label="Allow: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    await wrapper.get('[aria-label="Open Settings: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    expect(wrapper.text()).not.toContain("Relaunch");
    expect(wrapper.get('[role="alert"]').text()).toContain("In Privacy & Security, find Text insertion · Accessibility");
    await wrapper.get('[aria-label="Open Settings: Text insertion · Accessibility"]').trigger("click");
    await flushPromises();
    expect(wrapper.find('[role="alert"]').exists()).toBe(false);
  });
  it("uses intrinsic, bounded action buttons for all three permissions", async () => {
    state.permissions.value = { microphone: "denied", input_monitoring: "denied", accessibility: "denied" };
    await render(); await flushPromises();
    const rows = wrapper.findAll('[data-testid="permission-row"]');
    expect(rows).toHaveLength(3);
    for (const row of rows) {
      expect(row.classes()).toContain("items-center");
      expect(row.classes()).toContain("grid-cols-[1.25rem_minmax(0,1fr)_auto]");
      expect(row.get("p").classes()).toContain("text-balance");
      const button = row.get("button");
      expect(button.classes()).toEqual(expect.arrayContaining(["w-fit", "max-w-full", "min-h-9", "whitespace-normal", "justify-self-end"]));
      expect(button.classes()).not.toContain("w-28");
    }
    expect(rows[0].get("p").text()).toBe("For microphone checks, dictation, and meetings you choose to record.");
  });
  it.each([true, false])("keeps status text blank and layout stable through the write and confirmation when toggling to %s", async (target) => {
    let finishWrite!: () => void;
    let finishRead!: (value: boolean) => void;
    const mutation = target ? state.enable : state.disable;
    mutation.mockReturnValue(new Promise<void>((resolve) => { finishWrite = resolve; }));
    state.isEnabled.mockResolvedValue(!target);
    await render(); await flushPromises();
    state.isEnabled.mockReturnValueOnce(new Promise<boolean>((resolve) => { finishRead = resolve; }));
    const row = wrapper.get('[data-testid="autostart-row"]');
    const status = row.get('[data-testid="autostart-status"]');
    const description = row.get("p");
    expect(row.findAll("p")).toHaveLength(1);
    expect(status.text()).toBe("");
    expect(status.element.parentElement!.classList.contains("w-28")).toBe(true);
    expect(status.element.parentElement!.classList.contains("min-h-11")).toBe(true);

    await row.get('[role="switch"]').trigger("click");
    await flushPromises();
    expect(status.text()).toBe("");
    expect(row.get('[role="switch"]').attributes("aria-busy")).toBe("true");
    expect(row.get('[role="switch"]').attributes("disabled")).toBeDefined();
    expect(row.findAll("p")).toHaveLength(1);
    expect(row.get("p").element).toBe(description.element);

    finishWrite(); await flushPromises();
    expect(status.text()).toBe("");
    expect(row.get('[role="switch"]').attributes("aria-busy")).toBe("true");
    expect(row.text()).not.toContain(en.onboarding.permissions.status.checking);
    expect(row.findAll("p")).toHaveLength(1);

    finishRead(target); await flushPromises();
    expect(status.text()).toBe("");
    expect(row.get('[role="switch"]').attributes("aria-busy")).toBe("false");
    expect(row.get('[role="switch"]').attributes("disabled")).toBeUndefined();
    expect(row.get("p").text()).toBe(en.onboarding.permissions.autostart.body);
    expect(row.get('[role="switch"]').attributes("aria-checked")).toBe(String(target));
  });
  it("disables the optional switch during a write to resist concurrent clicks", async () => {
    let finish!: () => void;
    state.enable.mockReturnValue(new Promise<void>((resolve) => { finish = resolve; }));
    await render(); await flushPromises();
    await wrapper.get('[role="switch"]').trigger("click");
    await flushPromises();
    expect(wrapper.get('[role="switch"]').attributes("disabled")).toBeDefined();
    await wrapper.get('[role="switch"]').trigger("click");
    expect(state.enable).toHaveBeenCalledOnce();
    state.isEnabled.mockResolvedValue(true); finish(); await flushPromises();
    expect(wrapper.get('[role="switch"]').attributes("aria-checked")).toBe("true");
  });
});
