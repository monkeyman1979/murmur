import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import { createMemoryHistory, createRouter } from "vue-router";
import { ref, nextTick } from "vue";
import en from "@/locales/en.json";

const activeCapture = ref<{ id: number; status: "recording" | "paused" } | null>(null);
const controls = ref<string[]>([]);
const action = ref<string | null>(null);
const requestStartFlow = vi.fn();
const pause = vi.fn();
const resume = vi.fn();
const stop = vi.fn();
const animationFrames: FrameRequestCallback[] = [];

async function advanceFrame() {
  const callbacks = animationFrames.splice(0);
  callbacks.forEach((callback) => callback(0));
  await nextTick();
}

vi.mock("@/composables/useMeetingCapture", () => ({
  useMeetingCapture: () => ({
    action,
    activeCapture,
    activeDurationMs: ref(12_000),
    controls,
    pause,
    requestStartFlow,
    resume,
    stop,
  }),
}));

import AppSidebar from "@/components/AppSidebar.vue";
import MicAudioLines from "@/components/icons/MicAudioLines.vue";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";

function mountRail() {
  const open = ref(false);
  const router = createRouter({
    history: createMemoryHistory(),
    routes: ["/", "/history", "/captures", "/dictionary", "/settings/general"].map((path) => ({
      path, component: { template: "<div />" },
    })),
  });
  const wrapper = mount({
    components: { SidebarProvider, AppSidebar, SidebarInset },
    setup: () => ({ open }),
    template: '<SidebarProvider :open="open"><AppSidebar collapsible="icon" /><SidebarInset /></SidebarProvider>',
  }, {
    global: {
      plugins: [router, createI18n({ legacy: false, locale: "en", messages: { en } })],
      stubs: { UpdateCard: true },
    },
  });
  return { wrapper, open };
}

describe("AppSidebar collapsed rail", () => {
  beforeEach(() => {
    activeCapture.value = null;
    controls.value = [];
    action.value = null;
    vi.clearAllMocks();
    animationFrames.length = 0;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      animationFrames.push(callback);
      return animationFrames.length;
    });
  });

  afterEach(() => vi.unstubAllGlobals());

  it("keeps a full-footprint, non-interactive brand and a distinct record action", async () => {
    const { wrapper } = mountRail();
    await nextTick();
    expect(wrapper.get('[data-slot="sidebar"][data-state="collapsed"]').attributes("data-collapsible")).toBe("icon");
    expect(wrapper.get('[role="img"][aria-label="Murmur"] .murmur-brand-mark').classes()).toContain("group-data-[collapsible=icon]:size-8");
    expect(wrapper.get('[role="img"][aria-label="Murmur"]').element.closest("button, a")).toBeNull();
    expect(wrapper.get('[role="separator"]').element.compareDocumentPosition(wrapper.get('a[href="/"]').element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // The collapsed rail needs less top padding than expanded navigation to balance both sides of the divider.
    expect(wrapper.get('[data-sidebar="content"]').classes()).toContain("pt-3");
    expect(wrapper.get('[data-sidebar="content"]').classes()).toContain("group-data-[collapsible=icon]:pt-2");
    expect(wrapper.get('[data-slot="sidebar-inset"]').classes()).not.toContain("md:peer-data-[variant=inset]:peer-data-[state=collapsed]:ml-2");
    const record = wrapper.get('[aria-label="Record meeting"]');
    expect(record.classes()).toContain("bg-sidebar-primary");
    expect(record.findComponent(MicAudioLines).exists()).toBe(true);
    expect(record.get('svg rect[x="10"][y="9"][width="4"][height="8"]')).toBeDefined();
    expect(record.find(".recording-pulse-dot").exists()).toBe(false);
    expect(wrapper.get("#rail-meeting-controls").classes()).not.toContain("rail-controls-open");
    expect(wrapper.get("#rail-meeting-controls").attributes("inert")).toBeDefined();
    await record.trigger("click");
    expect(requestStartFlow).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it("automatically animates controls out when recording starts, without a second click", async () => {
    const { wrapper, open } = mountRail();
    await nextTick();
    await wrapper.get('[aria-label="Record meeting"]').trigger("click");
    activeCapture.value = { id: 7, status: "recording" };
    controls.value = ["pause", "stop"];
    await nextTick();
    const indicator = wrapper.get('[aria-label="Show meeting controls"]');
    expect(indicator.classes()).toContain("bg-sidebar-primary");
    expect(indicator.find(".recording-pulse-dot").exists()).toBe(true);
    expect(indicator.attributes("aria-expanded")).toBe("false");
    expect(wrapper.get("#rail-meeting-controls").attributes("inert")).toBeDefined();
    await advanceFrame();
    expect(indicator.attributes("aria-expanded")).toBe("false");
    await advanceFrame();
    expect(indicator.attributes("aria-expanded")).toBe("true");
    expect(wrapper.get("#rail-meeting-controls").classes()).toContain("rail-controls-open");
    wrapper.get('[aria-label="Pause meeting recording"]');
    wrapper.get('[aria-label="Stop meeting recording and transcribe"]');
    await indicator.trigger("click");
    expect(indicator.attributes("aria-expanded")).toBe("true");
    expect(stop).not.toHaveBeenCalled();

    activeCapture.value = { id: 7, status: "paused" };
    controls.value = ["resume", "stop"];
    await nextTick();
    expect(indicator.find(".recording-pulse-dot").exists()).toBe(false);
    expect(indicator.find(".bg-amber-500").exists()).toBe(true);
    wrapper.get('[aria-label="Resume meeting recording"]');
    await wrapper.get('[aria-label="Stop meeting recording and transcribe"]').trigger("click");
    expect(stop).toHaveBeenCalledOnce();

    open.value = true;
    await nextTick();
    expect(wrapper.find("#rail-meeting-controls").exists()).toBe(false);
    open.value = false;
    await nextTick();
    expect(wrapper.get('[aria-label="Show meeting controls"]').attributes("aria-expanded")).toBe("false");
    await advanceFrame();
    await advanceFrame();
    expect(wrapper.get('[aria-label="Show meeting controls"]').attributes("aria-expanded")).toBe("true");
    const controlsElement = wrapper.get("#rail-meeting-controls").element;
    const homeElement = wrapper.get('a[href="/"]').element;
    activeCapture.value = null;
    await nextTick();
    wrapper.get('[aria-label="Record meeting"]');
    const retracting = wrapper.get("#rail-meeting-controls");
    expect(retracting.element).toBe(controlsElement);
    expect(retracting.classes()).not.toContain("rail-controls-open");
    expect(retracting.attributes("aria-hidden")).toBe("true");
    expect(retracting.attributes("inert")).toBeDefined();
    expect(wrapper.get('[aria-label="Resume meeting recording"]').attributes("disabled")).toBeDefined();
    expect(wrapper.get('[aria-label="Stop meeting recording and transcribe"]').attributes("disabled")).toBeDefined();
    expect(wrapper.get('a[href="/"]').element).toBe(homeElement);
    wrapper.unmount();
  });

  it("reveals controls when the rail mounts around a meeting already in progress", async () => {
    activeCapture.value = { id: 8, status: "recording" };
    controls.value = ["pause", "stop"];
    const { wrapper } = mountRail();
    await nextTick();
    await advanceFrame();
    await advanceFrame();
    expect(wrapper.get('[aria-label="Show meeting controls"]').attributes("aria-expanded")).toBe("true");
    wrapper.unmount();
  });
});
