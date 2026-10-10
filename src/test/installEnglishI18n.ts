import { config } from "@vue/test-utils";
import { afterEach, beforeEach } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";

// Component tests that mount translated children without the production entry point.
beforeEach(() => {
  config.global.plugins = [createI18n({ legacy: false, locale: "en", messages: { en } })];
});
afterEach(() => { config.global.plugins = []; });
