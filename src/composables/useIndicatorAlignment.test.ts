import { flushPromises, mount } from "@vue/test-utils";
import { defineComponent, nextTick } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useIndicatorAlignment } from "./useIndicatorAlignment";

const events = vi.hoisted(() => ({
  callback: undefined as undefined | ((event: { payload: string }) => void),
  cleanup: vi.fn(),
  listen: vi.fn(),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ label: "indicator" }) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

const Pill = defineComponent({
  setup() { return { alignment: useIndicatorAlignment() }; },
  template: '<div :class="alignment" />',
});

beforeEach(() => {
  events.callback = undefined;
  events.cleanup.mockReset();
  events.listen.mockReset().mockImplementation(async (_name, callback) => {
    events.callback = callback;
    return events.cleanup;
  });
});

describe("recording pill alignment", () => {
  it.each([
    ["top-left", "justify-start"], ["bottom-left", "justify-start"],
    ["top-center", "justify-center"], ["bottom-center", "justify-center"],
    ["top-right", "justify-end"], ["bottom-right", "justify-end"],
  ])("aligns %s to the visible window edge", async (position, expected) => {
    const wrapper = mount(Pill);
    expect(wrapper.classes()).toContain("justify-center");
    await flushPromises();
    events.callback?.({ payload: position });
    await nextTick();
    expect(wrapper.classes()).toContain(expected);
    wrapper.unmount();
    expect(events.cleanup).toHaveBeenCalledOnce();
  });

  it("ignores invalid position events", async () => {
    const wrapper = mount(Pill);
    await flushPromises();
    events.callback?.({ payload: "invalid" });
    await nextTick();
    expect(wrapper.classes()).toContain("justify-center");
    wrapper.unmount();
  });

  it("cleans up when the listener finishes registering after unmount", async () => {
    let finish!: (cleanup: () => void) => void;
    events.listen.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    const wrapper = mount(Pill);
    wrapper.unmount();
    finish(events.cleanup);
    await flushPromises();
    expect(events.cleanup).toHaveBeenCalledOnce();
  });
});
