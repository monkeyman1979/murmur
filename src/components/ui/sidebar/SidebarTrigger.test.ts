import { mount } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { defineComponent, nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TOOLTIP_HOVER_DELAY_MS } from "@/components/ui/tooltip/timing";
import { SidebarMenuButton, SidebarProvider, SidebarTrigger } from "@/components/ui/sidebar";
import en from "@/locales/en.json";
import ru from "@/locales/ru.json";

const Fixture = defineComponent({
  components: { SidebarProvider, SidebarTrigger },
  template: "<SidebarProvider><SidebarTrigger /></SidebarProvider>",
});

const RailFixture = defineComponent({
  components: { SidebarProvider, SidebarMenuButton },
  template: '<SidebarProvider :default-open="false"><SidebarMenuButton tooltip="Home">Home</SidebarMenuButton><SidebarMenuButton tooltip="History">History</SidebarMenuButton></SidebarProvider>',
});

describe("sidebar collapse control", () => {
  afterEach(() => vi.useRealTimers());

  it("uses sidebar foreground, including hover, and follows App language", async () => {
    const i18n = createI18n({ legacy: false, locale: "en", messages: { en, ru } });
    const wrapper = mount(Fixture, { global: { plugins: [i18n] } });
    const button = wrapper.get('[data-sidebar="trigger"]');
    expect(button.classes()).toContain("text-sidebar-foreground");
    expect(button.classes()).toContain("hover:text-sidebar-foreground");
    expect(button.classes()).toContain("hover:bg-sidebar-hover");
    expect(button.classes()).toContain("dark:hover:bg-sidebar-hover");
    expect(button.classes()).not.toContain("hover:bg-muted");
    expect(button.classes()).not.toContain("dark:hover:bg-muted/50");
    expect(button.attributes("aria-label")).toBe("Toggle sidebar");
    i18n.global.locale.value = "ru";
    await nextTick();
    expect(button.attributes("aria-label")).toBe("Показать или скрыть боковую панель");
    wrapper.unmount();
  });

  it("waits for a sustained pointer hover, but opens immediately on keyboard focus", async () => {
    vi.useFakeTimers();
    const i18n = createI18n({ legacy: false, locale: "en", messages: { en } });
    const wrapper = mount(Fixture, {
      global: { plugins: [i18n], stubs: { TooltipContent: true } },
    });
    const button = wrapper.get('[data-sidebar="trigger"]');

    await button.trigger("pointermove", { pointerType: "mouse" });
    await vi.advanceTimersByTimeAsync(TOOLTIP_HOVER_DELAY_MS - 1);
    expect(button.attributes("data-state")).toBe("closed");
    await vi.advanceTimersByTimeAsync(1);
    expect(button.attributes("data-state")).toBe("delayed-open");

    await button.trigger("blur");
    await button.trigger("focus");
    expect(button.attributes("data-state")).toBe("instant-open");
    wrapper.unmount();
  });

  it("delays rail labels even when moving between neighboring icons", async () => {
    vi.useFakeTimers();
    const wrapper = mount(RailFixture, { global: { stubs: { TooltipContent: true } } });
    const [home, history] = wrapper.findAll('[data-sidebar="menu-button"]');

    await home.trigger("pointermove", { pointerType: "mouse" });
    await vi.advanceTimersByTimeAsync(TOOLTIP_HOVER_DELAY_MS);
    expect(home.attributes("data-state")).toBe("delayed-open");
    await home.trigger("blur");
    await vi.advanceTimersByTimeAsync(1);
    await history.trigger("pointermove", { pointerType: "mouse" });
    expect(history.attributes("data-state")).toBe("closed");
    await vi.advanceTimersByTimeAsync(TOOLTIP_HOVER_DELAY_MS);
    expect(history.attributes("data-state")).toBe("delayed-open");
    wrapper.unmount();
  });
});
