import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
const state = vi.hoisted(() => ({ permissions: null as unknown as Ref<PermissionSnapshot | null>, invoke: vi.fn(), isEnabled: vi.fn(), enable: vi.fn(), disable: vi.fn() }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => ({ permissions: state.permissions }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
vi.mock("@tauri-apps/plugin-autostart", () => ({ isEnabled: state.isEnabled, enable: state.enable, disable: state.disable }));
let wrapper: VueWrapper;
beforeEach(() => { vi.resetModules(); vi.resetAllMocks(); state.permissions = ref(null); state.isEnabled.mockResolvedValue(false); state.invoke.mockResolvedValue(undefined); });
afterEach(() => { wrapper?.unmount(); vi.restoreAllMocks(); });
async function render(catalog = en) {
  const Step = (await import("./StepPermissions.vue")).default;
  wrapper = mount(Step, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en: catalog } })] } });
}

const AX = "Accessibility";
const IM = "Input Monitoring";
const granted = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" } as const;

describe("StepPermissions", () => {
  it.each([en, es, fr, ru])("keeps permission purposes short and the Settings switch instruction visible", async (catalog) => {
    state.permissions.value = { microphone: "not-determined", input_monitoring: "denied", accessibility: "denied" };
    await render(catalog); await flushPromises();
    const rows = wrapper.findAll('[data-testid="permission-row"]');
    expect(rows).toHaveLength(2);
    expect(rows[0].get("h2").text()).toBe(catalog.onboarding.permissions.microphone.title);
    expect(rows[0].get("p").text()).toBe(catalog.onboarding.permissions.microphone.body);
    expect(rows[1].get("h2").text()).toBe(catalog.onboarding.permissions.accessibility.title);
    expect(rows[1].get("p").text()).toBe(catalog.onboarding.permissions.accessibility.body);
    expect(rows[1].get('[data-testid="accessibility-settings-hint"]').text()).toBe(catalog.onboarding.permissions.accessibility.settingsHint);
    expect(rows[1].find("details").exists()).toBe(false);
    expect(catalog.onboarding.permissions.microphone.body.split(/[.!?]/).filter(Boolean)).toHaveLength(1);
    expect(catalog.onboarding.permissions.accessibility.body.split(/[.!?]/).filter(Boolean)).toHaveLength(1);
    expect(catalog.onboarding.permissions.accessibility.title).not.toContain(" · ");
    state.permissions.value = { ...granted };
    await flushPromises();
    expect(wrapper.find('[data-testid="accessibility-settings-hint"]').exists()).toBe(false);
  });

  it("shows neutral checking for the required permissions and login hydration", async () => {
    state.isEnabled.mockReturnValue(new Promise(() => {}));
    await render();
    expect(wrapper.findAll("button").filter((button) => button.text() === "Allow")).toHaveLength(0);
    expect(wrapper.text().match(/Checking/g)).toHaveLength(3);
    expect(wrapper.get('[role="switch"]').attributes("disabled")).toBeDefined();
  });
  it("asks for Accessibility for the shortcut and never shows Input Monitoring while Accessibility is missing", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "denied" };
      state.invoke.mockResolvedValueOnce(false);
      await render(); await flushPromises();
      expect(wrapper.findAll('[data-testid="permission-row"]')).toHaveLength(2);
      expect(wrapper.text()).not.toContain(IM);
      await wrapper.get(`[aria-label="Allow: ${AX}"]`).trigger("click");
      await flushPromises();
      expect(state.invoke.mock.calls).toEqual([["request_accessibility_prompt"]]);
      const help = wrapper.findAll('[data-testid="permission-row"] details');
      expect(help).toHaveLength(1);
      expect(help[0].get("summary").text()).toBe("Need help?");
      expect(help[0].text()).toContain("System Settings → Privacy & Security → Accessibility");
      expect(help[0].text()).toContain("turn on Murmur, then return here");
      expect(help[0].text()).not.toContain("{permission}");
      await vi.advanceTimersByTimeAsync(60_000);
      expect(wrapper.text()).not.toContain(IM);
      expect(wrapper.text()).not.toContain("click +");
      expect(wrapper.text()).not.toContain("Relaunch");
      await wrapper.get(`[aria-label="Open Settings: ${AX}"]`).trigger("click");
      await flushPromises();
      expect(state.invoke).toHaveBeenLastCalledWith("open_privacy_pane", { pane: "accessibility" });
      expect(wrapper.text()).toContain("Relaunch");
      state.permissions.value = { ...granted };
      await flushPromises();
      expect(wrapper.text().match(/Granted/g)).toHaveLength(2);
      expect(wrapper.findAll('[data-testid="permission-row"] details')).toHaveLength(0);
      expect(wrapper.text()).not.toContain("Relaunch");
      await vi.advanceTimersByTimeAsync(60_000);
      expect(wrapper.text()).not.toContain(IM);
    } finally { vi.useRealTimers(); }
  });
  it("gives the shortcut listener time to start before offering the Input Monitoring fallback", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "denied" };
      await render(); await flushPromises();
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
      await flushPromises();
      await vi.advanceTimersByTimeAsync(5_000);
      expect(wrapper.text()).not.toContain(IM);
      state.permissions.value = { ...granted };
      await vi.advanceTimersByTimeAsync(5_000);
      expect(wrapper.text()).not.toContain(IM);
      expect(wrapper.text()).not.toContain("Relaunch");
    } finally { vi.useRealTimers(); }
  });
  it("cancels the fallback delay on Accessibility revoke and restarts it on re-grant", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
      await render(); await flushPromises();
      await vi.advanceTimersByTimeAsync(5_000);
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "denied" };
      await flushPromises();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(wrapper.text()).not.toContain(IM);
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
      await flushPromises();
      await vi.advanceTimersByTimeAsync(5_999);
      expect(wrapper.text()).not.toContain(IM);
      await vi.advanceTimersByTimeAsync(1);
      expect(wrapper.text()).toContain(IM);
    } finally { vi.useRealTimers(); }
  });
  it("clears its pending fallback timer when the step unmounts", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
      await render(); await flushPromises();
      expect(vi.getTimerCount()).toBe(1);
      wrapper.unmount();
      expect(vi.getTimerCount()).toBe(0);
    } finally { vi.useRealTimers(); }
  });
  it("offers an Input Monitoring fallback with Relaunch help, without + instructions, if listening is still missing", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
      state.invoke.mockResolvedValueOnce("denied");
      await render(); await flushPromises();
      expect(wrapper.text()).not.toContain(IM);
      await vi.advanceTimersByTimeAsync(6_000);
      expect(wrapper.findAll('[data-testid="permission-row"]')).toHaveLength(3);
      expect(wrapper.text()).toContain("Relaunch");
      await wrapper.get(`[aria-label="Allow: ${IM}"]`).trigger("click");
      await flushPromises();
      expect(state.invoke.mock.calls).toEqual([["request_input_monitoring_prompt"]]);
      expect(wrapper.find(`[aria-label="Open Settings: ${IM}"]`).exists()).toBe(true);
      expect(wrapper.text()).not.toContain("click +");
      expect(state.invoke).not.toHaveBeenCalledWith("open_privacy_pane", { pane: "input-monitoring" });
      state.permissions.value = { ...granted };
      await flushPromises();
      expect(wrapper.text()).not.toContain(IM);
      expect(wrapper.text()).not.toContain("Relaunch");
    } finally { vi.useRealTimers(); }
  });
  it("keeps Allow instead of flipping to Open Settings when the native request grants", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
      state.invoke.mockResolvedValueOnce("granted");
      await render(); await vi.advanceTimersByTimeAsync(6_000);
      await wrapper.get(`[aria-label="Allow: ${IM}"]`).trigger("click");
      await flushPromises();
      expect(wrapper.find(`[aria-label="Allow: ${IM}"]`).exists()).toBe(true);
      expect(wrapper.find(`[aria-label="Open Settings: ${IM}"]`).exists()).toBe(false);
      expect(wrapper.findAll('[data-testid="permission-row"] details')).toHaveLength(0);
    } finally { vi.useRealTimers(); }
  });
  it("shows only the required setup permissions, not the optional meeting-audio Settings link", async () => {
    state.permissions.value = { ...granted };
    await render(); await flushPromises();
    expect(wrapper.findAll('[data-testid="permission-row"]')).toHaveLength(2);
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
    state.permissions.value = { ...granted };
    await flushPromises();
    expect(wrapper.text().match(/Granted/g)).toHaveLength(2);
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
    await wrapper.get(`[aria-label="Allow: ${AX}"]`).trigger("click");
    await flushPromises();
    await wrapper.get(`[aria-label="Open Settings: ${AX}"]`).trigger("click");
    await flushPromises();
    const relaunch = wrapper.findAll("button").find((button) => button.text().includes("Relaunch"))!;
    await relaunch.trigger("click");
    expect(state.invoke).toHaveBeenCalledWith("restart_murmur");
  });
  it("does not open Settings or stack permission requests while a native request is pending", async () => {
    let complete!: (value: boolean) => void;
    state.permissions.value = { microphone: "not-determined", input_monitoring: "denied", accessibility: "denied" };
    state.invoke.mockImplementationOnce(() => new Promise<boolean>((resolve) => { complete = resolve; }));
    await render(); await flushPromises();
    await wrapper.get(`[aria-label="Allow: ${AX}"]`).trigger("click");
    await flushPromises();
    expect(wrapper.get(`[aria-label="Allow: ${AX}"]`).attributes("disabled")).toBeDefined();
    expect(wrapper.get('[aria-label="Allow: Microphone"]').attributes("disabled")).toBeDefined();
    expect(wrapper.findAll('[data-testid="permission-row"] details')).toHaveLength(0);
    expect(state.invoke.mock.calls).toEqual([["request_accessibility_prompt"]]);
    complete(false); await flushPromises();
    expect(wrapper.get(`[aria-label="Open Settings: ${AX}"]`).attributes("disabled")).toBeUndefined();
    expect(wrapper.findAll('[data-testid="permission-row"] details')).toHaveLength(1);
    expect(state.invoke).not.toHaveBeenCalledWith("open_privacy_pane", { pane: "accessibility" });
  });
  it("does not suggest relaunching if opening Settings fails", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    state.permissions.value = { microphone: "granted", input_monitoring: "granted", accessibility: "denied" };
    state.invoke.mockResolvedValueOnce(false).mockRejectedValueOnce(new Error("Settings unavailable"));
    await render(); await flushPromises();
    await wrapper.get(`[aria-label="Allow: ${AX}"]`).trigger("click");
    await flushPromises();
    await wrapper.get(`[aria-label="Open Settings: ${AX}"]`).trigger("click");
    await flushPromises();
    expect(wrapper.text()).not.toContain("Relaunch");
    expect(wrapper.get('[role="alert"]').text()).toContain(`In Privacy & Security, find ${AX}`);
    await wrapper.get(`[aria-label="Open Settings: ${AX}"]`).trigger("click");
    await flushPromises();
    expect(wrapper.find('[role="alert"]').exists()).toBe(false);
  });
  it("uses intrinsic, bounded action buttons for every permission row", async () => {
    vi.useFakeTimers();
    try {
      state.permissions.value = { microphone: "denied", input_monitoring: "denied", accessibility: "granted" };
      await render(); await vi.advanceTimersByTimeAsync(6_000);
      const rows = wrapper.findAll('[data-testid="permission-row"]');
      expect(rows).toHaveLength(3);
      for (const row of rows) {
        expect(row.classes()).toContain("items-center");
        expect(row.classes()).toContain("grid-cols-[1.25rem_minmax(0,1fr)_auto]");
        expect(row.get("p").classes()).toContain("text-balance");
      }
      for (const row of [rows[0], rows[2]]) {
        const button = row.get("button");
        expect(button.classes()).toEqual(expect.arrayContaining(["w-fit", "max-w-full", "min-h-9", "whitespace-normal", "justify-self-end"]));
        expect(button.classes()).not.toContain("w-28");
      }
      expect(rows[0].get("p").text()).toBe("Record audio for dictation and meetings.");
    } finally { vi.useRealTimers(); }
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
