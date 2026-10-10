import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import fr from "@/locales/fr.json";
import es from "@/locales/es.json";

const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
let wrapper: VueWrapper | undefined;
let finishVerification: (report: { ok: boolean }) => void;
let finishActivation: () => void;
let rejectActivation: (error: Error) => void;

beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  localStorage.clear();
  native.listen.mockResolvedValue(vi.fn());
  native.invoke.mockImplementation((command: string) => {
    if (command === "get_permissions") return Promise.resolve({ microphone: "granted", input_monitoring: "granted", accessibility: "granted" });
    if (command === "model_info") return Promise.resolve({ path: "/verified-model", exists: true, size: 100, modified: 1, env_override: false, download_url: "", approx_download_bytes: 100 });
    if (command === "verify_model") return new Promise<{ ok: boolean }>((resolve) => { finishVerification = resolve; });
    if (command === "set_active_stt") return new Promise<void>((resolve, reject) => { finishActivation = resolve; rejectActivation = reject; });
    return Promise.resolve();
  });
});
afterEach(async () => { wrapper?.unmount(); wrapper = undefined; await flushPromises(); document.body.innerHTML = ""; localStorage.clear(); });

async function render(step: string) {
  localStorage.setItem("murmur:onboarding-step-key", step);
  const Flow = (await import("./OnboardingFlow.vue")).default;
  wrapper = mount(Flow, {
    global: {
      plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })],
      stubs: {
        OnboardingFrame: { props: ["primaryDisabled"], emits: ["primary"], template: '<button :disabled="primaryDisabled" @click="$emit(\'primary\')">Continue</button>' },
        StepProductOverview: true, StepPermissions: true, StepRequiredModel: true,
        StepMicrophone: true, StepTryIt: true,
      },
    },
  });
  await flushPromises();
  return (await import("@/composables/useOnboarding")).useOnboarding();
}

describe("OnboardingFlow with real model readiness and persistence", () => {
  it("hydrates a saved interface language on the actual Welcome frame and switches without advancing", async () => {
    localStorage.setItem("murmur:app-language", "fr");
    const { i18n } = await import("@/i18n");
    const { appLanguage, setAppLanguage } = await import("@/i18n/appLanguage");
    const Flow = (await import("./OnboardingFlow.vue")).default;
    wrapper = mount(Flow, { attachTo: document.body, global: { plugins: [i18n] } });
    await flushPromises();
    expect(wrapper.get("main h1").text()).toBe(fr.onboarding.steps.welcome.title);
    expect(wrapper.get('#onboarding-app-language').attributes("role")).toBe("combobox");
    expect(wrapper.get('#onboarding-app-language').text()).toBe("Français");
    expect(wrapper.find('label[for="onboarding-app-language"]').exists()).toBe(false);
    expect(wrapper.get('#onboarding-app-language').attributes('aria-label')).toBe(fr.settingsAppLanguage.label);
    expect(wrapper.get("footer").text()).toContain(fr.onboarding.cta.getStarted);
    expect(wrapper.get("footer p").text()).toBe(fr.onboarding.welcome.privacyLink);

    setAppLanguage("es");
    await flushPromises();
    expect(wrapper.get("main h1").text()).toBe(es.onboarding.steps.welcome.title);
    expect(wrapper.get('#onboarding-app-language').text()).toBe("Español");
    expect(wrapper.get("footer").text()).toContain(es.onboarding.cta.getStarted);
    expect(wrapper.get("footer p").text()).toBe(es.onboarding.welcome.privacyLink);
    expect(appLanguage.value).toBe("es");
    expect(localStorage.getItem("murmur:app-language")).toBe("es");
    expect(localStorage.getItem("murmur:onboarding-step-key")).toBe("welcome");
  });

  it.each(["active", "expired", "purchased"])("keeps Ready minimal regardless of legacy license state %s", async (licenseState) => {
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation((command, ...args) => command === "get_trial_state"
      ? Promise.resolve({ state: licenseState }) : original(command, ...args));
    localStorage.setItem("murmur:onboarding-step-key", "ready");
    const Flow = (await import("./OnboardingFlow.vue")).default;
    wrapper = mount(Flow, {
      attachTo: document.body,
      global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
    });
    await flushPromises();
    const main = wrapper.get("main");
    expect(main.get("h1").text()).toBe("You’re ready.");
    expect(main.findAll("p").map((p) => p.text())).toEqual(["Dictate anywhere and capture meetings on your Mac."]);
    expect(main.findAll(".onboarding-brand-mark")).toHaveLength(1);
    expect(main.findAll("button, ul, input")).toHaveLength(0);
    expect(wrapper.get("header").text()).toBe("");
    expect(wrapper.get("header").find('[role="img"]').exists()).toBe(false);
    expect(wrapper.text()).not.toMatch(/Permissions granted|Launch at login|Upgrade|license key|free version|early bird/i);
    expect(native.invoke).not.toHaveBeenCalledWith("get_trial_state");

    const primary = wrapper.findAll("footer button").find((button) => button.text() === "Open Murmur")!;
    expect(primary.attributes("disabled")).toBeDefined();
    finishVerification({ ok: true }); await flushPromises();
    finishActivation(); await flushPromises();
    expect(primary.attributes("disabled")).toBeUndefined();
    await primary.trigger("click");
    expect(localStorage.getItem("murmur:onboarding-completed")).toBe("true");
    expect(native.invoke).not.toHaveBeenCalledWith("activate_license", expect.anything());
  });

  it.each(["microphone", "tryIt", "ready"])("retains %s through existing-file verification and activation", async (step) => {
    const onboarding = await render(step);
    expect(onboarding.stepKey.value).toBe(step);
    expect(localStorage.getItem("murmur:onboarding-step-key")).toBe(step);
    expect(wrapper!.get("button").attributes("disabled")).toBeDefined();
    expect(native.invoke).not.toHaveBeenCalledWith("set_active_stt", expect.anything());
    finishVerification({ ok: true });
    await flushPromises();
    expect(onboarding.stepKey.value).toBe(step);
    expect(wrapper!.get("button").attributes("disabled")).toBeDefined();
    expect(native.invoke).toHaveBeenCalledWith("set_active_stt", { modelId: "whisper-small-multilingual" });
    finishActivation();
    await flushPromises();
    expect(onboarding.stepKey.value).toBe(step);
    expect(localStorage.getItem("murmur:onboarding-step-key")).toBe(step);
    expect(onboarding.completed.value).toBe(false);
    if (step === "ready") {
      await wrapper!.get("button").trigger("click");
      expect(onboarding.completed.value).toBe(true);
    }
  });

  it("redirects a resumed Ready only when activation actually fails", async () => {
    const onboarding = await render("ready");
    finishVerification({ ok: true });
    await flushPromises();
    expect(onboarding.stepKey.value).toBe("ready");
    rejectActivation(new Error("engine unavailable"));
    await flushPromises();
    expect(onboarding.stepKey.value).toBe("requiredModel");
    expect(localStorage.getItem("murmur:onboarding-step-key")).toBe("requiredModel");
    expect(onboarding.completed.value).toBe(false);
  });
});
