import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ref } from "vue";
import { CALENDAR_ACCOUNT_SELECTION_CHANGED, readCalendarSelection } from "./useCalendarConnection";

const ENABLED_KEY = "murmur:calendar-reminders-enabled-v1";
export const REMINDER_LEAD_SECONDS_KEY = "murmur:calendar-reminders-lead-seconds-v1";
export const REMINDER_TIMINGS = [0, 15, 30, 60] as const;
export const HANDLED_REMINDERS_KEY = "murmur:calendar-reminders-handled-v1";
interface Handled { key: string; expires_ms: number }
interface Due extends Handled { token: string }

function savedEnabled(): boolean {
  try { return localStorage.getItem(ENABLED_KEY) === "true"; } catch { return false; }
}
function savedLeadSeconds(): number {
  try {
    const saved = localStorage.getItem(REMINDER_LEAD_SECONDS_KEY);
    return REMINDER_TIMINGS.some((option) => String(option) === saved) ? Number(saved) : 15;
  } catch { return 15; }
}
const enabled = ref(savedEnabled());
const leadSeconds = ref(savedLeadSeconds());
const pending = ref(false);
const failed = ref(false);
let accountIds = readCalendarSelection().ids;
let queue: Promise<void> = Promise.resolve();
let started = false;
let writes = 0;
let unlisteners: UnlistenFn[] = [];

function handled(): Handled[] {
  const raw = localStorage.getItem(HANDLED_REMINDERS_KEY);
  if (!raw) return [];
  const value: unknown = JSON.parse(raw);
  if (!Array.isArray(value) || value.length > 1024) throw new Error("Invalid reminder preferences");
  const now = Date.now();
  if (!value.every((item) => item && typeof item.key === "string" && /^[a-f0-9]{64}$/.test(item.key)
    && Number.isSafeInteger(item.expires_ms) && item.expires_ms <= now + 172_800_000)) throw new Error("Invalid reminder preferences");
  return value.filter((item) => item.expires_ms > now);
}

function serialize(work: () => Promise<void>): Promise<void> {
  queue = queue.then(work, work);
  return queue;
}

async function configure(value: boolean): Promise<void> {
  await invoke("configure_calendar_reminders", { enabled: value, accountIds, leadSeconds: leadSeconds.value, handled: value ? handled() : [] });
}

async function pauseAfterFailure(): Promise<void> {
  enabled.value = false;
  failed.value = true;
  try { localStorage.setItem(ENABLED_KEY, "false"); } catch { /* disabled for this session */ }
  await configure(false).catch(() => {});
}

async function synchronize(): Promise<void> {
  try {
    await configure(enabled.value);
    failed.value = false;
  } catch {
    // Never keep an earlier configuration monitoring deselected accounts.
    await pauseAfterFailure();
  }
}

function setEnabled(value: boolean): Promise<void> {
  writes++;
  pending.value = true;
  return serialize(async () => {
    try {
      if (value && (!started || unlisteners.length !== 2)) throw new Error("Reminder listeners are not ready");
      // Fail closed if consent cannot be saved. No ephemeral opt-in that
      // silently changes its meaning on the next launch.
      localStorage.setItem(ENABLED_KEY, String(value));
      await configure(value);
      enabled.value = value;
      failed.value = false;
    } catch {
      enabled.value = false;
      failed.value = true;
      try { localStorage.setItem(ENABLED_KEY, "false"); } catch { /* session remains disabled */ }
      await configure(false).catch(() => {});
    } finally { writes--; pending.value = writes > 0; }
  });
}

/** First-time setup is an explicit account-selection action, never a mount,
 * refresh, permission grant or restoration of previously saved accounts.
 */
function enableOnFirstSelection(): Promise<void> {
  try {
    if (!enabled.value && writes === 0 && accountIds.length > 0 && localStorage.getItem(ENABLED_KEY) === null) {
      return setEnabled(true);
    }
  } catch { /* An unreadable saved choice is not permission to enable reminders. */ }
  return Promise.resolve();
}

function setLeadSeconds(value: number): Promise<void> {
  if (!REMINDER_TIMINGS.some((option) => option === value)) return Promise.resolve();
  writes++;
  pending.value = true;
  return serialize(async () => {
    try {
      if (enabled.value && (!started || unlisteners.length !== 2)) throw new Error("Reminder listeners are not ready");
      localStorage.setItem(REMINDER_LEAD_SECONDS_KEY, String(value));
      leadSeconds.value = value;
      await configure(enabled.value);
      failed.value = false;
    } catch {
      await pauseAfterFailure();
    } finally { writes--; pending.value = writes > 0; }
  });
}

function selectionChanged(event: Event): void {
  const detail: unknown = (event as CustomEvent).detail;
  if (!Array.isArray(detail) || detail.length > 2048 || !detail.every((id) => typeof id === "string" && id.length <= 4096)) return;
  accountIds = [...detail];
  if (started) void serialize(synchronize);
}

async function offer(due: Due): Promise<void> {
  if (!enabled.value || !due || typeof due.token !== "string" || !/^[a-f0-9]{32}$/.test(due.token)
    || !/^[a-f0-9]{64}$/.test(due.key) || !Number.isSafeInteger(due.expires_ms)
    || due.expires_ms <= Date.now() || due.expires_ms > Date.now() + 172_800_000) return;
  try {
    const saved = handled().filter((item) => item.key !== due.key);
    saved.push({ key: due.key, expires_ms: due.expires_ms });
    if (saved.length > 1024) throw new Error("Reminder preference limit");
    // Persist before asking native code to show anything. No titles, invite
    // URLs, email labels, or raw event IDs are written here.
    localStorage.setItem(HANDLED_REMINDERS_KEY, JSON.stringify(saved));
  } catch {
    await pauseAfterFailure();
    return;
  }
  // An offer can expire/cancel while the acknowledgment is queued. That is a
  // normal stale-offer rejection, not a reason to expose native error content.
  try {
    await invoke("present_calendar_reminder", { token: due.token });
  } catch (reason) {
    if (reason !== "Reminder is no longer available.") await pauseAfterFailure();
  }
}

/** Called once by the routed main window, never by the reminder/controller.
 * Register listeners before arming native monitoring so no first offer is lost.
 */
export async function startCalendarReminders(): Promise<() => void> {
  if (started) return () => {};
  started = true;
  try {
    const registered = await Promise.allSettled([
      listen<Due>("murmur://calendar-reminder-due", ({ payload }) => { void serialize(() => offer(payload)); }),
      listen("murmur://calendar-reminder-error", () => { void serialize(pauseAfterFailure); }),
    ]);
    unlisteners = registered.flatMap((result) => result.status === "fulfilled" ? [result.value] : []);
    if (registered.some((result) => result.status === "rejected")) throw new Error("Reminder listener unavailable");
    window.addEventListener(CALENDAR_ACCOUNT_SELECTION_CHANGED, selectionChanged);
    accountIds = readCalendarSelection().ids;
    await serialize(synchronize);
  } catch {
    for (const unlisten of unlisteners.splice(0)) unlisten();
    started = false;
    await pauseAfterFailure();
  }
  return () => {
    started = false;
    window.removeEventListener(CALENDAR_ACCOUNT_SELECTION_CHANGED, selectionChanged);
    for (const unlisten of unlisteners.splice(0)) unlisten();
    void serialize(() => configure(false).catch(() => {}));
  };
}

export function useCalendarReminders() {
  return { enabled, leadSeconds, pending, failed, setEnabled, enableOnFirstSelection, setLeadSeconds };
}
