import { flushPromises, mount } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { nextTick } from "vue";
import { afterEach, describe, expect, it } from "vitest";
import SupportedDictationLanguagesDialog from "./SupportedDictationLanguagesDialog.vue";
import en from "@/locales/en.json";
import ru from "@/locales/ru.json";

afterEach(() => { document.body.innerHTML = ""; });

describe("dictation and meeting languages dialog", () => {
  it("distinguishes app languages from transcription-only languages without promising accuracy", async () => {
    const i18n = createI18n({ legacy: false, locale: "en", messages: { en, ru } });
    const wrapper = mount(SupportedDictationLanguagesDialog, {
      attachTo: document.body, global: { plugins: [i18n] },
    });
    await wrapper.get("button").trigger("click");
    await flushPromises();
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(dialog.textContent).toContain("not translated into your App language");
    expect(dialog.textContent).toContain("not tested transcription accuracy language by language for either group");
    expect(dialog.querySelector('#app-and-model-languages')?.textContent).toBe("App, dictation & meeting transcripts (untested)");
    expect(dialog.querySelector('#model-only-languages')?.textContent).toBe("Dictation & meeting transcripts only (untested)");
    expect(dialog.querySelectorAll("li")).toHaveLength(99);
    expect(dialog.querySelectorAll('#app-and-model-languages + ul li')).toHaveLength(4);
    expect(dialog.querySelectorAll('#model-only-languages + ul li')).toHaveLength(95);
    expect(dialog.querySelector('#app-and-model-languages + ul')?.textContent).toContain("Spanish");
    expect(dialog.querySelector('#model-only-languages + ul')?.textContent).toContain("Portuguese");
    expect(dialog.textContent).not.toContain("Cantonese");

    i18n.global.locale.value = "ru";
    await nextTick();
    expect(dialog.textContent).toContain("португальский");
    expect(dialog.textContent).toContain("не переводится на язык приложения");
    const search = dialog.querySelector<HTMLInputElement>('input[type="search"]')!;
    search.value = "португальский";
    search.dispatchEvent(new Event("input", { bubbles: true }));
    await nextTick();
    expect(dialog.querySelectorAll("li")).toHaveLength(1);
    expect(dialog.querySelector("li")?.textContent).toBe("португальский");
    expect(dialog.querySelector('#app-and-model-languages')).toBeNull();
    search.value = "английский";
    search.dispatchEvent(new Event("input", { bubbles: true }));
    await nextTick();
    expect(dialog.querySelectorAll("li")).toHaveLength(1);
    expect(dialog.querySelector('#model-only-languages')).toBeNull();
    wrapper.unmount();
  });
});
