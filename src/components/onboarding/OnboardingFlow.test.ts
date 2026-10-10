import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { computed, defineComponent, ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "@/i18n";
import { appLanguage, APP_LANGUAGE_KEY, setAppLanguage, supportedAppLanguages } from "@/i18n/appLanguage";
import { Select, SelectTrigger } from "@/components/ui/select";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import type { ModelStatus } from "@/composables/useModelDownload";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
import { ONBOARDING_STEPS, type StepKey } from "@/composables/useOnboarding";
import OnboardingFlow from "./OnboardingFlow.vue";
import OnboardingFrame from "./OnboardingFrame.vue";

const state = vi.hoisted(() => ({
  step: null as unknown as Ref<StepKey>,
  permissions: null as unknown as Ref<PermissionSnapshot | null>,
  status: null as unknown as Ref<ModelStatus>,
  modelState: null as unknown as Ref<string>,
  retry: vi.fn(), stopMonitor: vi.fn(), openUrl: vi.fn(),
  markComplete: vi.fn(), refresh: vi.fn(), download: vi.fn(), invoke: vi.fn(),
}));
vi.mock("@/composables/useOnboarding", async (original) => ({
  ...await original<typeof import("@/composables/useOnboarding")>(),
  useOnboarding: () => ({ stepKey: state.step, stepIndex: computed(() => ONBOARDING_STEPS.indexOf(state.step.value)), markComplete: state.markComplete }),
}));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => ({ permissions: state.permissions }) }));
vi.mock("@/composables/useOnboardingModel", () => ({ useOnboardingModel: () => ({ status: state.status, state: state.modelState, operational: computed(() => state.modelState.value === "operational"), busy: computed(() => ["checking", "verifying", "downloading", "activating"].includes(state.modelState.value)), prepare: state.refresh, download: state.download, retry: state.retry }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => state.invoke(...args) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: (...args: unknown[]) => state.openUrl(...args) }));

const granted: PermissionSnapshot = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
const info = { path: "/model", exists: true, size: 488000000, modified: null, env_override: false, download_url: "", approx_download_bytes: 488000000 };
let wrapper: VueWrapper;
function render() {
  wrapper = mount(OnboardingFlow, {
    global: {
      plugins: [i18n],
      stubs: {
        OnboardingFrame: { props: ["title", "subtitle", "primaryDisabled", "currentStep", "primaryLabel", "centered", "centerWelcome", "bleedRight", "motionKey"], emits: ["primary", "back"], template: '<div><slot name="identity"/><slot name="eyebrow"/><div data-testid="narrative"><slot name="left"/></div><slot name="right"/><footer><slot name="secondary"/></footer></div>' },
        StepProductOverview: true, StepPermissions: true, StepRequiredModel: true,
        StepMicrophone: defineComponent({ name: "StepMicrophone", emits: ["availability"], setup: (_, { expose }) => { expose({ stopForExit: state.stopMonitor }); }, template: "<div />" }),
        StepTryIt: defineComponent({ name: "StepTryIt", props: ["activePhase"], emits: ["holdComplete", "toggleComplete"], template: "<div />" }),
      },
    },
  });
  return wrapper;
}
function frame() { return wrapper.findComponent(OnboardingFrame); }
async function next() { frame().vm.$emit("primary"); await flushPromises(); }

beforeEach(() => {
  vi.clearAllMocks();
  setAppLanguage("en");
  state.step = ref<StepKey>("welcome");
  state.permissions = ref<PermissionSnapshot | null>(granted);
  state.status = ref<ModelStatus>({ kind: "ready", info });
  state.modelState = ref("operational");
  state.stopMonitor.mockResolvedValue(true);
  state.invoke.mockResolvedValue(undefined);
  state.openUrl.mockResolvedValue(undefined);
  state.refresh.mockResolvedValue(info);
  state.download.mockResolvedValue(undefined);
});
afterEach(() => {
  wrapper?.unmount();
  setAppLanguage("en");
});

describe("OnboardingFlow", () => {
  it("follows all seven steps; both demo phases share step six and only Ready completes", async () => {
    render();
    expect(frame().props("primaryLabel")).toBe("Get started");
    expect(frame().props("centered")).toBe(true);
    expect(frame().props("centerWelcome")).toBe(true);
    for (const step of ["productOverview", "permissions", "requiredModel", "microphone"]) {
      await next();
      expect(state.step.value).toBe(step);
      expect(state.markComplete).not.toHaveBeenCalled();
    }
    await next();
    expect(state.step.value).toBe("microphone");
    wrapper.findComponent({ name: "StepMicrophone" }).vm.$emit("availability", true);
    await next();
    expect(state.step.value).toBe("tryIt");
    await next();
    expect(state.step.value).toBe("tryIt");
    const demo = wrapper.findComponent({ name: "StepTryIt" });
    demo.vm.$emit("holdComplete");
    await flushPromises();
    expect(frame().props("primaryLabel")).toBe("Try hands-free");
    await next();
    expect(demo.props("activePhase")).toBe("toggle");
    expect(frame().props("motionKey")).toBe("toggle");
    expect(frame().props("primaryLabel")).toBe("Complete this gesture");
    expect(frame().props("currentStep")).toBe(6);
    await next();
    expect(state.step.value).toBe("tryIt");
    demo.vm.$emit("toggleComplete");
    await flushPromises();
    expect(frame().props("primaryLabel")).toBe("Continue");
    await next();
    expect(state.step.value).toBe("ready");
    expect(frame().props("primaryLabel")).toBe("Open Murmur");
    expect(frame().props("centerWelcome")).toBe(false);
    expect(state.markComplete).not.toHaveBeenCalled();
    await next();
    expect(state.markComplete).toHaveBeenCalledOnce();
  });

  it("switches the welcome screen and later setup copy immediately using the saved App language", async () => {
    render();
    const selector = wrapper.getComponent(Select);
    const trigger = wrapper.getComponent(SelectTrigger);
    expect(trigger.attributes("aria-label")).toBe(en.settingsAppLanguage.label);
    expect(trigger.attributes("id")).toBe("onboarding-app-language");
    expect(wrapper.find('label[for="onboarding-app-language"]').exists()).toBe(false);
    expect(selector.props("modelValue")).toBe("en");
    expect(frame().props("currentStep")).toBe(1);

    for (const [code, catalog] of [["es", es], ["fr", fr], ["ru", ru], ["en", en]] as const) {
      selector.vm.$emit("update:modelValue", code);
      await flushPromises();
      expect(appLanguage.value).toBe(code);
      expect(localStorage.getItem(APP_LANGUAGE_KEY)).toBe(code);
      expect(frame().props("title")).toBe(catalog.onboarding.steps.welcome.title);
      expect(frame().props("primaryLabel")).toBe(catalog.onboarding.cta.getStarted);
      expect(trigger.attributes("aria-label")).toBe(catalog.settingsAppLanguage.label);
      expect(wrapper.get('footer p').text()).toBe(catalog.onboarding.welcome.privacyLink);
      expect(frame().props("currentStep")).toBe(1);
    }
    expect(supportedAppLanguages.map(({ code }) => code)).toEqual(["en", "es", "fr", "ru"]);

    selector.vm.$emit("update:modelValue", "es");
    await flushPromises();
    await next();
    expect(frame().props("title")).toBe(es.onboarding.steps.productOverview.title);
    expect(wrapper.findComponent(Select).exists()).toBe(false);
    frame().vm.$emit("back");
    await flushPromises();
    expect(wrapper.getComponent(Select).props("modelValue")).toBe("es");
  });

  it("keeps only the translated Privacy policy link in the Welcome footer", () => {
    render();
    const privacy = wrapper.get("footer p");
    expect(privacy.text()).toBe(en.onboarding.welcome.privacyLink);
    expect(privacy.get("a").attributes("href")).toBe("https://github.com/monkeyman1979/murmur/blob/main/docs/PRIVACY.md");
  });

  it("opens the privacy link explicitly and reports launch failure", async () => {
    render();
    const link = wrapper.get('footer a');
    await link.trigger("click");
    await flushPromises();
    expect(state.openUrl).toHaveBeenCalledWith("https://github.com/monkeyman1979/murmur/blob/main/docs/PRIVACY.md");
    expect(wrapper.find('[role="alert"]').exists()).toBe(false);
    state.openUrl.mockRejectedValueOnce(new Error("browser unavailable"));
    await link.trigger("click");
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain("Could not open");
  });

  it("replaces the overview subtitle with feature copy and an edge-bleeding Home preview", async () => {
    state.step.value = "productOverview";
    render();
    expect(frame().props("subtitle")).toBe("");
    expect(wrapper.text()).not.toContain("Two ways to use Murmur");
    expect(wrapper.get('[data-testid="narrative"]').findComponent({ name: "StepProductOverview" }).exists()).toBe(true);
    expect(frame().props("bleedRight")).toBe(true);
    expect(wrapper.find('[data-testid="home-product-preview"]').exists()).toBe(true);
  });

  it("keeps the microphone screen simple without removing its availability gate", async () => {
    state.step.value = "microphone";
    render();
    const narrative = wrapper.get('[data-testid="narrative"]');
    expect(narrative.text()).toBe("");
    expect(narrative.find("svg").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("The level check is not retained.");
    const microphone = wrapper.findComponent({ name: "StepMicrophone" });
    expect(microphone.exists()).toBe(true);
    expect(frame().props("primaryDisabled")).toBe(true);
    microphone.vm.$emit("availability", true);
    await flushPromises();
    expect(frame().props("primaryDisabled")).toBe(false);
  });

  it("guards resumed and changing prerequisites in permission-first order", async () => {
    state.step.value = "ready";
    state.permissions.value = { ...granted, input_monitoring: "denied" };
    state.modelState.value = "missing";
    render();
    expect(state.step.value).toBe("permissions");
    await next();
    expect(state.step.value).toBe("permissions");
    state.permissions.value = granted;
    await flushPromises();
    expect(state.step.value).toBe("permissions");
    await next();
    expect(state.step.value).toBe("requiredModel");
    await next();
    expect(state.retry).toHaveBeenCalledOnce();
    expect(state.step.value).toBe("requiredModel");
    state.modelState.value = "operational";
    await flushPromises();
    expect(state.step.value).toBe("requiredModel");
    await next();
    expect(state.step.value).toBe("microphone");
    state.modelState.value = "missing";
    await flushPromises();
    expect(state.step.value).toBe("requiredModel");
  });

  it("does not redirect a resumed user before prerequisites are known", async () => {
    state.step.value = "tryIt";
    state.permissions.value = null;
    state.modelState.value = "checking";
    render();
    expect(state.step.value).toBe("tryIt");
    state.permissions.value = { microphone: "unknown", input_monitoring: "unknown", accessibility: "unknown" };
    await flushPromises();
    expect(state.step.value).toBe("tryIt");
    state.permissions.value = { ...granted, microphone: "denied" };
    await flushPromises();
    expect(state.step.value).toBe("permissions");
  });

  it("preserves explicit model download and stage retry behavior", async () => {
    state.step.value = "requiredModel";
    state.modelState.value = "checking";
    render();
    expect(frame().props("primaryDisabled")).toBe(true);
    state.modelState.value = "downloading";
    await flushPromises();
    expect(frame().props("primaryDisabled")).toBe(true);
    await next();
    expect(state.step.value).toBe("requiredModel");
    state.modelState.value = "error";
    await next();
    expect(state.retry).toHaveBeenCalledOnce();
  });

  it("blocks hydrated Ready until every permission and operational model are confirmed", async () => {
    state.step.value = "ready";
    state.permissions.value = null;
    state.modelState.value = "checking";
    render();
    expect(frame().props("primaryDisabled")).toBe(true);
    await next();
    expect(state.markComplete).not.toHaveBeenCalled();
    expect(state.step.value).toBe("ready");
    state.permissions.value = granted;
    await next();
    expect(state.markComplete).not.toHaveBeenCalled();
    state.modelState.value = "operational";
    await next();
    expect(state.markComplete).toHaveBeenCalledOnce();
  });

  it("known denial takes precedence even with other permissions unknown", async () => {
    state.step.value = "ready";
    state.permissions.value = { microphone: "denied", accessibility: "unknown", input_monitoring: "unknown" };
    render();
    expect(state.step.value).toBe("permissions");
    await next();
    expect(state.markComplete).not.toHaveBeenCalled();
  });

  it.each(["error", "mismatch", "missing", "downloading"])("redirects only established non-operational model state %s", async (stage) => {
    state.step.value = "tryIt";
    state.modelState.value = stage;
    render();
    expect(state.step.value).toBe("requiredModel");
    await next();
    expect(state.step.value).toBe("requiredModel");
  });

  it.each(["verifying", "activating"])("preserves resumed progress while %s but blocks completion", async (stage) => {
    state.step.value = "ready";
    state.modelState.value = stage;
    render();
    expect(state.step.value).toBe("ready");
    expect(frame().props("primaryDisabled")).toBe(true);
    await next();
    expect(state.markComplete).not.toHaveBeenCalled();
    state.modelState.value = "operational";
    await next();
    expect(state.markComplete).toHaveBeenCalledOnce();
  });

  it("awaits monitor shutdown before mounting the shortcut test", async () => {
    let release!: (value: boolean) => void;
    state.stopMonitor.mockReturnValue(new Promise<boolean>((resolve) => { release = resolve; }));
    state.step.value = "microphone";
    render();
    wrapper.findComponent({ name: "StepMicrophone" }).vm.$emit("availability", true);
    await next();
    expect(state.step.value).toBe("microphone");
    expect(frame().props("primaryDisabled")).toBe(true);
    release(true);
    await flushPromises();
    expect(state.step.value).toBe("tryIt");
  });

  it("Back revisits a demo phase without changing global progress", async () => {
    state.step.value = "tryIt";
    render();
    wrapper.findComponent({ name: "StepTryIt" }).vm.$emit("holdComplete");
    await next();
    frame().vm.$emit("back");
    await flushPromises();
    expect(wrapper.findComponent({ name: "StepTryIt" }).props("activePhase")).toBe("hold");
    expect(frame().props("currentStep")).toBe(6);
    frame().vm.$emit("back");
    await flushPromises();
    expect(state.step.value).toBe("microphone");
  });
});
