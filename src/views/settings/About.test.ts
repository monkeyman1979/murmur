import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import About from "./About.vue";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const state = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "0.1.0") }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: (...args: unknown[]) => state.openUrl(...args) }));
const updates = vi.hoisted(() => ({
  enabled: true,
  phase: null as unknown as import("vue").Ref<string>,
  progress: null as unknown as import("vue").Ref<number | null>,
  status: null as unknown as import("vue").Ref<string>,
  label: null as unknown as import("vue").Ref<string | null>,
  disabled: null as unknown as import("vue").Ref<boolean>,
  run: vi.fn(),
}));
vi.mock("@/composables/useUpdateActions", () => ({
  useUpdateActions: () => ({
    updater: { enabled: updates.enabled, phase: updates.phase, progress: updates.progress },
    settingsStatus: updates.status,
    settingsActionLabel: updates.label,
    settingsActionDisabled: updates.disabled,
    runSettingsAction: updates.run,
  }),
}));
beforeEach(() => {
  updates.enabled = true;
  updates.phase = ref("idle");
  updates.progress = ref(null);
  updates.status = ref("Murmur checks at launch at most once per day.");
  updates.label = ref("Check for updates");
  updates.disabled = ref(false);
  updates.run.mockReset();
});

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
    expect(wrapper.text()).not.toContain("GitHub Releases");
    wrapper.unmount();
  });

  it("offers one report link, retains opt-in diagnostics, and surfaces opener errors", async () => {
    const wrapper = mount(About, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en, es, fr, ru } })], stubs: { RouterLink: true, SupportDiagnosticsDialog: true } } });
    await flushPromises();
    expect(wrapper.get("dl dd").text()).toBe("0.1.0");
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

describe("About software updates", () => {
  const mountAbout = () => mount(About, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })], stubs: { RouterLink: true, SupportDiagnosticsDialog: true } } });

  it("checks and updates from below the version, independent of the sidebar", async () => {
    const wrapper = mountAbout();
    await flushPromises();
    const row = wrapper.get('[data-test="about-updates"]');
    expect(row.text()).toContain("Software updates");
    expect(wrapper.text()).not.toContain("Automatic updates are off");
    expect(wrapper.text()).not.toContain("GitHub Releases");
    expect(wrapper.text()).toContain("update checks and downloads");
    expect(row.text()).toContain("Murmur checks at launch at most once per day.");
    const dl = wrapper.get("dl").element;
    expect(dl.compareDocumentPosition(row.element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    await row.get("button").trigger("click");
    expect(updates.run).toHaveBeenCalledTimes(1);

    updates.label.value = "Restart to update";
    await flushPromises();
    expect(row.get("button").text()).toBe("Restart to update");
    wrapper.unmount();
  });

  it("shows download progress in place of the button", async () => {
    updates.phase.value = "downloading";
    updates.progress.value = 0.42;
    updates.status.value = "Downloading update";
    updates.label.value = null;
    const wrapper = mountAbout();
    await flushPromises();
    const row = wrapper.get('[data-test="about-updates"]');
    expect(row.find("button").exists()).toBe(false);
    expect(row.get('[role="progressbar"]').attributes("aria-valuenow")).toBe("42");
    wrapper.unmount();
  });

  it("keeps a blocked restart disabled", async () => {
    updates.phase.value = "ready";
    updates.label.value = "Finish current work first";
    updates.disabled.value = true;
    const wrapper = mountAbout();
    await flushPromises();
    expect(wrapper.get('[data-test="about-updates"] button').attributes("disabled")).toBeDefined();
    wrapper.unmount();
  });

  it("omits the row when updates are unavailable in this build", async () => {
    updates.enabled = false;
    const wrapper = mountAbout();
    await flushPromises();
    expect(wrapper.find('[data-test="about-updates"]').exists()).toBe(false);
    wrapper.unmount();
  });
});
