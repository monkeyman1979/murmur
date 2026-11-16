import { mount } from "@vue/test-utils";
import { defineComponent, ref } from "vue";
import { describe, expect, it, vi } from "vitest";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import Picker from "./IndicatorPlacementPicker.vue";

const Harness = defineComponent({
  components: { Picker },
  setup() {
    return { dictation: ref("top-left"), meeting: ref("bottom-right"), dictationStyle: ref("floating"), meetingStyle: ref("floating") };
  },
  template: '<Picker v-model="dictation" v-model:style="dictationStyle" label="Dictation" /><Picker v-model="meeting" v-model:style="meetingStyle" label="Meetings" />',
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

  it("changes styles independently and restores each previous floating anchor", async () => {
    const wrapper = mount(Harness, {
      global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
    });
    const [dictation, meeting] = wrapper.findAllComponents(Picker);
    const dNames = dictation.findAll("input").map(i => i.attributes("name"));
    const mNames = meeting.findAll("input").map(i => i.attributes("name"));
    expect(dNames.some(name => mNames.includes(name))).toBe(false);

    await dictation.get('input[value="notch"]').setValue(true);
    expect(dictation.find('[aria-label="Top left"]').exists()).toBe(false);
    expect(meeting.get('[aria-label="Bottom right"]').element).toHaveProperty("checked", true);
    expect(meeting.get('input[value="floating"]').element).toHaveProperty("checked", true);
    expect(dictation.text()).not.toContain("Hover for a hint");
    expect(dictation.find("p").exists()).toBe(false);

    await dictation.get('input[value="floating"]').setValue(true);
    expect(dictation.get('[aria-label="Top left"]').element).toHaveProperty("checked", true);
    await dictation.get('[aria-label="Top right"]').setValue(true);
    expect(dictation.emitted("update:modelValue")?.slice(-1)[0]).toEqual(["top-right"]);
    expect(meeting.get('[aria-label="Bottom right"]').element).toHaveProperty("checked", true);
    wrapper.unmount();
  });
});
