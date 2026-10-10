import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ref } from "vue";

import { refreshHistoryStats } from "@/composables/useHistoryStats";
import type { FocusedApp, Transcript } from "@/composables/useMurmurEvents";

/**
 * Rust-side record shape. serde serializes `TranscriptRecord` with
 * snake_case field names; we keep it verbatim here so no wire adapter
 * is needed (unlike the corrections composable where the frontend's
 * camelCase preference justified the extra translation layer).
 */
export interface TranscriptRecord {
  id: number;
  created_at: number;
  text_raw: string;
  text_final: string;
  duration_ms: number;
  word_count: number;
  app_name: string | null;
  app_bundle: string | null;
}

export function recordToFocusedApp(
  record: TranscriptRecord,
): FocusedApp | null {
  if (!record.app_name && !record.app_bundle) return null;
  return {
    name: record.app_name,
    bundle_id: record.app_bundle,
  };
}

const PAGE_SIZE = 50;

const items = ref<TranscriptRecord[]>([]);
const total = ref<number>(0);
const loading = ref<boolean>(false);
const searchQuery = ref<string>("");
const hasMore = ref<boolean>(true);

let initialized = false;

async function refreshCount(): Promise<void> {
  try {
    total.value = await invoke<number>("history_count");
  } catch (err) {
    console.error("[murmur] history_count failed", err);
  }
}

async function loadFirstPage(): Promise<void> {
  loading.value = true;
  try {
    const query = searchQuery.value.trim();
    const rows = query
      ? await invoke<TranscriptRecord[]>("history_search", {
          query,
          limit: PAGE_SIZE,
        })
      : await invoke<TranscriptRecord[]>("history_list", {
          limit: PAGE_SIZE,
          offset: 0,
        });
    items.value = rows;
    hasMore.value = rows.length === PAGE_SIZE;
    await refreshCount();
  } catch (err) {
    console.error("[murmur] history load failed", err);
  } finally {
    loading.value = false;
  }
}

async function loadMore(): Promise<void> {
  if (loading.value || !hasMore.value) return;
  // FTS search currently returns up to `limit` rows total; paginating
  // past that needs a ranked cursor we haven't wired yet. For the
  // narrow search case the first page is usually the whole result set
  // anyway, so cap "load more" to the non-search path.
  if (searchQuery.value.trim().length > 0) {
    hasMore.value = false;
    return;
  }
  loading.value = true;
  try {
    const rows = await invoke<TranscriptRecord[]>("history_list", {
      limit: PAGE_SIZE,
      offset: items.value.length,
    });
    items.value = [...items.value, ...rows];
    hasMore.value = rows.length === PAGE_SIZE;
  } catch (err) {
    console.error("[murmur] history load-more failed", err);
  } finally {
    loading.value = false;
  }
}

async function setSearch(query: string): Promise<void> {
  searchQuery.value = query;
  await loadFirstPage();
}

async function deleteOne(id: number): Promise<void> {
  try {
    await invoke<boolean>("history_delete", { id });
    items.value = items.value.filter((r) => r.id !== id);
    await refreshCount();
    // Stats card totals change with every delete — keep Home's
    // numbers honest instead of waiting for the next dictation to
    // trigger a re-aggregation.
    void refreshHistoryStats();
  } catch (err) {
    console.error("[murmur] history_delete failed", err);
  }
}

async function clearAll(): Promise<void> {
  try {
    await invoke<number>("history_clear_all");
    items.value = [];
    total.value = 0;
    hasMore.value = false;
    void refreshHistoryStats();
  } catch (err) {
    console.error("[murmur] history_clear_all failed", err);
  }
}

function init(): void {
  if (initialized) return;
  initialized = true;

  // Seed on first use — the page reads reactive state synchronously so
  // the data arrives shortly after mount rather than blocking it.
  void loadFirstPage();

  // Keep the reactive list warm when a fresh dictation lands. The
  // Rust-side insert has already happened before the event fires, so
  // we can either prepend an optimistic local record (no extra
  // roundtrip) or re-fetch. Prepend is cheaper and matches what the
  // user just saw paste. Refresh count in parallel so the footer
  // total stays accurate.
  listen<Transcript>("murmur://transcript", (event) => {
    // While the user is actively searching, don't mutate the result
    // set — the new transcript may not match the query and injecting
    // it would confuse the visible "results for X" view.
    if (searchQuery.value.trim().length > 0) {
      void refreshCount();
      return;
    }
    const optimistic: TranscriptRecord = {
      // Negative placeholder so it can't collide with a real SQLite
      // rowid. The next full reload reconciles with the real id.
      id: -Date.now(),
      created_at: Date.now(),
      text_raw: event.payload.text,
      text_final: event.payload.text,
      duration_ms: event.payload.duration_ms,
      word_count: event.payload.text.split(/\s+/).filter(Boolean).length,
      app_name: event.payload.app?.name ?? null,
      app_bundle: event.payload.app?.bundle_id ?? null,
    };
    items.value = [optimistic, ...items.value];
    void refreshCount();
  }).catch((err) => {
    console.error("[murmur] history listen failed", err);
  });
}

export function useHistory() {
  init();
  return {
    items,
    total,
    loading,
    hasMore,
    searchQuery,
    loadFirstPage,
    loadMore,
    setSearch,
    deleteOne,
    clearAll,
  };
}
