import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

export type SuccessfulAudioRetention =
  | "delete_immediately"
  | "after_24_hours"
  | "after_7_days";

export const MEETING_AUDIO_RETENTION_OPTIONS: Array<{ value: SuccessfulAudioRetention; label: string }> = [
  { value: "delete_immediately", label: "Delete immediately" },
  { value: "after_24_hours", label: "After 24 hours" },
  { value: "after_7_days", label: "After 7 days" },
];

const STORAGE_KEY = "murmur:meeting-audio-retention";
// v0.1.1 offered "Until deleted"; retention now tops out at 7 days and the
// watcher below persists the replacement.
const LEGACY_KEEP = "keep";
const saved = typeof localStorage === "undefined" ? null : localStorage.getItem(STORAGE_KEY);
const stored = saved === LEGACY_KEEP ? "after_7_days" : saved;
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
