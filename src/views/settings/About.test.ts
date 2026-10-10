import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import About from "./About.vue";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const state = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "0.1.0-pre.1") }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: (...args: unknown[]) => state.openUrl(...args) }));

describe("About external links", () => {
  beforeEach(() => { state.openUrl.mockReset().mockResolvedValue(undefined); });

  it.each([
    ["es", "Informar de un problema"],
    ["fr", "Signaler un problème"],
    ["ru", "Сообщить о проблеме"],
  ])("renders %s interface copy without changing the report URL", (locale, reportLabel) => {
    const wrapper = mount(About, { global: {
      plugins: [createI18n({ legacy: false, locale, messages: { en, es, fr, ru } })],
      stubs: { RouterLink: true, SupportDiagnosticsDialog: true },
    } });
    expect(wrapper.get("a").text()).toBe(reportLabel);
    expect(wrapper.get("a").attributes("href")).toBe("https://github.com/monkeyman1979/murmur/issues/new?template=bug_report.yml");
    wrapper.unmount();
  });

  it("offers one report link, retains opt-in diagnostics, and surfaces opener errors", async () => {
    const wrapper = mount(About, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en, es, fr, ru } })], stubs: { RouterLink: true, SupportDiagnosticsDialog: true } } });
    await flushPromises();
    expect(wrapper.text()).toContain("Source license");
    expect(wrapper.text()).toContain("MIT");
    expect(wrapper.text()).not.toContain("do not redistribute");
    const links = wrapper.findAll("a");
    expect(links).toHaveLength(1);
    expect(wrapper.text()).toContain("Preview diagnostics");
    const report = links.find((link) => link.text() === "Report a problem")!;
    const bugForm = "https://github.com/monkeyman1979/murmur/issues/new?template=bug_report.yml";
    expect(report.attributes("href")).toBe(bugForm);
    state.openUrl.mockRejectedValueOnce(new Error("browser unavailable"));
    await report.trigger("click");
    await flushPromises();
    expect(state.openUrl).toHaveBeenCalledWith(bugForm);
    expect(state.openUrl).toHaveBeenCalledTimes(1);
    expect(wrapper.get('[role="alert"]').text()).toContain("Could not open the report page");
    wrapper.unmount();
  });
});
