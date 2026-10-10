import { beforeEach, describe, expect, it } from "vitest";
import { nextTick } from "vue";
import { appLanguage, APP_LANGUAGE_KEY, resolveAppLanguage, setAppLanguage, supportedAppLanguages } from "./appLanguage";
import { i18n } from "./index";

beforeEach(async () => {
  localStorage.clear();
  setAppLanguage("en");
  await nextTick();
});

describe("app language preference", () => {
  it("offers four interface languages with English fallback for unknown/stale values", () => {
    expect(supportedAppLanguages.map((language) => language.code)).toEqual(["en", "es", "fr", "ru"]);
    for (const value of ["en", "es", "fr", "ru"] as const) {
      expect(resolveAppLanguage(value)).toBe(value);
    }
    for (const value of [null, "xx", ""]) expect(resolveAppLanguage(value)).toBe("en");
    expect(i18n.global.fallbackLocale.value).toBe("en");
    expect(supportedAppLanguages.map(({ label }) => label)).toEqual(["English", "Español", "Français", "Русский"]);
  });

  it("persists independently of dictation and updates only interface messages", async () => {
    localStorage.setItem("murmur:dictation-language", "ru");
    localStorage.setItem("murmur:meeting-audio-retention", "keep");
    setAppLanguage("es");
    expect(appLanguage.value).toBe("es");
    expect(i18n.global.locale.value).toBe("es");
    expect(i18n.global.t("navigation.home")).toBe("Inicio");
    await nextTick();
    expect(localStorage.getItem(APP_LANGUAGE_KEY)).toBe("es");
    expect(localStorage.getItem("murmur:dictation-language")).toBe("ru");
    expect(localStorage.getItem("murmur:meeting-audio-retention")).toBe("keep");
    window.dispatchEvent(new StorageEvent("storage", {
      key: APP_LANGUAGE_KEY,
      newValue: "ru",
    }));
    expect(appLanguage.value).toBe("ru");
    expect(i18n.global.locale.value).toBe("ru");
    expect(i18n.global.t("navigation.home")).toBe("Главная");
    expect(localStorage.getItem("murmur:dictation-language")).toBe("ru");
    expect(localStorage.getItem("murmur:meeting-audio-retention")).toBe("keep");
  });
});
