import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { computed, ref } from "vue";

/**
 * Full-corpus dictation stats backed by the SQLite history store
 * (SPEC §2.9). Replaces the old `useSessionStats` which derived its
 * numbers from the in-memory session list and therefore reset to
 * zero on every app restart — exactly the bug Ian flagged on
 * 2026-04-22 where the Home cards showed `0` / `0 min` / `0 wpm`
 * next to a Recent-transcripts list that clearly had entries.
 *
 * Totals refresh on: first use (mount), every incoming
 * `murmur://transcript` event, and whenever `bumpRefresh()` is
 * called from the outside (used by `useHistory` after delete /
 * clear-all so the numbers stay honest).
 */

// Assumed typing baseline in words per minute. Same as the old
// `useSessionStats`. Post-v0.1 this moves into Settings
// as a configurable field.
const ASSUMED_TYPING_WPM = 40;

interface HistoryStatsWire {
  total_entries: number;
  total_words: number;
  total_duration_ms: number;
}

const totalEntries = ref<number>(0);
const totalWords = ref<number>(0);
const totalDurationMs = ref<number>(0);

let initialized = false;
let inflight = false;

export async function refreshHistoryStats(): Promise<void> {
  if (inflight) return;
  inflight = true;
  try {
    const stats = await invoke<HistoryStatsWire>("history_stats");
    totalEntries.value = stats.total_entries;
    totalWords.value = stats.total_words;
    totalDurationMs.value = stats.total_duration_ms;
  } catch (err) {
    console.error("[murmur] history_stats failed", err);
  } finally {
    inflight = false;
  }
}

function init(): void {
  if (initialized) return;
  initialized = true;

  void refreshHistoryStats();

  // Every successful dictation writes a row and then emits the
  // transcript event — re-aggregate so the Home cards update live.
  listen("murmur://transcript", () => {
    void refreshHistoryStats();
  }).catch((err) => {
    console.error("[murmur] useHistoryStats listen failed", err);
  });
}

export function useHistoryStats() {
  init();

  const dictatedWords = totalWords;
  const totalDictationMs = totalDurationMs;

  // Milliseconds saved vs. typing at `ASSUMED_TYPING_WPM`. Clamped
  // to zero — if dictation took longer than typing would, we show
  // "0 min saved" rather than a negative value. Same math the old
  // session-only version used.
  const timeSavedMs = computed<number>(() => {
    const typingMs = (totalWords.value * 60_000) / ASSUMED_TYPING_WPM;
    const saved = typingMs - totalDurationMs.value;
    return Math.max(0, saved);
  });

  // Average dictation throughput. `null` when there's no history
  // yet so consumers can render a "—" or "0" instead of NaN.
  const averageWpm = computed<number | null>(() => {
    if (totalDurationMs.value === 0) return null;
    return totalWords.value / (totalDurationMs.value / 60_000);
  });

  return {
    dictatedWords,
    timeSavedMs,
    averageWpm,
    totalDictationMs,
    totalEntries,
    refresh: refreshHistoryStats,
  };
}
