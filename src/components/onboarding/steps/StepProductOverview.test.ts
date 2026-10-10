import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import OnboardingHomePreview from "@/components/onboarding/OnboardingHomePreview.vue";
import StepProductOverview from "./StepProductOverview.vue";

function i18n() { return createI18n({ legacy: false, locale: "en", messages: { en } }); }

describe("StepProductOverview", () => {
  it("introduces only the two capabilities without instructional cards or AI detail", () => {
    const wrapper = mount(StepProductOverview, { global: { plugins: [i18n()] } });
    expect(wrapper.findAll("h2").map((h) => h.text())).toEqual(["Dictate anywhere", "Capture meetings"]);
    expect(wrapper.text()).toContain("A private, local alternative to Wispr Flow.");
    expect(wrapper.text()).toContain("A private, local alternative to Granola.");
    expect(wrapper.text()).toContain("Record meetings and keep a local transcript.");
    expect(wrapper.findAll("article, input, textarea, button")).toHaveLength(0);
    expect(wrapper.text()).not.toMatch(/Copy for AI|summary prompt|replacement for/i);
    wrapper.unmount();
  });

  it("uses one bundled, non-interactive Home image with descriptive alt text but no visible English caption", () => {
    const wrapper = mount(OnboardingHomePreview, { global: { plugins: [i18n()] } });
    const image = wrapper.get("img");
    expect(image.attributes("src")).toContain("home-preview.jpg");
    expect(image.attributes("src")).not.toMatch(/^https?:/);
    expect(image.attributes("alt")).toContain("fictional meeting in progress");
    expect(wrapper.find("figcaption").exists()).toBe(false);
    expect(wrapper.text()).toBe("");
    expect(image.attributes("width")).toBe("2240");
    expect(image.attributes("height")).toBe("1600");
    expect(wrapper.findAll("button, input, video, iframe, a")).toHaveLength(0);
    expect(image.attributes("draggable")).toBe("false");
    wrapper.unmount();
  });

  it.each([["es", es], ["fr", fr], ["ru", ru]] as const)("labels the illustrative English screenshot in %s", (locale, catalog) => {
    const plugin = createI18n({ legacy: false, locale, messages: { en, es, fr, ru } });
    const wrapper = mount(OnboardingHomePreview, { global: { plugins: [plugin] } });
    expect(wrapper.get("figcaption").text()).toBe(catalog.onboarding.productOverview.previewEnglishNote);
    expect(wrapper.get("img").attributes("alt")).toBe(catalog.onboarding.productOverview.previewAlt);
    wrapper.unmount();
  });
});
