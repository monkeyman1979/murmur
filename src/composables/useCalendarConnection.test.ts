import { defineComponent } from "vue";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CALENDAR_SELECTION_KEY, useCalendarConnection, type CalendarSnapshot } from "./useCalendarConnection";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
let wrapper: VueWrapper;
let connection: ReturnType<typeof useCalendarConnection>;
const snapshot: CalendarSnapshot = {
  accounts: [{ id: "work", email: "work@example.com" }, { id: "personal", email: "personal@example.com" }],
  meetings: [
    { id: "event:1", account_id: "work", calendar_id: "clients", title: "Work meeting", start_ms: 1000, end_ms: 2000, provider: "GoogleMeet" },
    { id: "event:2", account_id: "personal", calendar_id: "family", title: "Personal meeting", start_ms: 1000, end_ms: 2000, provider: "Zoom" },
  ],
};
function render() {
  wrapper = mount(defineComponent({ setup() { connection = useCalendarConnection(); return () => null; } }));
}
beforeEach(() => { invoke.mockReset(); localStorage.removeItem(CALENDAR_SELECTION_KEY); localStorage.removeItem("murmur:calendar-selection-v1"); });
afterEach(() => { wrapper?.unmount(); vi.restoreAllMocks(); });
function allowed() {
  invoke.mockImplementation(async (command: string, args?: { accountIds: string[] }) => {
    if (command === "calendar_permission") return "granted";
    if (command === "read_calendar_snapshot") return { ...snapshot, meetings: snapshot.meetings.filter((meeting) => args?.accountIds.includes(meeting.account_id)) };
    throw new Error("Unexpected command");
  });
}

describe("calendar account connection consent, selection and lifecycle", () => {
  it("defaults to no accounts and never reads events or requests access on mount", async () => {
    render(); await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(connection.snapshot.value).toBeNull();
    expect(connection.selectedIds.value).toEqual([]);
  });

  it("does not silently expand previous per-calendar choices into a whole-account opt-in", () => {
    localStorage.setItem("murmur:calendar-selection-v1", JSON.stringify(["clients"])); render();
    expect(connection.selectedIds.value).toEqual([]);
    expect(localStorage.getItem("murmur:calendar-selection-v1")).toBe(JSON.stringify(["clients"]));
  });

  it("requests access only after explicit connect and sends no selected accounts after a grant", async () => {
    invoke.mockResolvedValueOnce("not-determined").mockResolvedValueOnce("granted").mockResolvedValueOnce(snapshot); render(); await connection.connect();
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(["calendar_permission", "request_calendar_access", "read_calendar_snapshot"]);
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: [] });
    expect(connection.meetings.value).toEqual([]);
    expect(connection.pending.value).toBe(false);
  });

  it("uses saved account choices only after explicit connection and filters unselected results defensively", async () => {
    localStorage.setItem(CALENDAR_SELECTION_KEY, JSON.stringify(["work"]));
    invoke.mockResolvedValueOnce("granted").mockResolvedValueOnce(snapshot); render();
    expect(invoke).not.toHaveBeenCalled(); await connection.connect();
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: ["work"] });
    expect(connection.meetings.value.map((meeting) => meeting.id)).toEqual(["event:1"]);
    expect(connection.selectedCount.value).toBe(1);
  });

  it("saves only account IDs and narrows the native query without storing emails or meeting content", async () => {
    allowed(); render(); await connection.connect(); await connection.setAccount("unknown", true);
    expect(connection.selectedIds.value).toEqual([]);
    await connection.setAccount("work", true);
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: ["work"] });
    expect(JSON.parse(localStorage.getItem(CALENDAR_SELECTION_KEY)!)).toEqual(["work"]);
    expect(localStorage.getItem(CALENDAR_SELECTION_KEY)).not.toContain("@");
    expect(localStorage.getItem(CALENDAR_SELECTION_KEY)).not.toContain("Work meeting");
    expect(connection.meetings.value.map((meeting) => meeting.title)).toEqual(["Work meeting"]);
    await connection.setAccount("work", false);
    expect(connection.selectedIds.value).toEqual([]);
    expect(connection.meetings.value).toEqual([]);
  });

  it("retains choices across disconnect and remount without automatically reconnecting", async () => {
    allowed(); render(); await connection.connect(); await connection.setAccount("work", true);
    connection.disconnect(); expect(connection.snapshot.value).toBeNull();
    wrapper.unmount(); invoke.mockClear(); render();
    expect(connection.selectedIds.value).toEqual(["work"]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("keeps a temporarily unavailable account choice without selecting another account", async () => {
    localStorage.setItem(CALENDAR_SELECTION_KEY, JSON.stringify(["missing"])); allowed(); render(); await connection.connect();
    expect(connection.selectedCount.value).toBe(0);
    expect(connection.meetings.value).toEqual([]);
    expect(connection.selectedIds.value).toEqual(["missing"]);
  });

  it.each(["not json", JSON.stringify([123]), JSON.stringify({ all: true })])("fails closed for malformed preferences: %s", (raw) => {
    localStorage.setItem(CALENDAR_SELECTION_KEY, raw); render();
    expect(connection.selectedIds.value).toEqual([]);
    expect(connection.selectionStorageFailed.value).toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("keeps session choices and reports when preferences cannot be saved", async () => {
    allowed(); render(); await connection.connect();
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
    await connection.setAccount("work", true);
    expect(connection.selectedIds.value).toEqual(["work"]);
    expect(connection.selectionStorageFailed.value).toBe(true);
    expect(connection.meetings.value).toHaveLength(1);
  });

  it.each(["denied", "restricted", "unsupported"])("does not read account data for %s permission", async (permission) => {
    invoke.mockResolvedValueOnce(permission); render(); await connection.connect();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(connection.permission.value).toBe(permission);
    expect(connection.snapshot.value).toBeNull();
  });

  it("does not read events when the OS prompt is denied", async () => {
    invoke.mockResolvedValueOnce("not-determined").mockResolvedValueOnce("denied"); render(); await connection.connect();
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain("read_calendar_snapshot");
    expect(connection.permission.value).toBe("denied");
  });

  it("reports unchanged authorization rather than silently resetting the button", async () => {
    invoke.mockResolvedValueOnce("not-determined").mockResolvedValueOnce("not-determined"); render(); await connection.connect();
    expect(connection.failed.value).toBe(true);
    expect(connection.pending.value).toBe(false);
  });

  it("requests full access rather than treating write-only access as readable", async () => {
    invoke.mockResolvedValueOnce("write-only").mockResolvedValueOnce("granted").mockResolvedValueOnce(snapshot); render(); await connection.connect();
    expect(invoke).toHaveBeenCalledWith("request_calendar_access");
  });

  it("rechecks permission when an account is selected without automatically prompting after revocation", async () => {
    allowed(); render(); await connection.connect(); invoke.mockResolvedValueOnce("not-determined");
    await connection.setAccount("work", true);
    expect(connection.snapshot.value).toBeNull();
    expect(connection.permission.value).toBe("not-determined");
    expect(invoke).not.toHaveBeenCalledWith("request_calendar_access");
  });

  it("suppresses concurrent connects", async () => {
    let resolve!: (value: string) => void;
    invoke.mockReturnValueOnce(new Promise<string>((r) => { resolve = r; })); render();
    const first = connection.connect(); await connection.connect();
    expect(invoke).toHaveBeenCalledTimes(1); resolve("denied"); await first;
  });

  it.each(["disconnect", "unmount"])("does not publish a late snapshot after %s", async (operation) => {
    let resolve!: (value: CalendarSnapshot) => void;
    invoke.mockResolvedValueOnce("granted").mockReturnValueOnce(new Promise<CalendarSnapshot>((r) => { resolve = r; })); render();
    const work = connection.connect(); await flushPromises();
    if (operation === "disconnect") connection.disconnect(); else wrapper.unmount();
    resolve(snapshot); await work;
    expect(connection.snapshot.value).toBeNull(); expect(connection.pending.value).toBe(false);
  });

  it("keeps the account picker steady while immediately clearing stale meetings", async () => {
    localStorage.setItem(CALENDAR_SELECTION_KEY, JSON.stringify(["work"])); allowed(); render(); await connection.connect();
    let resolve!: (value: CalendarSnapshot) => void;
    invoke.mockResolvedValueOnce("granted").mockReturnValueOnce(new Promise<CalendarSnapshot>((r) => { resolve = r; }));
    const changing = connection.setAccount("work", false); await flushPromises();
    expect(connection.snapshot.value?.accounts).toHaveLength(2);
    expect(connection.meetings.value).toEqual([]);
    expect(connection.pending.value).toBe(true);
    connection.disconnect(); resolve(snapshot); await changing;
    expect(connection.snapshot.value).toBeNull();
  });

  it("clears snapshots on failure without logging native exceptions or persisting event data", async () => {
    const log = vi.spyOn(console, "error").mockImplementation(() => {});
    const storage = vi.spyOn(Storage.prototype, "setItem");
    allowed(); render(); await connection.connect();
    invoke.mockResolvedValueOnce("granted").mockRejectedValueOnce(new Error("Private appointment details"));
    await connection.connect();
    expect(connection.snapshot.value).toBeNull(); expect(connection.failed.value).toBe(true);
    expect(log).not.toHaveBeenCalled(); expect(storage).not.toHaveBeenCalled();
  });
});
