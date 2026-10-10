import { createI18n } from "vue-i18n";
import { watch } from "vue";
import { appLanguage } from "./appLanguage";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

/**
 * Shared `vue-i18n` instance for both window entry points (main + indicator;
 * see `src/main.ts`). Built once at module load so re-importing in either
 * code path gets the same translation cache.
 *
 * Complete trial catalogs are registered for the four interface languages.
 * See src/locales/README.md for the contributor and QA checklist.
 *
 * - `legacy: false` — Composition API mode. Required for `useI18n()` in
 *   `<script setup>` blocks.
 * - `fallbackLocale: 'en'` — unexpected missing keys fall back to English.
 *   A missing key is still a release-blocking translation defect.
 * - `missingWarn` / `fallbackWarn` — only chatty in dev so unmigrated
 *   strings are visible in the console while CI / production stays clean.
 *   Catalog parity and static-key tests catch missing translations in CI.
 */
export const i18n = createI18n({
  legacy: false,
  locale: appLanguage.value,
  fallbackLocale: "en",
  messages: { en, es, fr, ru },
  missingWarn: import.meta.env.DEV,
  fallbackWarn: import.meta.env.DEV,
});

watch(appLanguage, (value) => {
  i18n.global.locale.value = value;
}, { flush: "sync" });
