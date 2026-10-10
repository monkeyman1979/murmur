import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

const STORAGE_KEY = "murmur:history-retention";

/**
 * Retention window in days. `0` means "keep forever" (the default —
 * matches local-first posture and Willow's default). Anything >0 is
 * a day count; Rust purges on startup and after each dictation.
 */
export type RetentionDays = 0 | 7 | 30 | 90 | 365;

export const RETENTION_OPTIONS: { value: RetentionDays; label: string }[] = [
  { value: 0, label: "Keep forever" },
  { value: 7, label: "7 days" },
  { value: 30, label: "30 days" },
  { value: 90, label: "90 days" },
  { value: 365, label: "1 year" },
];

function isRetention(n: unknown): n is RetentionDays {
  return n === 0 || n === 7 || n === 30 || n === 90 || n === 365;
}

function loadStored(): RetentionDays {
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

const days = ref<RetentionDays>(loadStored());
let initialized = false;

function init(): void {
  if (initialized) return;
  initialized = true;

  watch(
    days,
    (value) => {
      if (typeof localStorage !== "undefined") {
        try {
          localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
        } catch {
          // Quota / private mode — session-only is fine.
        }
      }
      void invoke("set_history_retention", { days: value }).catch((err) => {
        console.error("[murmur] set_history_retention failed", err);
      });
    },
    { immediate: true },
  );
}

export function useHistoryRetention() {
  init();
  return { days };
}
