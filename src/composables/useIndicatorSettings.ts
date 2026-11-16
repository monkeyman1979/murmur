import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

export const INDICATOR_POSITIONS = [
  "top-left", "top-center", "top-right",
  "bottom-left", "bottom-center", "bottom-right",
] as const;
export type IndicatorPosition = typeof INDICATOR_POSITIONS[number];
export type IndicatorStyle = "floating" | "notch";

const SETTINGS_KEY = "murmur:recording-indicator-v1";
// Migration only: never maintain separate preferences after a successful save.
const LEGACY_KEYS = [
  "murmur:dictation-indicator-position", "murmur:meeting-indicator-position",
  "murmur:dictation-indicator-style", "murmur:meeting-indicator-style",
] as const;
const DEFAULT_POSITION: IndicatorPosition = "bottom-center";

function positionOrNull(value: unknown): IndicatorPosition | null {
  return INDICATOR_POSITIONS.includes(value as IndicatorPosition)
    ? value as IndicatorPosition : null;
}

function styleOrNull(value: unknown): IndicatorStyle | null {
  return value === "notch" || value === "floating" ? value : null;
}

function loadSettings(): { position: IndicatorPosition; style: IndicatorStyle } {
  try {
    const stored = localStorage.getItem(SETTINGS_KEY);
    if (stored !== null) {
      try {
        const saved = JSON.parse(stored);
        if (saved && typeof saved === "object") {
          return {
            position: positionOrNull(saved.position) ?? DEFAULT_POSITION,
            style: styleOrNull(saved.style) ?? "floating",
          };
        }
      } catch { /* Recover malformed shared data from legacy choices if available. */ }
    }
    // Dictation wins conflicts; a valid meeting choice is the fallback.
    return {
      position: positionOrNull(localStorage.getItem(LEGACY_KEYS[0]))
        ?? positionOrNull(localStorage.getItem(LEGACY_KEYS[1])) ?? DEFAULT_POSITION,
      style: styleOrNull(localStorage.getItem(LEGACY_KEYS[2]))
        ?? styleOrNull(localStorage.getItem(LEGACY_KEYS[3])) ?? "floating",
    };
  } catch {
    return { position: DEFAULT_POSITION, style: "floating" };
  }
}

const saved = loadSettings();
const position = ref<IndicatorPosition>(saved.position);
const style = ref<IndicatorStyle>(saved.style);

// Module lifetime, not the Settings component lifetime. Root imports hydrate
// one native preference for both recording types before Settings is opened.
watch([position, style], ([position, style]) => {
  const settings = { position, style };
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    // Remove old keys only after the shared preference is durably saved.
    for (const key of LEGACY_KEYS) localStorage.removeItem(key);
  } catch {
    // Native placement still works when browser storage is unavailable.
  }
  void invoke("set_indicator_settings", { settings }).catch((error) => {
    console.error("[murmur] set_indicator_settings failed", error);
  });
}, { immediate: true });

export function useIndicatorSettings() {
  return { position, style };
}
