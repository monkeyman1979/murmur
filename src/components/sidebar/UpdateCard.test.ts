import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";

const state = vi.hoisted(() => ({
  actions: null as unknown as Record<string, unknown>,
  sidebar: null as unknown as Record<string, unknown>,
}));
vi.mock("@/composables/useUpdateActions", () => ({ useUpdateActions: () => state.actions }));
vi.mock("@/components/ui/sidebar", () => ({ useSidebar: () => state.sidebar }));

import UpdateCard from "./UpdateCard.vue";

const sidebarState = ref("expanded");
const isMobile = ref(false);
const runAction = vi.fn();
const dismissUpdate = vi.fn();
const notes = ref("### Added\n\n- Optional meeting reminders from Apple Calendar.");

function mountCard() {
  return mount(UpdateCard, {
    global: {
      plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })],
      stubs: { IconTooltip: { template: "<div><slot /></div>" } },
    },
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  sidebarState.value = "expanded";
  isMobile.value = false;
  notes.value = "### Added\n\n- Optional meeting reminders from Apple Calendar.";
  state.sidebar = { state: sidebarState, isMobile };
  state.actions = {
    updater: {
      enabled: true,
      sidebarVisible: ref(true),
      phase: ref("available"),
      notes,
      dismissUpdate,
    },
    restartBlocked: ref(false),
    statusLabel: ref("Murmur 0.1.4 available"),
    actionLabel: ref("Update"),
    runAction,
  };
});

describe("UpdateCard", () => {
  it("shows only availability and the update action, without release-note previews", () => {
    const wrapper = mountCard();
    expect(wrapper.get("section").findAll("p").map((paragraph) => paragraph.text())).toEqual(["Murmur 0.1.4 available"]);
    expect(wrapper.findAll("button").map((button) => button.text()).filter(Boolean)).toEqual(["Update"]);
    expect(wrapper.text()).not.toContain("### Added");
    expect(wrapper.text()).not.toContain("Optional meeting reminders");
    wrapper.unmount();
  });

  it("preserves the Update and Later actions", async () => {
    const wrapper = mountCard();
    await wrapper.findAll("button").find((button) => button.text() === "Update")!.trigger("click");
    expect(runAction).toHaveBeenCalledOnce();
    await wrapper.get('button[aria-label="Later"]').trigger("click");
    expect(dismissUpdate).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it("keeps the collapsed sidebar action accessible without release notes", async () => {
    sidebarState.value = "collapsed";
    const wrapper = mountCard();
    expect(wrapper.find("section").exists()).toBe(false);
    await wrapper.get('button[aria-label="Murmur 0.1.4 available"]').trigger("click");
    expect(runAction).toHaveBeenCalledOnce();
    expect(wrapper.text()).not.toContain("Optional meeting reminders");
    wrapper.unmount();
  });
});
