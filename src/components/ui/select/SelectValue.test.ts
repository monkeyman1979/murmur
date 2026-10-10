import { mount } from "@vue/test-utils";
import { createI18n, useI18n } from "vue-i18n";
import { defineComponent, h, nextTick, ref } from "vue";
import { describe, expect, it } from "vitest";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

// Reka caches selected item text while its menu is closed. An explicit slot
// keeps the trigger live when the app locale changes without opening the menu.
const Probe = defineComponent({
  setup() {
    const value = ref("auto");
    const { t } = useI18n();
    return () => h(Select, { modelValue: value.value }, {
      default: () => [
        h(SelectTrigger, { "aria-label": "Language" }, {
          default: () => h(SelectValue, null, { default: () => t("auto") }),
        }),
        h(SelectContent, null, { default: () => h(SelectItem, { value: "auto" }, { default: () => t("auto") }) }),
      ],
    });
  },
});

describe("localized select trigger", () => {
  it("updates the selected label when the locale switches while closed", async () => {
    const i18n = createI18n({ legacy: false, locale: "en", messages: { en: { auto: "Auto-detect" }, ru: { auto: "Автоопределение" } } });
    const wrapper = mount(Probe, { global: { plugins: [i18n] } });
    expect(wrapper.get("button").text()).toContain("Auto-detect");
    i18n.global.locale.value = "ru";
    await nextTick();
    expect(wrapper.get("button").text()).toContain("Автоопределение");
    expect(wrapper.get("button").text()).not.toContain("Auto-detect");
    wrapper.unmount();
  });
});
