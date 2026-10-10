import { ref } from "vue";

/** Trial interface locales. Dictation audio is always auto-detected. */
export const supportedAppLanguages = [
  { code: "en", label: "English" },
  { code: "es", label: "Español" },
  { code: "fr", label: "Français" },
  { code: "ru", label: "Русский" },
] as const;
export type AppLanguage = (typeof supportedAppLanguages)[number]["code"];
export const APP_LANGUAGE_KEY = "murmur:app-language";

export function resolveAppLanguage(value: string | null): AppLanguage {
  return supportedAppLanguages.find((option) => option.code === value)?.code ?? "en";
}

function storedLanguage(): AppLanguage {
  try {
    return resolveAppLanguage(globalThis.localStorage?.getItem(APP_LANGUAGE_KEY) ?? null);
  } catch {
    return "en";
  }
}

export const appLanguage = ref<AppLanguage>(storedLanguage());

// Main and indicator are separate webviews: receive changes made in another window.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (event) => {
    if (event.key === APP_LANGUAGE_KEY) {
      appLanguage.value = resolveAppLanguage(event.newValue);
    }
  });
}

export function setAppLanguage(value: string): void {
  const resolved = resolveAppLanguage(value);
  appLanguage.value = resolved;
  try {
    globalThis.localStorage?.setItem(APP_LANGUAGE_KEY, resolved);
  } catch {
    // Storage can be unavailable; keep the preference for this session.
  }
}
