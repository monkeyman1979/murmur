import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
const state = vi.hoisted(() => ({ permissions: null as unknown as Ref<PermissionSnapshot | null>, invoke: vi.fn() }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => ({ permissions: state.permissions }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
let wrapper: VueWrapper;
beforeEach(() => { vi.resetModules(); vi.resetAllMocks(); vi.useFakeTimers(); state.permissions = ref(null); state.invoke.mockResolvedValue(undefined); });
afterEach(() => { wrapper?.unmount(); vi.useRealTimers(); });
async function render() {
  const Permissions = (await import("./Permissions.vue")).default;
  wrapper = mount(Permissions, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
  await flushPromises();
}

const IM = "Input Monitoring";
const titles = () => wrapper.findAll("p.text-sm").map((title) => title.text());

describe("Settings permissions", () => {
  it("lists Accessibility for the shortcut and never shows Input Monitoring while Accessibility is missing", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "denied" };
    await render();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(titles()).toEqual(["Microphone", "Accessibility"]);
    expect(wrapper.text()).toContain(en.onboarding.permissions.microphone.body);
    expect(wrapper.text()).toContain(en.onboarding.permissions.accessibility.body);
    expect(wrapper.text()).toContain("Input Monitoring only appears if your Mac needs it separately.");
    expect(wrapper.text()).not.toContain("CGEventTap");
  });

  it("hides Input Monitoring once Accessibility starts the shortcut listener", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
    await render();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(titles()).not.toContain(IM);
  });

  it("offers Input Monitoring only if listening is still missing after the Accessibility grant settles", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
    await render();
    await vi.advanceTimersByTimeAsync(5_999);
    expect(titles()).not.toContain(IM);
    await vi.advanceTimersByTimeAsync(1);
    expect(titles()).toEqual(["Microphone", "Accessibility", IM]);
  });

  it("withdraws the fallback when Accessibility is revoked", async () => {
    state.permissions.value = { microphone: "granted", input_monitoring: "denied", accessibility: "granted" };
    await render();
    await vi.advanceTimersByTimeAsync(6_000);
    expect(titles()).toContain(IM);
    state.permissions.value = { ...state.permissions.value, accessibility: "denied" };
    await flushPromises();
    expect(titles()).not.toContain(IM);
  });
});
