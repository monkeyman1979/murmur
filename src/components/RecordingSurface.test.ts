import { flushPromises, mount } from "@vue/test-utils";
import { h, nextTick } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import Surface from "./RecordingSurface.vue";

type Presentation = { style: "floating" | "notch"; expanded: boolean; compact_width: number; notch_width: number; notch_height: number; revision?: number; fresh?: boolean; expanded_width?: number; expanded_height?: number };
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(), listen: vi.fn(), cleanup: vi.fn(),
  callback: undefined as undefined | ((e: { payload: Presentation }) => void),
  hover: undefined as undefined | ((e: { payload: boolean }) => void),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ label: "indicator" }) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

function mountSurface(props: Record<string, unknown> = {}) {
  return mount(Surface, {
    props,
    slots: { default: '<button class="stop">Stop recording</button>', compact: "Recording · 00:02", leading: "Mic", trailing: ({ expanded }: { expanded: boolean }) => h("span", { class: "header-mode", "data-expanded": String(expanded) }, "Activity") },
    global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
  });
}
const notch = { style: "notch", expanded: false, compact_width: 329, notch_width: 185, notch_height: 32, expanded_width: 400, expanded_height: 176, revision: 1, fresh: true } as const;

beforeEach(() => {
  mocks.cleanup.mockReset();
  mocks.callback = undefined;
  mocks.hover = undefined;
  mocks.listen.mockReset().mockImplementation(async (name, callback) => {
    if (name === "murmur://indicator-hover") mocks.hover = callback;
    else mocks.callback = callback;
    return mocks.cleanup;
  });
  let revision = 1;
  mocks.invoke.mockReset().mockImplementation(async (command, args) => {
    if (command === "get_indicator_presentation") return notch;
    if (command === "set_indicator_expanded") mocks.callback?.({ payload: { ...notch, expanded: args.expanded, fresh: false, revision: ++revision } });
    return undefined;
  });
});

describe("recording notch surface", () => {
  it("reserves the measured camera area and confines content to separate wings", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    const panel = wrapper.get(".notch-surface");
    expect(panel.attributes("style")).toContain("--camera-width: 185px");
    expect(panel.attributes("style")).toContain("--camera-height: 32px");
    expect(panel.attributes("style")).toContain("--header-width: 400px");
    expect(panel.attributes("style")).toContain("--expanded-body-height: 144px");
    expect(panel.find('[role="status"]').exists()).toBe(false);
    expect(wrapper.get('[role="status"]').text()).toContain("Recording");
    expect(panel.attributes("style")).toContain("--arrival-width: 281px");
    expect(wrapper.get(".notch-surface__camera").text()).toBe("");
    expect(wrapper.findAll(".notch-surface__wing").map(w => w.text())).toEqual(["Mic", "Activity"]);
    wrapper.unmount();
  });

  it("widens the surface without widening its camera column or replaying arrival on resize", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    const original = wrapper.get(".notch-surface").element;
    expect(wrapper.get(".notch-surface").attributes("style")).toContain("--surface-width: 329px");
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(wrapper.get(".notch-surface").element).toBe(original);
    expect(wrapper.get(".notch-surface").attributes("style")).toContain("--surface-width: 400px");
    expect(wrapper.get(".notch-surface").attributes("style")).toContain("--header-width: 400px");
    expect(wrapper.get(".notch-surface").attributes("style")).toContain("--camera-width: 185px");
    expect(wrapper.get(".notch-surface__camera").text()).toBe("");
    mocks.callback?.({ payload: { ...notch, revision: 8, fresh: true } });
    await flushPromises();
    expect(wrapper.get(".notch-surface").element).not.toBe(original);
    wrapper.unmount();
  });

  it("renders the laptop mirror as passive status without expansion authority", async () => {
    const wrapper = mountSurface();
    await wrapper.setProps({ interactive: false });
    await flushPromises();
    expect(wrapper.find("button").exists()).toBe(false);
    await wrapper.get(".notch-surface__handle").trigger("click");
    await wrapper.get(".notch-surface").trigger("wheel", { deltaY: 50 });
    await flushPromises();
    expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith("get_indicator_presentation");
    expect(wrapper.find(".stop").exists()).toBe(false);
    wrapper.unmount();
  });

  it("preserves the floating controls without a notch toggle on the fallback display", async () => {
    mocks.invoke.mockResolvedValue({ ...notch, style: "floating" });
    const wrapper = mountSurface();
    await flushPromises();
    expect(wrapper.find(".stop").exists()).toBe(true);
    expect(wrapper.find(".notch-surface").exists()).toBe(false);
    expect(mocks.invoke).toHaveBeenCalledExactlyOnceWith("get_indicator_presentation");
    wrapper.unmount();
    expect(mocks.cleanup).toHaveBeenCalledOnce();
  });

  it("uses click to expose controls, and Escape to collapse without a recorder command", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    expect(wrapper.find(".stop").exists()).toBe(false);
    expect(wrapper.get(".notch-surface__handle").attributes("aria-expanded")).toBe("false");
    await wrapper.get(".notch-surface__handle").trigger("mouseenter");
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(mocks.invoke).toHaveBeenLastCalledWith("set_indicator_expanded", { expanded: true });
    expect(wrapper.find(".stop").exists()).toBe(true);
    await wrapper.get(".stop").trigger("click");
    expect(mocks.invoke).toHaveBeenCalledTimes(2);
    await wrapper.get(".notch-surface").trigger("keydown", { key: "Escape" });
    await flushPromises();
    expect(mocks.invoke).toHaveBeenLastCalledWith("set_indicator_expanded", { expanded: false });
    expect(wrapper.find(".stop").exists()).toBe(false);
    wrapper.unmount();
  });

  it("keeps side content steady until the rendered collapse finishes, then acknowledges its revision", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(wrapper.get(".header-mode").attributes("data-expanded")).toBe("true");
    expect(mocks.invoke).not.toHaveBeenCalledWith("finish_indicator_collapse", expect.anything());
    const panel = wrapper.get(".notch-surface");
    vi.spyOn(panel.element, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 329, 32));
    const ended = new Event("transitionend", { bubbles: true });
    Object.defineProperty(ended, "propertyName", { value: "width" });
    panel.element.dispatchEvent(ended);
    await flushPromises();
    expect(wrapper.get(".header-mode").attributes("data-expanded")).toBe("false");
    expect(mocks.invoke).toHaveBeenLastCalledWith("finish_indicator_collapse", { revision: 3 });
    wrapper.unmount();
  });

  it("keeps the same visible side content throughout collapse and native settlement", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    const wrapper = mountSurface();
    const paint = async () => { frames.splice(0).forEach(callback => callback(0)); await flushPromises(); };
    try {
      await wrapper.setProps({ syncCollapsePaint: true });
      await flushPromises();
      const indicators = wrapper.findAll(".notch-surface__wing").map(wing => wing.element);
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      const panel = wrapper.get(".notch-surface");
      const expectContinuous = () => {
        expect(panel.classes()).not.toContain("notch-surface--side-settling");
        expect(wrapper.findAll(".notch-surface__wing").map(wing => wing.element)).toEqual(indicators);
      };
      expectContinuous();
      vi.spyOn(panel.element, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 329, 32));
      const ended = new Event("transitionend", { bubbles: true });
      Object.defineProperty(ended, "propertyName", { value: "width" });
      panel.element.dispatchEvent(ended);
      await flushPromises();
      expectContinuous();
      expect(mocks.invoke).not.toHaveBeenCalledWith("finish_indicator_collapse", expect.anything());
      await paint();
      expect(mocks.invoke).not.toHaveBeenCalledWith("finish_indicator_collapse", expect.anything());
      await paint();
      expect(mocks.invoke).toHaveBeenCalledWith("finish_indicator_collapse", { revision: 3 });
      expectContinuous();
      await paint(); await paint();
      await vi.advanceTimersByTimeAsync(1000);
      expectContinuous();
      expect(mocks.invoke.mock.calls.filter(call => call[0] === "finish_indicator_collapse")).toHaveLength(1);
    } finally { wrapper.unmount(); vi.unstubAllGlobals(); vi.useRealTimers(); }
  });

  it("does not let a late native resize acknowledgement disturb a reopened meeting", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    const originalInvoke = mocks.invoke.getMockImplementation()!;
    let completeResize!: () => void;
    mocks.invoke.mockImplementation((command, args) => command === "finish_indicator_collapse"
      ? new Promise<void>(resolve => { completeResize = resolve; }) : originalInvoke(command, args));
    const wrapper = mountSurface();
    const paint = async () => { frames.splice(0).forEach(callback => callback(0)); await flushPromises(); };
    try {
      await wrapper.setProps({ syncCollapsePaint: true });
      await flushPromises();
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      const panel = wrapper.get(".notch-surface");
      vi.spyOn(panel.element, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 329, 32));
      const ended = new Event("transitionend", { bubbles: true });
      Object.defineProperty(ended, "propertyName", { value: "width" });
      panel.element.dispatchEvent(ended);
      await flushPromises();
      await paint(); await paint();
      expect(completeResize).toBeTypeOf("function");
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      completeResize();
      await flushPromises();
      expect(panel.classes()).toContain("notch-surface--expanded");
      expect(panel.classes()).not.toContain("notch-surface--side-settling");
      expect(wrapper.get(".notch-surface__handle").attributes("aria-expanded")).toBe("true");
    } finally { wrapper.unmount(); vi.unstubAllGlobals(); vi.useRealTimers(); }
  });

  it("never subscribes to native hover unless the surface opts into the cue", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    expect(mocks.listen).not.toHaveBeenCalledWith("murmur://indicator-hover", expect.anything(), expect.anything());
    expect(mocks.hover).toBeUndefined();
    wrapper.unmount();
  });

  it("grows on native hover even when CSS :hover is unavailable, but not while expanded or passive", async () => {
    const wrapper = mountSurface({ hoverCue: true });
    await flushPromises();
    const panel = wrapper.get(".notch-surface");
    expect(panel.classes()).not.toContain("notch-surface--hovered");
    mocks.hover?.({ payload: true });
    await nextTick();
    expect(panel.classes()).toContain("notch-surface--hovered");
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(panel.classes()).not.toContain("notch-surface--hovered");
    await wrapper.get(".notch-surface").trigger("keydown", { key: "Escape" });
    await flushPromises();
    await wrapper.setProps({ interactive: false });
    expect(wrapper.get(".notch-surface").classes()).not.toContain("notch-surface--hovered");
    mocks.hover?.({ payload: false });
    await wrapper.setProps({ interactive: true });
    expect(wrapper.get(".notch-surface").classes()).not.toContain("notch-surface--hovered");
    wrapper.unmount();
    expect(mocks.cleanup).toHaveBeenCalledTimes(2);
  });

  it("settles the compact hover size without waiting for the fallback timer", async () => {
    const wrapper = mountSurface({ hoverCue: true });
    await flushPromises();
    mocks.hover?.({ payload: true });
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    await wrapper.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    const panel = wrapper.get(".notch-surface");
    vi.spyOn(panel.element, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 339, 35));
    const ended = new Event("transitionend", { bubbles: true });
    Object.defineProperty(ended, "propertyName", { value: "width" });
    panel.element.dispatchEvent(ended);
    await flushPromises();
    expect(mocks.invoke).toHaveBeenLastCalledWith("finish_indicator_collapse", { revision: 3 });
    wrapper.unmount();
  });

  it("does not fade side content when Reduce Motion is enabled", async () => {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    vi.stubGlobal("matchMedia", () => ({ matches: true }));
    const wrapper = mountSurface();
    const paint = async () => { frames.splice(0).forEach(callback => callback(0)); await flushPromises(); };
    try {
      await wrapper.setProps({ syncCollapsePaint: true });
      await flushPromises();
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
      expect(wrapper.get(".notch-surface").classes()).not.toContain("notch-surface--side-settling");
      await paint(); await paint();
      expect(mocks.invoke).toHaveBeenCalledWith("finish_indicator_collapse", { revision: 3 });
      await paint(); await paint();
      expect(wrapper.get(".notch-surface").classes()).not.toContain("notch-surface--side-settling");
    } finally { wrapper.unmount(); vi.unstubAllGlobals(); }
  });

  it("does not acknowledge a stale collapse after reopening", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    for (let step = 0; step < 3; step++) {
      await wrapper.get(".notch-surface__handle").trigger("click");
      await flushPromises();
    }
    const ended = new Event("transitionend", { bubbles: true });
    Object.defineProperty(ended, "propertyName", { value: "width" });
    wrapper.get(".notch-surface").element.dispatchEvent(ended);
    await flushPromises();
    expect(mocks.invoke).not.toHaveBeenCalledWith("finish_indicator_collapse", expect.anything());
    expect(wrapper.get(".notch-surface__handle").attributes("aria-expanded")).toBe("true");
    wrapper.unmount();
  });

  it("never exposes actions in a passive mirror, even before notch geometry hydrates", async () => {
    mocks.invoke.mockResolvedValue({ ...notch, style: "floating" });
    const wrapper = mountSurface();
    await wrapper.setProps({ interactive: false });
    await flushPromises();
    expect(wrapper.find("button").exists()).toBe(false);
    mocks.callback?.({ payload: { ...notch, expanded: true } });
    await flushPromises();
    expect(wrapper.find(".stop").exists()).toBe(false);
    expect(wrapper.find("button").exists()).toBe(false);
    wrapper.unmount();
  });

  it("only responds to deliberate vertical scroll over the panel", async () => {
    const wrapper = mountSurface();
    await flushPromises();
    const panel = wrapper.get(".notch-surface");
    const horizontal = new WheelEvent("wheel", { deltaX: 50, deltaY: 2, bubbles: true, cancelable: true });
    const modified = new WheelEvent("wheel", { deltaY: 40, ctrlKey: true, bubbles: true, cancelable: true });
    panel.element.dispatchEvent(horizontal);
    panel.element.dispatchEvent(modified);
    await nextTick();
    expect(horizontal.defaultPrevented).toBe(false);
    expect(modified.defaultPrevented).toBe(false);
    await panel.trigger("wheel", { deltaY: 1 });
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    const vertical = new WheelEvent("wheel", { deltaY: 30, bubbles: true, cancelable: true });
    panel.element.dispatchEvent(vertical);
    await flushPromises();
    expect(vertical.defaultPrevented).toBe(true);
    expect(mocks.invoke).toHaveBeenLastCalledWith("set_indicator_expanded", { expanded: true });
    await panel.trigger("wheel", { deltaY: -30 });
    await flushPromises();
    expect(mocks.invoke).toHaveBeenLastCalledWith("set_indicator_expanded", { expanded: false });
    wrapper.unmount();
  });

  it("does not let a late hydration snapshot overwrite a newer capture presentation", async () => {
    let resolve!: (p: Presentation) => void;
    mocks.invoke.mockImplementation(() => new Promise<Presentation>(r => { resolve = r; }));
    const wrapper = mountSurface();
    await flushPromises();
    mocks.callback?.({ payload: { ...notch, expanded: true } });
    await nextTick();
    resolve({ ...notch, style: "floating" });
    await flushPromises();
    expect(wrapper.find(".notch-surface").exists()).toBe(true);
    expect(wrapper.find(".stop").exists()).toBe(true);
    // A new capture resets the expanded state, rather than inheriting it.
    mocks.callback?.({ payload: notch });
    await nextTick();
    expect(wrapper.find(".stop").exists()).toBe(false);
    wrapper.unmount();
  });

  it("unlistens if registration completes after the component unmounts", async () => {
    let finish!: (cleanup: () => void) => void;
    mocks.listen.mockReturnValue(new Promise(r => { finish = r; }));
    const wrapper = mountSurface();
    wrapper.unmount();
    finish(mocks.cleanup);
    await flushPromises();
    expect(mocks.cleanup).toHaveBeenCalledOnce();
    expect(mocks.invoke).not.toHaveBeenCalled();
  });
});
