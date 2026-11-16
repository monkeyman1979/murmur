import { mount } from "@vue/test-utils";
import { defineComponent, ref } from "vue";
import { describe, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import Picker from "./IndicatorPlacementPicker.vue";
import generalSource from "@/views/settings/General.vue?raw";

const Harness = defineComponent({
  components: { Picker },
  setup() {
    return { position: ref("top-left"), style: ref("floating") };
  },
  template: '<Picker v-model="position" v-model:style="style" label="Recording indicator" />',
});

describe("visual recording placement", () => {
  it("keeps the Floating and Notch previews the same height so switching never shifts the page", async () => {
    const wrapper = mount(Picker, {
      props: { modelValue: "bottom-center", style: "floating", label: "Dictation" },
      global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
    });
    const floatingHeight = wrapper.get(".placement-screen").classes().find(name => name.startsWith("h-"));
    await wrapper.setProps({ style: "notch" });
    expect(floatingHeight).toBeDefined();
    expect(wrapper.get(".placement-screen").classes()).toContain(floatingHeight);
    const notch = wrapper.get(".notch-preview");
    expect(notch.classes()).toEqual(expect.arrayContaining(["justify-between", "items-center", "px-2"]));
  });

  it("provides six spatially arranged native radio choices, not a dropdown", () => {
    const wrapper = mount(Picker, {
      props: { modelValue: "bottom-center", style: "floating", label: "Dictation" },
      global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
    });
    const anchors = wrapper.findAll('input[type="radio"]').filter(input => input.attributes("name")?.endsWith("-position"));
    expect(anchors).toHaveLength(6);
    expect(anchors[4].element).toHaveProperty("checked", true);
    expect(wrapper.find("select").exists()).toBe(false);
    expect(wrapper.get('[aria-label="Top left"]').element.closest("label")?.style.gridRow).toBe("1");
    expect(wrapper.get('[aria-label="Bottom right"]').element.closest("label")?.style.gridRow).toBe("3");
    wrapper.unmount();
  });

  it("switches the shared style without losing the floating anchor", async () => {
    const wrapper = mount(Harness, {
      global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
    });
    const picker = wrapper.getComponent(Picker);
    await picker.get('input[value="notch"]').setValue(true);
    expect(picker.find('[aria-label="Top left"]').exists()).toBe(false);
    expect(picker.find("p").exists()).toBe(false);

    await picker.get('input[value="floating"]').setValue(true);
    expect(picker.get('[aria-label="Top left"]').element).toHaveProperty("checked", true);
    await picker.get('[aria-label="Top right"]').setValue(true);
    expect(picker.emitted("update:modelValue")?.slice(-1)[0]).toEqual(["top-right"]);
    wrapper.unmount();
  });

  it("places one shared picker below Fundamentals and before Dictation settings", () => {
    expect(generalSource.match(/<IndicatorPlacementPicker\b/g)).toHaveLength(1);
    expect(generalSource).toContain('v-model="indicatorPosition" v-model:style="indicatorStyle"');
    expect(generalSource).toContain("t('indicatorPosition.title')");
    expect(generalSource).toContain("t('indicatorPosition.description')");
    expect(generalSource).toContain("indicatorStyle === 'notch' ? t('indicatorPosition.notchDescription')");
    expect(en.indicatorPosition.notchDescription).toContain("bottom center");
    const fundamentals = generalSource.indexOf("t('generalSettings.fundamentals')");
    const feedback = generalSource.indexOf("t('generalSettings.recordingFeedback')");
    const dictation = generalSource.indexOf("t('generalSettings.dictation')");
    expect(fundamentals).toBeGreaterThan(-1);
    expect(feedback).toBeGreaterThan(fundamentals);
    expect(dictation).toBeGreaterThan(feedback);
  });
});
