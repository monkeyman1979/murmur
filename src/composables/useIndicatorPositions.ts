import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

export const INDICATOR_POSITIONS = [
  "top-left", "top-center", "top-right",
  "bottom-left", "bottom-center", "bottom-right",
] as const;
export type IndicatorPosition = typeof INDICATOR_POSITIONS[number];
export type IndicatorStyle = "floating" | "notch";

const DEFAULT_POSITION: IndicatorPosition = "bottom-center";
const DICTATION_KEY = "murmur:dictation-indicator-position";
const MEETING_KEY = "murmur:meeting-indicator-position";
const DICTATION_STYLE_KEY = "murmur:dictation-indicator-style";
const MEETING_STYLE_KEY = "murmur:meeting-indicator-style";

function loadStyle(key: string): IndicatorStyle {
  try { return localStorage.getItem(key) === "notch" ? "notch" : "floating"; }
  catch { return "floating"; }
}

function loadStored(key: string): IndicatorPosition {
  try {
    const value = localStorage.getItem(key);
    return INDICATOR_POSITIONS.includes(value as IndicatorPosition)
      ? value as IndicatorPosition
      : DEFAULT_POSITION;
  } catch {
    return DEFAULT_POSITION;
  }
}

const dictation = ref<IndicatorPosition>(loadStored(DICTATION_KEY));
const meeting = ref<IndicatorPosition>(loadStored(MEETING_KEY));
const dictationStyle = ref<IndicatorStyle>(loadStyle(DICTATION_STYLE_KEY));
const meetingStyle = ref<IndicatorStyle>(loadStyle(MEETING_STYLE_KEY));

// Module lifetime, not the Settings component lifetime. Root imports hydrate
// the native defaults before recording, even if Settings is never opened.
watch([dictation, meeting, dictationStyle, meetingStyle], ([dictationPosition, meetingPosition, dictationMode, meetingMode]) => {
  try {
    localStorage.setItem(DICTATION_KEY, dictationPosition);
    localStorage.setItem(MEETING_KEY, meetingPosition);
    localStorage.setItem(DICTATION_STYLE_KEY, dictationMode);
    localStorage.setItem(MEETING_STYLE_KEY, meetingMode);
  } catch {
    // Native placement still works when browser storage is unavailable.
  }
  void invoke("set_indicator_positions", {
    dictation: dictationPosition,
    meeting: meetingPosition,
    dictationStyle: dictationMode,
    meetingStyle: meetingMode,
  }).catch((error) => {
    console.error("[murmur] set_indicator_positions failed", error);
  });
}, { immediate: true });

export function useIndicatorPositions() {
  return { dictation, meeting, dictationStyle, meetingStyle };
}
