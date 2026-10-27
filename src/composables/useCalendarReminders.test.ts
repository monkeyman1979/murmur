import { flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mock = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), events: new Map<string, (event: { payload: unknown }) => void>() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mock.listen }));
const ENABLED = "murmur:calendar-reminders-enabled-v1";
const SELECTION = "murmur:calendar-account-selection-v1";
const HANDLED = "murmur:calendar-reminders-handled-v1";
const TIMING = "murmur:calendar-reminders-lead-seconds-v1";
let stop: (() => void) | undefined;
let api: typeof import("./useCalendarReminders");
beforeEach(async () => {
  vi.resetModules(); mock.invoke.mockReset(); mock.listen.mockReset(); mock.events.clear();
  for (const key of [ENABLED, SELECTION, HANDLED, TIMING]) localStorage.removeItem(key);
  mock.invoke.mockResolvedValue(undefined);
  mock.listen.mockImplementation(async (name, callback) => { mock.events.set(name, callback); return () => mock.events.delete(name); });
});
afterEach(async () => { stop?.(); stop = undefined; await flushPromises(); vi.restoreAllMocks(); });
async function start() { api = await import("./useCalendarReminders"); stop = await api.startCalendarReminders(); }
async function event(name: string, payload: unknown) { mock.events.get(name)!({ payload }); await flushPromises(); }
function due() { return { token: "1".repeat(32), key: "a".repeat(64), expires_ms: Date.now() + 86_400_000 }; }

describe("calendar reminders opt-in and durable acknowledgment", () => {
  it("starts disabled and registers listeners before configuring native monitoring", async () => {
    await start();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(mock.invoke).toHaveBeenCalledWith("configure_calendar_reminders", { enabled: false, accountIds: [], leadSeconds: 15, handled: [] });
    expect(mock.listen.mock.invocationCallOrder[1]).toBeLessThan(mock.invoke.mock.invocationCallOrder[0]);
    expect(mock.invoke.mock.calls.map(([command]) => command)).not.toContain("read_calendar_snapshot");
  });

  it("enables first-time setup only after an explicit account-selection action", async () => {
    await start();
    await api.useCalendarReminders().enableOnFirstSelection();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(localStorage.getItem(ENABLED)).toBeNull();
    localStorage.setItem(SELECTION, JSON.stringify(["account"]));
    window.dispatchEvent(new CustomEvent("murmur:calendar-account-selection-changed", { detail: ["account"] }));
    await flushPromises();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    await api.useCalendarReminders().enableOnFirstSelection();
    expect(localStorage.getItem(ENABLED)).toBe("true");
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", {
      enabled: true, accountIds: ["account"], leadSeconds: 15, handled: [],
    });
  });

  it("never overrides a saved Off choice, including after an app restart", async () => {
    localStorage.setItem(ENABLED, "false");
    localStorage.setItem(SELECTION, JSON.stringify(["account"]));
    await start();
    await api.useCalendarReminders().enableOnFirstSelection();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    stop!(); stop = undefined; await flushPromises(); vi.resetModules(); await start();
    await api.useCalendarReminders().enableOnFirstSelection();
    expect(localStorage.getItem(ENABLED)).toBe("false");
    expect(mock.invoke).not.toHaveBeenCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true }));
  });

  it("does not auto-enable restored accounts or overwrite an unreadable saved choice", async () => {
    localStorage.setItem(SELECTION, JSON.stringify(["account"]));
    await start();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    const get = Storage.prototype.getItem;
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(function (this: Storage, key) {
      if (key === ENABLED) throw new Error("Storage unavailable");
      return get.call(this, key);
    });
    await api.useCalendarReminders().enableOnFirstSelection();
    expect(mock.invoke).not.toHaveBeenCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true }));
  });

  it("fails closed when first-selection consent cannot be saved", async () => {
    localStorage.setItem(SELECTION, JSON.stringify(["account"]));
    await start();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    await api.useCalendarReminders().enableOnFirstSelection();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(api.useCalendarReminders().failed.value).toBe(true);
    expect(mock.invoke).not.toHaveBeenCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true }));
  });

  it("does not override a pending explicit Off choice with first-selection setup", async () => {
    localStorage.setItem(SELECTION, JSON.stringify(["account"]));
    await start();
    const disable = api.useCalendarReminders().setEnabled(false);
    await api.useCalendarReminders().enableOnFirstSelection();
    await disable;
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(localStorage.getItem(ENABLED)).toBe("false");
    expect(mock.invoke).not.toHaveBeenCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true }));
  });

  it("restores explicit opt-in, selected account IDs, and unexpired handled keys", async () => {
    const item = due();
    localStorage.setItem(ENABLED, "true"); localStorage.setItem(SELECTION, JSON.stringify(["account"]));
    localStorage.setItem(HANDLED, JSON.stringify([{ key: item.key, expires_ms: item.expires_ms }, { key: "b".repeat(64), expires_ms: Date.now() - 1 }]));
    await start();
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", { enabled: true, accountIds: ["account"], leadSeconds: 15, handled: [{ key: item.key, expires_ms: item.expires_ms }] });
  });

  it.each([0, 15, 30, 60])("persists and restores %i-second reminder timing", async (seconds) => {
    await start();
    await api.useCalendarReminders().setLeadSeconds(seconds);
    expect(localStorage.getItem(TIMING)).toBe(String(seconds));
    expect(api.useCalendarReminders().leadSeconds.value).toBe(seconds);
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", expect.objectContaining({ leadSeconds: seconds, enabled: false }));
    stop!(); stop = undefined; await flushPromises(); vi.resetModules(); await start();
    expect(api.useCalendarReminders().leadSeconds.value).toBe(seconds);
  });

  it("reschedules enabled reminders immediately and retains handled occurrences", async () => {
    const item = due();
    localStorage.setItem(HANDLED, JSON.stringify([{ key: item.key, expires_ms: item.expires_ms }]));
    await start(); await api.useCalendarReminders().setEnabled(true);
    await api.useCalendarReminders().setLeadSeconds(30);
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", expect.objectContaining({
      enabled: true, leadSeconds: 30, handled: [{ key: item.key, expires_ms: item.expires_ms }],
    }));
  });

  it.each(["invalid", "", "-1", "16", "Infinity"])("defaults invalid saved timing %s to 15 seconds", async (value) => {
    localStorage.setItem(TIMING, value); await start();
    expect(api.useCalendarReminders().leadSeconds.value).toBe(15);
  });

  it("rejects unsupported timing and fails closed if a valid choice cannot be saved", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true);
    await api.useCalendarReminders().setLeadSeconds(17);
    expect(api.useCalendarReminders().leadSeconds.value).toBe(15);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    await api.useCalendarReminders().setLeadSeconds(30);
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(api.useCalendarReminders().failed.value).toBe(true);
  });

  it("persists the occurrence hash before allowing native code to show the prompt", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true);
    const item = due();
    mock.invoke.mockImplementation(async (command) => {
      if (command === "present_calendar_reminder") {
        expect(JSON.parse(localStorage.getItem(HANDLED)!)).toEqual([{ key: item.key, expires_ms: item.expires_ms }]);
      }
    });
    await event("murmur://calendar-reminder-due", item);
    expect(mock.invoke).toHaveBeenCalledWith("present_calendar_reminder", { token: item.token });
    expect(localStorage.getItem(HANDLED)).not.toContain("token");
    expect(mock.invoke.mock.calls.map(([command]) => command)).not.toContain("meeting_recorder_start");
  });

  it("does not show offers while disabled", async () => {
    await start(); await event("murmur://calendar-reminder-due", due());
    expect(mock.invoke).not.toHaveBeenCalledWith("present_calendar_reminder", expect.anything());
    expect(localStorage.getItem(HANDLED)).toBeNull();
  });

  it("fails closed if the durable acknowledgment cannot be saved", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    await event("murmur://calendar-reminder-due", due());
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(api.useCalendarReminders().failed.value).toBe(true);
    expect(mock.invoke).not.toHaveBeenCalledWith("present_calendar_reminder", expect.anything());
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", { enabled: false, accountIds: [], leadSeconds: 15, handled: [] });
  });

  it("does not arm monitoring before the main-window listeners are ready", async () => {
    api = await import("./useCalendarReminders");
    await api.useCalendarReminders().setEnabled(true);
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(mock.invoke).not.toHaveBeenCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true }));
  });

  it("does not arm reminders if local consent cannot be saved", async () => {
    await start();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    await api.useCalendarReminders().setEnabled(true);
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(mock.invoke).not.toHaveBeenCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true }));
  });

  it("updates monitoring when account choices change without requesting calendar access", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true);
    window.dispatchEvent(new CustomEvent("murmur:calendar-account-selection-changed", { detail: ["new-account"] })); await flushPromises();
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", { enabled: true, accountIds: ["new-account"], leadSeconds: 15, handled: [] });
    expect(mock.invoke.mock.calls.map(([command]) => command)).not.toContain("request_calendar_access");
  });

  it("retains at-most-once acknowledgment across a fresh module/app session", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true); const item = due();
    await event("murmur://calendar-reminder-due", item);
    stop!(); stop = undefined; await flushPromises(); vi.resetModules(); await start();
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: true, handled: [{ key: item.key, expires_ms: item.expires_ms }] }));
  });

  it("pauses rather than silently monitoring with malformed handled preferences", async () => {
    localStorage.setItem(ENABLED, "true"); localStorage.setItem(HANDLED, "broken"); await start();
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(api.useCalendarReminders().failed.value).toBe(true);
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: false }));
  });

  it("pauses if native code cannot display the reminder window", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true);
    mock.invoke.mockRejectedValueOnce("Reminder window is unavailable.");
    await event("murmur://calendar-reminder-due", due());
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(api.useCalendarReminders().failed.value).toBe(true);
  });

  it("treats a canceled/expired offer as stale rather than a monitoring failure", async () => {
    await start(); await api.useCalendarReminders().setEnabled(true);
    mock.invoke.mockRejectedValueOnce("Reminder is no longer available.");
    await event("murmur://calendar-reminder-due", due());
    expect(api.useCalendarReminders().enabled.value).toBe(true);
    expect(api.useCalendarReminders().failed.value).toBe(false);
  });

  it("does not re-enable after a queued disable during an enable operation", async () => {
    await start(); let resolve!: () => void;
    mock.invoke.mockImplementationOnce(() => new Promise<void>((r) => { resolve = r; }));
    const enable = api.useCalendarReminders().setEnabled(true); await flushPromises();
    const disable = api.useCalendarReminders().setEnabled(false); resolve(); await Promise.all([enable, disable]);
    expect(api.useCalendarReminders().enabled.value).toBe(false);
    expect(mock.invoke).toHaveBeenLastCalledWith("configure_calendar_reminders", expect.objectContaining({ enabled: false }));
  });
});
