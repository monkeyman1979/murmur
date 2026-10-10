import "@/test/installEnglishI18n";
import { flushPromises, shallowMount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { reactive } from "vue";

const mocks = vi.hoisted(() => ({
  rename: vi.fn(),
  syncActiveCapture: vi.fn(),
}));

const renameState = reactive({ loading: false, error: null, success: null });

vi.mock("@/composables/useCaptures", () => ({
  useCaptures: () => ({
    rename: mocks.rename,
    stateFor: () => renameState,
  }),
}));

vi.mock("@/composables/useMeetingCapture", () => ({
  useMeetingCapture: () => ({ syncActiveCapture: mocks.syncActiveCapture }),
}));

import MeetingTitleEditor from "./MeetingTitleEditor.vue";

const renamedSummary = {
  id: 42,
  title: "Planning review",
  status: "completed",
};

function mountEditor() {
  return shallowMount(MeetingTitleEditor, {
    props: {
      captureId: 42,
      title: "Meeting 42",
      to: "/captures/42",
    },
    global: {
      stubs: {
        Button: { template: '<button v-bind="$attrs"><slot /></button>' },
        IconTooltip: {
          props: ["label"],
          template: '<div :data-tooltip="label"><slot /></div>',
        },
        RouterLink: {
          props: ["to"],
          template: '<a :href="to"><slot /></a>',
        },
      },
    },
  });
}

describe("MeetingTitleEditor", () => {
  beforeEach(() => {
    mocks.rename.mockReset();
    mocks.syncActiveCapture.mockReset();
    renameState.loading = false;
    renameState.error = null;
  });

  it("keeps navigation and rename as separate controls", () => {
    const wrapper = mountEditor();

    expect(wrapper.get("a").text()).toBe("Meeting 42");
    expect(wrapper.classes()).toContain("w-fit");
    const renameButton = wrapper.get('button[aria-label="Rename meeting"]');
    expect(renameButton.attributes("variant")).toBe("secondary");
    expect(renameButton.classes()).toEqual(expect.arrayContaining(["cursor-pointer", "shadow-sm", "ring-1"]));
    expect(renameButton.classes()).not.toContain("hover:bg-muted");
    expect(renameButton.attributes("title")).toBeUndefined();
    expect(wrapper.get('[data-tooltip="Rename meeting"]')).toBeTruthy();
    expect(wrapper.find("a button").exists()).toBe(false);
  });

  it("renames in place with Enter and synchronizes an active capture", async () => {
    mocks.rename.mockResolvedValue(renamedSummary);
    const wrapper = mountEditor();

    await wrapper.get('button[aria-label="Rename meeting"]').trigger("click");
    const input = wrapper.get("input");
    const saveButton = wrapper.get('button[aria-label="Save meeting title"]');
    expect(wrapper.classes()).toContain("w-full");
    expect(input.attributes("maxlength")).toBe("160");
    expect(saveButton.attributes("variant")).toBe("secondary");
    expect(saveButton.attributes("aria-keyshortcuts")).toBe("Enter");

    await input.setValue("  Planning review  ");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();

    expect(mocks.rename).toHaveBeenCalledWith(42, "Planning review");
    expect(mocks.syncActiveCapture).toHaveBeenCalledWith(renamedSummary);
    expect(wrapper.find("input").exists()).toBe(false);
    expect(wrapper.emitted("renamed")?.[0]).toEqual([renamedSummary]);
  });

  it("stays editable through transient window focus loss so dictation can paste", async () => {
    mocks.rename.mockResolvedValue(renamedSummary);
    const wrapper = mountEditor();

    await wrapper.get('button[aria-label="Rename meeting"]').trigger("click");
    const input = wrapper.get("input");
    await input.trigger("blur", { relatedTarget: null });

    expect(wrapper.find("input").exists()).toBe(true);
    expect(mocks.rename).not.toHaveBeenCalled();

    await input.setValue("Planning review");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();

    expect(mocks.rename).toHaveBeenCalledWith(42, "Planning review");
  });

  it("rejects an empty title and cancels with Escape", async () => {
    const wrapper = mountEditor();

    await wrapper.get('button[aria-label="Rename meeting"]').trigger("click");
    await wrapper.get("input").setValue("   ");
    await wrapper.get("input").trigger("keydown", { key: "Enter" });

    expect(mocks.rename).not.toHaveBeenCalled();
    expect(wrapper.get('[role="alert"]').text()).toBe("Enter a meeting title.");

    await wrapper.get("input").trigger("keydown", { key: "Escape" });
    expect(wrapper.find("input").exists()).toBe(false);
    expect(wrapper.get("a").text()).toBe("Meeting 42");
  });
});
