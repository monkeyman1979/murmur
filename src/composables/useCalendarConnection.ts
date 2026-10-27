import { invoke } from "@tauri-apps/api/core";
import { computed, onBeforeUnmount, ref } from "vue";

export type CalendarPermission = "not-determined" | "granted" | "denied" | "restricted" | "write-only" | "unsupported";
export interface CalendarAccount {
  id: string;
  email: string;
}
export interface CalendarMeeting {
  id: string;
  account_id: string;
  calendar_id: string;
  title: string;
  start_ms: number;
  end_ms: number;
  provider: "GoogleMeet" | "Zoom" | "Teams" | "Webex";
}
export interface CalendarSnapshot {
  accounts: CalendarAccount[];
  meetings: CalendarMeeting[];
}

// Account choices deliberately use a new key. Converting an earlier partial
// calendar choice into a whole-account opt-in would silently broaden access.
export const CALENDAR_SELECTION_KEY = "murmur:calendar-account-selection-v1";
export const CALENDAR_CONNECTED_KEY = "murmur:calendar-connected-v1";

export const CALENDAR_ACCOUNT_SELECTION_CHANGED = "murmur:calendar-account-selection-changed";

export function readCalendarSelection(): { ids: string[]; failed: boolean } {
  try {
    const raw = localStorage.getItem(CALENDAR_SELECTION_KEY);
    if (!raw) return { ids: [], failed: false };
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value) || value.length > 2048
      || !value.every((id) => typeof id === "string" && id.length > 0 && id.length <= 4096)) {
      return { ids: [], failed: true };
    }
    return { ids: [...new Set(value as string[])], failed: false };
  } catch {
    return { ids: [], failed: true };
  }
}

function savedConnection(selectedIds: string[]): boolean {
  try {
    const raw = localStorage.getItem(CALENDAR_CONNECTED_KEY);
    // Existing account selections were saved only after an explicit connection.
    // An explicit disconnect overrides them without erasing those selections.
    return raw === null ? selectedIds.length > 0 : raw === "true";
  } catch { return false; }
}

/** Opening Settings never reads events or requests access. Connection preferences
 * and opaque account IDs are saved on this Mac; email labels remain in component
 * memory. Account management never queries meeting content. Disconnect/unmount clears the private snapshot but keeps
 * the user's selections. Connection intent is separate from the reminder opt-in;
 * unmount clears private content, not the saved connection.
 */
export function useCalendarConnection() {
  const saved = readCalendarSelection();
  const selectedIds = ref(saved.ids);
  const connected = ref(savedConnection(saved.ids));
  const selectionStorageFailed = ref(saved.failed);
  const permission = ref<CalendarPermission | null>(null);
  const snapshot = ref<CalendarSnapshot | null>(null);
  const pending = ref(false);
  const failed = ref(false);
  let generation = 0;
  let disposed = false;

  const selectedSet = computed(() => new Set(selectedIds.value));
  const selectedCount = computed(() => snapshot.value?.accounts.filter((account) => selectedSet.value.has(account.id)).length ?? 0);

  function clearSnapshot(): void {
    generation++;
    snapshot.value = null;
    pending.value = false;
    failed.value = false;
  }

  function saveConnection(value: boolean): void {
    connected.value = value;
    try { localStorage.setItem(CALENDAR_CONNECTED_KEY, String(value)); }
    catch { selectionStorageFailed.value = true; }
  }

  function disconnect(): void {
    saveConnection(false);
    clearSnapshot();
  }

  async function readConnection(requestPermission: boolean): Promise<void> {
    if (disposed || pending.value) return;
    const token = ++generation;
    const current = () => !disposed && token === generation;
    pending.value = true;
    failed.value = false;
    // Keep the existing account picker steady while refreshing.
    try {
      const actual = await invoke<CalendarPermission>("calendar_permission");
      if (!current()) return;
      permission.value = actual;
      if (requestPermission && (actual === "not-determined" || actual === "write-only")) {
        const requested = await invoke<CalendarPermission>("request_calendar_access");
        if (!current()) return;
        permission.value = requested;
      }
      if (permission.value !== "granted") {
        saveConnection(false);
        snapshot.value = null;
        if (permission.value === "not-determined") failed.value = true;
        return;
      }
      // Account management needs only the directory. Empty selection tells
      // EventKit not to query events; the opt-in native reminder loop owns that.
      const result = await invoke<CalendarSnapshot>("read_calendar_snapshot", { accountIds: [] });
      if (current()) {
        snapshot.value = { accounts: result.accounts, meetings: [] };
        saveConnection(true);
      }
    } catch {
      // Never log/render native exceptions: they may contain private data.
      if (current()) {
        snapshot.value = null;
        failed.value = true;
      }
    } finally {
      if (current()) pending.value = false;
    }
  }

  function connect(): Promise<void> {
    return readConnection(true);
  }

  async function setAccount(id: string, enabled: boolean): Promise<void> {
    if (disposed || pending.value || !snapshot.value) return;
    if (!snapshot.value.accounts.some((account) => account.id === id)) return;
    const next = new Set(selectedIds.value);
    if (enabled) next.add(id); else next.delete(id);
    selectedIds.value = [...next];
    try {
      localStorage.setItem(CALENDAR_SELECTION_KEY, JSON.stringify(selectedIds.value));
      selectionStorageFailed.value = false;
    } catch {
      // Session choices still work; make it explicit that they weren't saved.
      selectionStorageFailed.value = true;
    }
    window.dispatchEvent(new CustomEvent(CALENDAR_ACCOUNT_SELECTION_CHANGED, { detail: [...selectedIds.value] }));
    // Toggling an account never launches a new OS permission prompt.
    await readConnection(false);
  }

  onBeforeUnmount(() => {
    disposed = true;
    clearSnapshot();
  });

  return {
    connected, permission, snapshot, pending, failed, selectedIds, selectedCount,
    selectionStorageFailed, connect, disconnect, setAccount,
  };
}
