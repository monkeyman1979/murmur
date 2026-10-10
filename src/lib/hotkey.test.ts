import { describe, expect, it } from "vitest";
import { createI18n } from "vue-i18n";
import { bindingSegments, DEFAULT_BINDING, formatBindingLabel, formatKeycodeName, CG_FLAG_CONTROL, CG_FLAG_SHIFT, NX_DEVICELCMDKEYMASK } from "./hotkey";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

describe("localized hotkey display", () => {
  const catalogs = { en, es, fr, ru };
  it.each([
    ["en", "Right ⌘", "Left ⌘", "Space"],
    ["es", "Derecha ⌘", "Izquierda ⌘", "Espacio"],
    ["fr", "Droite ⌘", "Gauche ⌘", "Espace"],
    ["ru", "Правая ⌘", "Левая ⌘", "Пробел"],
  ])("renders %s labels without changing the saved binding", (locale, right, left, space) => {
    const t = createI18n({ legacy: false, locale, messages: catalogs }).global.t;
    const stored = JSON.stringify(DEFAULT_BINDING);
    expect(bindingSegments(DEFAULT_BINDING, t)).toEqual([right]);
    expect(formatBindingLabel({ kind: "modifier_only", device_mask: NX_DEVICELCMDKEYMASK }, t)).toBe(left);
    const combo = { kind: "key_combo" as const, keycode: 49, required_flags: CG_FLAG_CONTROL | CG_FLAG_SHIFT };
    expect(bindingSegments(combo, t)).toEqual(["⌃", "⇧", space]);
    expect(formatBindingLabel(combo, t)).toBe(`⌃⇧${space}`);
    expect(formatKeycodeName(49, t)).toBe(space);
    expect(JSON.stringify(DEFAULT_BINDING)).toBe(stored);
  });
});
