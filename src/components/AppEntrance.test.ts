import { mount, type VueWrapper } from "@vue/test-utils";
import { nextTick, ref, type Ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AppEntrance from "./AppEntrance.vue";

const state = vi.hoisted(() => ({ motion: null as unknown as Ref<string> }));
vi.mock("@vueuse/core", () => ({ usePreferredReducedMotion: () => state.motion }));

const originalAnimate = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "animate");
const cancel = vi.fn();
const animate = vi.fn(() => ({ cancel }));
let wrapper: VueWrapper | undefined;

beforeEach(() => {
  state.motion = ref("no-preference");
  vi.clearAllMocks();
  Object.defineProperty(HTMLElement.prototype, "animate", { configurable: true, value: animate });
});
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  if (originalAnimate) Object.defineProperty(HTMLElement.prototype, "animate", originalAnimate);
  else Reflect.deleteProperty(HTMLElement.prototype, "animate");
});

function render(fromOnboarding: boolean) {
  wrapper = mount(AppEntrance, { props: { fromOnboarding }, slots: { default: '<button>Use Murmur</button>' } });
  return wrapper;
}

describe("app entrance", () => {
  it("uses a 300ms fade and 4px rise after onboarding without blocking content", async () => {
    const view = render(true);
    expect(animate).toHaveBeenCalledWith([
      { opacity: 0, transform: "translateY(4px)" },
      { opacity: 1, transform: "translateY(0)" },
    ], { id: "murmur-app-enter", duration: 300, easing: "cubic-bezier(0.2, 0.7, 0.2, 1)" });
    const click = vi.fn();
    view.get("button").element.addEventListener("click", click);
    await view.get("button").trigger("click");
    expect(click).toHaveBeenCalledOnce();
    expect(view.find("[inert], [aria-hidden], [disabled]").exists()).toBe(false);
    expect(view.classes()).toContain("app-shell-background");
    expect(animate.mock.contexts[0]).toBe(view.get('[data-testid="app-entrance-surface"]').element);
  });

  it("uses only a 180ms opacity fade on cold launch", () => {
    render(false);
    expect(animate).toHaveBeenCalledWith([{ opacity: 0 }, { opacity: 1 }], expect.objectContaining({ duration: 180 }));
  });

  it.each([true, false])("skips all entrance motion with reduced motion (onboarding=%s)", (fromOnboarding) => {
    state.motion.value = "reduce";
    expect(render(fromOnboarding).get("button").text()).toBe("Use Murmur");
    expect(animate).not.toHaveBeenCalled();
  });

  it("cancels if reduced motion is enabled and does not replay when disabled", async () => {
    render(true);
    state.motion.value = "reduce";
    await nextTick();
    expect(cancel).toHaveBeenCalledOnce();
    state.motion.value = "no-preference";
    await nextTick();
    expect(animate).toHaveBeenCalledOnce();
  });

  it("does not replay on updates, window focus, or visibility changes", async () => {
    const view = render(true);
    await view.setProps({ fromOnboarding: false });
    window.dispatchEvent(new Event("focus"));
    document.dispatchEvent(new Event("visibilitychange"));
    await nextTick();
    expect(animate).toHaveBeenCalledOnce();
  });

  it("cancels on unmount", () => {
    render(true);
    wrapper!.unmount();
    wrapper = undefined;
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("leaves the app usable if Web Animations are unavailable", () => {
    Reflect.deleteProperty(HTMLElement.prototype, "animate");
    expect(render(true).get("button").text()).toBe("Use Murmur");
    expect(animate).not.toHaveBeenCalled();
  });
});
