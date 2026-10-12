import { flushPromises, mount } from "@vue/test-utils";
import { defineComponent, onMounted, ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import { invoke } from "@tauri-apps/api/core";
import { createMemoryHistory, createRouter } from "vue-router";
import en from "@/locales/en.json";
import App from "@/App.vue";
import AppEntrance from "@/components/AppEntrance.vue";
import OnboardingFrame from "@/components/onboarding/OnboardingFrame.vue";

const state = vi.hoisted(() => ({
  completed: null as unknown as Ref<boolean>,
  storeHealth: { kind: "durable" } as { kind: string; reason?: string },
  launchedAtLogin: false,
  launchSourceError: null as Error | null,
  listeners: new Map<string, () => void>(),
}));
vi.mock("@/composables/useOnboarding", () => ({ useOnboarding: () => ({ completed: state.completed }) }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string) => {
    if (command === "startup_launch_at_login") {
      if (state.launchSourceError) throw state.launchSourceError;
      return state.launchedAtLogin;
    }
    return command === "capture_store_health" ? state.storeHealth : undefined;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (name, callback) => { state.listeners.set(name, callback); return () => state.listeners.delete(name); }) }));
vi.mock("@/composables/useTheme", () => ({ useTheme: () => ({ effectiveMode: ref("light") }) }));
vi.mock("@/composables/useMeetingCapture", () => ({ useMeetingCapture: () => ({ action: ref(null), activeCapture: ref(null), startFlowRequested: ref(false) }) }));
vi.mock("@/composables/useMenuBarDevices", () => ({ useMenuBarDevices: vi.fn() }));
vi.mock("@/composables/useVocabulary", () => ({ useVocabulary: vi.fn() }));
vi.mock("@/composables/useCorrections", () => ({ useCorrections: vi.fn() }));
vi.mock("@/composables/useHistoryRetention", () => ({ useHistoryRetention: vi.fn() }));
vi.mock("@/composables/useHotkeyBinding", () => ({ useHotkeyBinding: vi.fn() }));
vi.mock("@/composables/useActiveModels", () => ({ useActiveModels: vi.fn() }));
vi.mock("@/composables/useSoundCues", () => ({ useSoundCues: vi.fn() }));
vi.mock("@/composables/useMuteWhileDictating", () => ({ useMuteWhileDictating: vi.fn() }));
vi.mock("@/composables/useMeetingAudioRetention", () => ({ useMeetingAudioRetention: vi.fn() }));
vi.mock("@/composables/useMeetingTranscriptRetention", () => ({ useMeetingTranscriptRetention: vi.fn() }));

beforeEach(() => {
  vi.clearAllMocks();
  state.launchSourceError = null;
  state.listeners.clear();
});
afterEach(() => {
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

async function render(
  completed = false,
  storeHealth: { kind: string; reason?: string } = { kind: "durable" },
  launchedAtLogin = false,
) {
    state.completed = ref(completed);
    state.storeHealth = storeHealth;
    state.launchedAtLogin = launchedAtLogin;
    const router = createRouter({ history: createMemoryHistory(), routes: [{ path: "/", component: { template: '<p>Home content</p><Teleport defer to="#page-header-actions"><button>Home action</button></Teleport>' }, meta: { title: "Home" } }, { path: "/settings", component: { template: "<p>Settings content</p>" } }, ...["history", "captures", "dictionary"].map((name) => ({ path: `/${name}`, name, component: { template: `<p>${name} content</p>` } }))] });
    await router.push("/");
    await router.isReady();
    const wrapper = mount(App, {
      attachTo: document.body,
      global: {
        plugins: [router, createI18n({ legacy: false, locale: "en", messages: { en } })],
        stubs: {
          transition: false,
          AppSidebar: true, AppToaster: true, SidebarTrigger: true,
          SidebarProvider: { template: "<div><slot /></div>" },
          SidebarInset: { template: "<div><slot /></div>" },
          OnboardingFlow: defineComponent({
            components: { OnboardingFrame },
            setup: () => ({ complete: () => { state.completed.value = true; } }),
            template: '<OnboardingFrame title="Ready" subtitle="Setup complete" step-label="Step 7 of 7" :current-step="7" :total-steps="7" primary-label="Open Murmur" @primary="complete" />',
          }),
        },
      },
    });
    await flushPromises();
    return { wrapper, router };
}

describe("App startup visibility", () => {
  it("shows a returning user's deliberate cold launch", async () => {
    const { wrapper } = await render(true);
    expect(invoke).toHaveBeenCalledWith("startup_launch_at_login");
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "show_main_window")).toHaveLength(1);
    wrapper.unmount();
  });

  it("keeps login startup quiet after onboarding", async () => {
    const { wrapper } = await render(true, undefined, true);
    expect(invoke).toHaveBeenCalledWith("startup_launch_at_login");
    expect(invoke).not.toHaveBeenCalledWith("show_main_window");
    expect(wrapper.findComponent(AppEntrance).exists()).toBe(true);
    wrapper.unmount();
  });

  it("shows unfinished onboarding even when launched at login", async () => {
    const { wrapper } = await render(false, undefined, true);
    expect(invoke).toHaveBeenCalledWith("show_main_window");
    wrapper.unmount();
  });

  it("shows rather than trapping the user in the tray if launch-source lookup fails", async () => {
    state.launchSourceError = new Error("launch source unavailable");
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const { wrapper } = await render(true, undefined, true);
    expect(error).toHaveBeenCalledWith("[murmur] startup launch source could not be checked", state.launchSourceError);
    expect(invoke).toHaveBeenCalledWith("show_main_window");
    wrapper.unmount();
  });
});

describe("App onboarding completion focus", () => {
  it("clears a legacy forced dictation language without changing the App language", async () => {
    localStorage.setItem("murmur:dictation-language", "pt");
    localStorage.setItem("murmur:app-language", "ru");
    const { wrapper } = await render(true);
    expect(localStorage.getItem("murmur:dictation-language")).toBeNull();
    expect(localStorage.getItem("murmur:app-language")).toBe("ru");
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "set_dictation_language")).toBe(false);
    wrapper.unmount();
    localStorage.removeItem("murmur:app-language");
  });

  it("persistently discloses session-only history when durable storage is unavailable", async () => {
    const { wrapper, router } = await render(true, {
      kind: "ephemeral",
      reason: "durable_storage_unavailable",
    });
    const warning = wrapper.get('[data-testid="storage-warning"]');
    expect(warning.attributes("role")).toBe("alert");
    expect(warning.text()).toContain("History is session-only");
    expect(warning.text()).toContain("will not be saved after Murmur quits");
    await router.push("/dictionary");
    await flushPromises();
    expect(wrapper.get('[data-testid="storage-warning"]').text()).toContain(
      "meeting recording is unavailable",
    );
    wrapper.unmount();
  });

  it("does not show a storage warning for a verified durable store", async () => {
    const { wrapper } = await render(true);
    expect(wrapper.find('[data-testid="storage-warning"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("enters the app without a purchase prompt, including after completing onboarding", async () => {
    const { wrapper, router } = await render(false);
    expect(document.querySelector('[data-slot="dialog-content"]')).toBeNull();
    await wrapper.get("footer button").trigger("click");
    await flushPromises();
    await router.push("/dictionary");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(wrapper.text()).toContain("dictionary content");
    expect(state.listeners.has("murmur://activation-required")).toBe(false);
    expect(document.querySelector("#license-key-input")).toBeNull();
    wrapper.unmount();
  });

  it("replaces the actual route body immediately after onboarding", async () => {
    const { wrapper, router } = await render();
    await wrapper.get("footer button").trigger("click");
    await flushPromises();
    const home = wrapper.findAll("p").find((node) => node.text() === "Home content")!.element;
    await router.push("/settings");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(wrapper.text()).toContain("Settings content");
    expect(home.isConnected).toBe(false);
    wrapper.unmount();
  });
  it("restores the selected meeting anchor after delayed list hydration", async () => {
    const { wrapper, router } = await render(true);
    let mounts = 0;
    router.removeRoute("captures");
    router.addRoute({ path: "/captures", name: "captures", component: defineComponent({
      setup() {
        const ready = ref(false);
        const row = ref<HTMLElement>();
        const mount = ++mounts;
        onMounted(() => setTimeout(async () => {
          ready.value = true;
          await flushPromises();
          if (row.value) row.value.getBoundingClientRect = () => ({ top: (mount === 1 ? 150 : 360) - wrapper.get("main").element.scrollTop } as DOMRect);
        }, 80));
        return { ready, row };
      },
      template: '<p>Meetings fixture</p><div v-if="ready" ref="row" data-capture-row="42">Selected meeting</div>',
    }) });
    router.addRoute({ path: "/captures/:captureId", name: "capture-detail", component: { template: "<p>Detail fixture</p>" } });
    await router.push("/captures");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 200));
    wrapper.get("main").element.scrollTop = 120;
    await router.push("/captures/42");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(wrapper.text()).toContain("Detail fixture");
    await router.push("/captures");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 300));
    expect(wrapper.text()).toContain("Selected meeting");
    expect(wrapper.get("main").element.scrollTop).toBe(330);
    wrapper.unmount();
  });

  it.each([false, true])("keeps one destination body through rapid navigation (cold=%s)", async (cold) => {
    const { wrapper, router } = await render(cold);
    if (!cold) await wrapper.get("footer button").trigger("click");
    await flushPromises();
    const surface = wrapper.get('[data-testid="app-entrance-surface"]').element;
    for (const path of ["/history", "/captures", "/dictionary", "/settings", "/"]) {
      await router.push(path);
      await flushPromises();
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(wrapper.text()).toContain("Home content");
    expect(wrapper.text()).not.toMatch(/history content|captures content|dictionary content|Settings content/);
    expect(wrapper.findAll('[class*="page-fade-leave"]')).toHaveLength(0);
    expect(wrapper.get('[data-testid="app-entrance-surface"]').element).toBe(surface);
    wrapper.unmount();
  });

  it("keeps the app shell unmounted until completion, then focuses its page heading", async () => {
    const { wrapper } = await render();
    expect(wrapper.findComponent(AppEntrance).exists()).toBe(false);
    expect(wrapper.find("app-sidebar-stub").exists()).toBe(false);
    expect(document.activeElement?.textContent).toBe("Ready");
    await wrapper.get("footer button").trigger("click");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
    expect(wrapper.find("app-sidebar-stub").exists()).toBe(true);
    expect(document.activeElement).toBe(wrapper.get("header h1").element);
    expect(document.activeElement?.textContent).toBe("Home");
    expect(wrapper.getComponent(AppEntrance).props("fromOnboarding")).toBe(true);
    wrapper.unmount();
  });

  it("selects the cold-launch entrance for completed installs and keeps it mounted across navigation", async () => {
    const { wrapper, router } = await render(true);
    const entrance = wrapper.getComponent(AppEntrance);
    expect(entrance.props("fromOnboarding")).toBe(false);
    const surface = entrance.get('[data-testid="app-entrance-surface"]').element;
    await router.push("/settings");
    await flushPromises();
    await new Promise((resolve) => setTimeout(resolve, 200));
    expect(wrapper.text()).toContain("Settings content");
    expect(wrapper.text()).not.toContain("Home content");
    expect(wrapper.get('[data-testid="app-entrance-surface"]').element).toBe(surface);
    wrapper.unmount();
  });
});
