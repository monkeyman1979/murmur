import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ref, type Ref } from "vue";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import OnboardingFrame from "./OnboardingFrame.vue";

const motion = vi.hoisted(() => ({ preference: null as unknown as Ref<"reduce" | "no-preference"> }));
vi.mock("@vueuse/core", async (importOriginal) => ({
  ...await importOriginal<typeof import("@vueuse/core")>(),
  usePreferredReducedMotion: () => motion.preference,
}));
beforeEach(() => { motion.preference = ref("no-preference"); });

const wrappers: ReturnType<typeof mount>[] = [];
const props = { title: "Welcome to Murmur", subtitle: "Private voice capture", stepLabel: "Step 1 of 7", currentStep: 1, totalSteps: 7, primaryLabel: "Get started" };
function render(extra = {}, slots = {}) {
  const wrapper = mount(OnboardingFrame, {
    attachTo: document.body,
    props: { ...props, ...extra },
    slots,
    global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
  });
  wrappers.push(wrapper);
  return wrapper;
}

afterEach(async () => {
  wrappers.forEach((wrapper) => wrapper.unmount());
  wrappers.length = 0;
  await flushPromises();
  document.body.innerHTML = "";
  localStorage.clear();
});

function observeMotion(wrapper: ReturnType<typeof render>) {
  const animations: { cancel: ReturnType<typeof vi.fn> }[] = [];
  const animate = vi.fn(() => {
    const animation = { cancel: vi.fn() };
    animations.push(animation);
    return animation as unknown as Animation;
  });
  for (const section of wrapper.findAll(".onboarding-content > section")) {
    Object.defineProperty(section.element, "animate", { value: animate, configurable: true });
  }
  return { animate, animations };
}

describe("OnboardingFrame", () => {
  it("fades/slides only the content on navigation and cancels motion on rapid Back or unmount", async () => {
    const wrapper = render(); await flushPromises();
    const { animate, animations } = observeMotion(wrapper);
    const header = wrapper.get("header").element;
    const footer = wrapper.get("footer").element;
    await wrapper.setProps({ currentStep: 2, title: "Overview" });
    expect(animate).toHaveBeenCalledTimes(2);
    expect(animate).toHaveBeenCalledWith([
      { opacity: 0, transform: "translateY(8px)" },
      { opacity: 1, transform: "translateY(0)" },
    ], { id: "onboarding-screen-enter", duration: 350, easing: "cubic-bezier(0.4, 0, 0.2, 1)" });
    expect(wrapper.get("header").element).toBe(header);
    expect(wrapper.get("footer").element).toBe(footer);
    expect(document.activeElement).toBe(wrapper.get("h1").element);
    await wrapper.setProps({ currentStep: 1 });
    expect(animate).toHaveBeenCalledTimes(4);
    expect(animations[0].cancel).toHaveBeenCalledOnce();
    expect(animations[1].cancel).toHaveBeenCalledOnce();
    wrapper.unmount();
    expect(animations[2].cancel).toHaveBeenCalledOnce();
    expect(animations[3].cancel).toHaveBeenCalledOnce();
  });

  it("animates gesture changes without remounting the demo or reacting to status/copy updates", async () => {
    const wrapper = render({ currentStep: 6, focusDemo: true, motionKey: "hold" }, { right: '<textarea aria-label="Demo"></textarea>' });
    await flushPromises();
    const { animate } = observeMotion(wrapper);
    const input = wrapper.get("textarea").element as HTMLTextAreaElement;
    input.focus(); input.value = "Still mounted";
    await wrapper.setProps({ motionKey: "toggle" });
    expect(animate).toHaveBeenCalledTimes(2);
    expect(wrapper.get("textarea").element).toBe(input);
    expect(input.value).toBe("Still mounted");
    expect(document.activeElement).toBe(input);
    await wrapper.setProps({ primaryDisabled: true, subtitle: "Updated status" });
    expect(animate).toHaveBeenCalledTimes(2);
  });

  it("skips reduced motion and cancels an active animation when the preference changes", async () => {
    motion.preference.value = "reduce";
    const wrapper = render(); await flushPromises();
    const { animate, animations } = observeMotion(wrapper);
    await wrapper.setProps({ currentStep: 2 });
    expect(animate).not.toHaveBeenCalled();
    motion.preference.value = "no-preference";
    await wrapper.setProps({ currentStep: 3 });
    expect(animate).toHaveBeenCalledTimes(2);
    motion.preference.value = "reduce"; await flushPromises();
    expect(animations.every((animation) => animation.cancel.mock.calls.length === 1)).toBe(true);
    await wrapper.setProps({ currentStep: 4 });
    expect(animate).toHaveBeenCalledTimes(2);
  });
  it("scopes dark appearance without mutating theme or gradient preferences", () => {
    localStorage.setItem("murmur:theme", "light");
    localStorage.setItem("murmur:shell-gradient", "desert");
    document.documentElement.classList.remove("dark");
    const wrapper = render();
    expect(wrapper.get('[role="dialog"]').classes()).toContain("dark");
    expect(document.documentElement.classList.contains("dark")).toBe(false);
    expect(localStorage.getItem("murmur:theme")).toBe("light");
    expect(localStorage.getItem("murmur:shell-gradient")).toBe("desert");
  });

  it.each([1, 7])("uses the shared bookend identity and direct logo/title/subtext stack on step %s", (currentStep) => {
    const wrapper = render({ currentStep, centered: true });
    const stack = wrapper.get(".onboarding-centered > section:first-child");
    expect(stack.get(".onboarding-brand-mark").classes()).toContain("size-36");
    expect(Array.from(stack.element.children, (element) => element.tagName)).toEqual(["SPAN", "H1", "P"]);
    expect(stack.get("h1").classes()).toContain("text-[clamp(2.25rem,calc(1.5rem+2.5vw),4rem)]");
    expect(stack.classes()).toEqual(expect.arrayContaining(["items-center", "text-center"]));
    expect(wrapper.get("header").find('[role="img"]').exists()).toBe(false);
  });

  it("centers only Welcome within the scrollable main area, not Ready or Overview", async () => {
    const wrapper = render({ centered: true, centerWelcome: true });
    const main = wrapper.get("main");
    expect(main.classes()).toContain("overflow-y-auto");
    expect(wrapper.get(".onboarding-content").classes()).toContain("onboarding-welcome");
    await wrapper.setProps({ currentStep: 2, centered: false, centerWelcome: false });
    expect(wrapper.get(".onboarding-content").classes()).not.toContain("onboarding-welcome");
    await wrapper.setProps({ currentStep: 7, centered: true });
    expect(wrapper.get(".onboarding-content").classes()).not.toContain("onboarding-welcome");
  });

  it("omits repeated header branding on Welcome and Ready while preserving the header space", async () => {
    const wrapper = render();
    const header = wrapper.get("header");
    expect(header.find('[role="img"]').exists()).toBe(false);
    await wrapper.setProps({ currentStep: 2 });
    expect(header.text()).toBe("Murmur");
    expect(header.find('[role="img"]').exists()).toBe(true);
    await wrapper.setProps({ currentStep: 7, centered: true });
    expect(wrapper.get("header").element).toBe(header.element);
    expect(header.text()).toBe("");
    expect(header.find('[role="img"]').exists()).toBe(false);
  });

  it("labels the non-dismissible dialog and seven bottom progress segments", async () => {
    const wrapper = render();
    const dialog = wrapper.get('[role="dialog"]');
    expect(dialog.attributes("aria-modal")).toBe("true");
    expect(dialog.attributes("aria-labelledby")).toBe(wrapper.get("h1").attributes("id"));
    const progress = wrapper.get('footer [role="progressbar"]');
    expect(progress.attributes("aria-valuenow")).toBe("1");
    expect(progress.attributes("aria-valuemax")).toBe("7");
    expect(progress.findAll(".progress-track")).toHaveLength(7);
    expect(wrapper.find('[aria-label="Back"]').exists()).toBe(false);
    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    dialog.element.dispatchEvent(escape);
    expect(escape.defaultPrevented).toBe(true);
    await dialog.trigger("click");
    expect(wrapper.emitted("primary")).toBeUndefined();
    expect(wrapper.emitted("back")).toBeUndefined();
  });

  it("focuses the heading on entry and navigation; contains focus and loops Tab", async () => {
    const outside = document.createElement("button");
    document.body.appendChild(outside);
    const wrapper = render({ showBack: true, currentStep: 2 });
    await flushPromises();
    expect(document.activeElement).toBe(wrapper.get("h1").element);
    const back = wrapper.get('[aria-label="Back"]');
    const primary = wrapper.findAll("footer button")[1];
    (back.element as HTMLButtonElement).focus();
    outside.focus();
    expect(document.activeElement).toBe(back.element);
    await back.trigger("keydown", { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(primary.element);
    await primary.trigger("keydown", { key: "Tab" });
    expect(document.activeElement).toBe(back.element);
    await wrapper.setProps({ currentStep: 3, title: "Permissions" });
    expect(document.activeElement).toBe(wrapper.get("h1").element);
  });

  it("uses one compact subtitle line height across multi-line steps without changing font size", async () => {
    const wrapper = render({ title: "Permissions", subtitle: en.onboarding.steps.permissions.subtitle });
    const subtitle = wrapper.get(".onboarding-split section:first-child > p");
    expect(subtitle.classes()).toEqual(expect.arrayContaining(["text-base", "leading-6", "text-balance"]));
    await wrapper.setProps({ title: "Download", subtitle: en.onboarding.steps.requiredModel.subtitle });
    expect(subtitle.classes()).toContain("leading-6");
  });

  it("sizes the primary action to translated content without losing bounded wrapping", async () => {
    const wrapper = render();
    const primary = wrapper.get('[data-testid="onboarding-primary"]');
    for (const catalog of [en, es, fr, ru]) {
      for (const primaryLabel of [catalog.onboarding.cta.grantRequired, catalog.onboarding.cta.connectMicrophone, catalog.onboarding.cta.continue]) {
        await wrapper.setProps({ primaryLabel });
        expect(primary.text()).toBe(primaryLabel);
        expect(primary.classes()).toEqual(expect.arrayContaining(["w-fit", "max-w-full", "min-h-12", "whitespace-normal"]));
        expect(primary.classes()).not.toContain("w-72");
        expect(primary.classes()).not.toContain("transition-all");
        expect(primary.classes()).toContain("transition-[background-color,color,border-color,box-shadow,opacity]");
      }
    }
  });

  it("emits navigation only from enabled actions and preserves demo-owned focus", async () => {
    const wrapper = render({ showBack: true, primaryDisabled: true });
    await flushPromises();
    const buttons = wrapper.findAll("footer button");
    await buttons[1].trigger("click");
    expect(wrapper.emitted("primary")).toBeUndefined();
    await buttons[0].trigger("click");
    expect(wrapper.emitted("back")).toHaveLength(1);
    await wrapper.setProps({ primaryDisabled: false });
    await buttons[1].trigger("click");
    expect(wrapper.emitted("primary")).toHaveLength(1);
    (buttons[1].element as HTMLButtonElement).focus();
    await wrapper.setProps({ currentStep: 6, focusDemo: true });
    expect(document.activeElement).toBe(buttons[1].element);
  });
});
