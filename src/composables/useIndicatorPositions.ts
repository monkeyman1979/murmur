import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

export const INDICATOR_POSITIONS = [
  "top-left", "top-center", "top-right",
  "bottom-left", "bottom-center", "bottom-right",
] as const;
export type IndicatorPosition = typeof INDICATOR_POSITIONS[number];

const DEFAULT_POSITION: IndicatorPosition = "bottom-center";
const DICTATION_KEY = "murmur:dictation-indicator-position";
const MEETING_KEY = "murmur:meeting-indicator-position";

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

// Module lifetime, not the Settings component lifetime. Root imports hydrate
// the native defaults before recording, even if Settings is never opened.
watch([dictation, meeting], ([dictationPosition, meetingPosition]) => {
  try {
    localStorage.setItem(DICTATION_KEY, dictationPosition);
    localStorage.setItem(MEETING_KEY, meetingPosition);
  } catch {
    // Native placement still works when browser storage is unavailable.
  }
  void invoke("set_indicator_positions", {
    dictation: dictationPosition,
    meeting: meetingPosition,
  }).catch((error) => {
    console.error("[murmur] set_indicator_positions failed", error);
  });
}, { immediate: true });

export function useIndicatorPositions() {
  return { dictation, meeting };
}
