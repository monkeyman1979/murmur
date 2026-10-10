import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

export type MeetingTranscriptRetentionDays = 0 | 7 | 30 | 90 | 365;

export const MEETING_TRANSCRIPT_RETENTION_OPTIONS: Array<{
  value: MeetingTranscriptRetentionDays;
  label: string;
}> = [
  { value: 0, label: "Keep forever" },
  { value: 7, label: "7 days" },
  { value: 30, label: "30 days" },
  { value: 90, label: "90 days" },
  { value: 365, label: "1 year" },
];

const STORAGE_KEY = "murmur:meeting-transcript-retention";

function isRetention(value: unknown): value is MeetingTranscriptRetentionDays {
  return value === 0 || value === 7 || value === 30 || value === 90 || value === 365;
}

function loadStored(): MeetingTranscriptRetentionDays {
  if (typeof localStorage === "undefined") return 0;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return 0;
    const parsed = Number(JSON.parse(raw));
    return isRetention(parsed) ? parsed : 0;
  } catch {
    return 0;
  }
}

const days = ref<MeetingTranscriptRetentionDays>(loadStored());
let initialized = false;

export function useMeetingTranscriptRetention() {
  if (!initialized) {
    initialized = true;
    watch(
      days,
      (value) => {
        if (typeof localStorage !== "undefined") {
          try {
            localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
          } catch {
            // Keep the setting for this session when storage is unavailable.
          }
        }
        void invoke("set_meeting_transcript_retention", { days: value }).catch((error) => {
          console.error("[murmur] set_meeting_transcript_retention failed", error);
        });
      },
      { immediate: true },
    );
  }

  return { days };
}
