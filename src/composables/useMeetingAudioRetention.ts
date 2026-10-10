import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

export type SuccessfulAudioRetention =
  | "delete_immediately"
  | "after_24_hours"
  | "after_7_days"
  | "keep";

export const MEETING_AUDIO_RETENTION_OPTIONS: Array<{ value: SuccessfulAudioRetention; label: string }> = [
  { value: "delete_immediately", label: "Delete immediately" },
  { value: "after_24_hours", label: "After 24 hours" },
  { value: "after_7_days", label: "After 7 days" },
  { value: "keep", label: "Until deleted" },
];

const STORAGE_KEY = "murmur:meeting-audio-retention";
const stored = typeof localStorage === "undefined" ? null : localStorage.getItem(STORAGE_KEY);
const valid = MEETING_AUDIO_RETENTION_OPTIONS.some((option) => option.value === stored);
const retention = ref<SuccessfulAudioRetention>(valid ? stored as SuccessfulAudioRetention : "after_24_hours");
let initialized = false;

export function useMeetingAudioRetention() {
  if (!initialized) {
    initialized = true;
    watch(retention, (value) => {
      localStorage.setItem(STORAGE_KEY, value);
      void invoke("set_meeting_audio_retention", { retention: value }).catch((error) => {
        console.error("[murmur] set_meeting_audio_retention failed", error);
      });
    }, { immediate: true });
  }
  return { retention };
}
