import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import CalendarConnection from "./CalendarConnection.vue";
import { CALENDAR_SELECTION_KEY, type CalendarSnapshot } from "@/composables/useCalendarConnection";
import en from "@/locales/en.json";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
let wrapper: VueWrapper;
const snapshot: CalendarSnapshot = {
  accounts: [{ id: "a1", email: "work@example.com" }, { id: "a2", email: "personal@example.com" }],
  meetings: [
    { id: "m1", account_id: "a1", calendar_id: "clients", title: "<script>private title</script>", start_ms: Date.UTC(2026, 9, 26, 15), end_ms: Date.UTC(2026, 9, 26, 16), provider: "GoogleMeet" },
    { id: "m2", account_id: "a1", calendar_id: "team", title: "Team meeting", start_ms: Date.UTC(2026, 9, 26, 16), end_ms: Date.UTC(2026, 9, 26, 17), provider: "Teams" },
    { id: "m3", account_id: "a2", calendar_id: "family", title: "Personal meeting", start_ms: Date.UTC(2026, 9, 26, 15), end_ms: Date.UTC(2026, 9, 26, 16), provider: "Zoom" },
  ],
};
function render() {
  wrapper = mount(CalendarConnection, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
}
beforeEach(() => { invoke.mockReset(); localStorage.removeItem(CALENDAR_SELECTION_KEY); });
afterEach(() => { wrapper?.unmount(); });
function button(text: string) { return wrapper.findAll("button").find((b) => b.text() === text)!; }
function account(email: string) { return wrapper.findAll("label").find((label) => label.text() === email)!.get('[role="checkbox"]'); }
async function openPreview() { await button(en.calendarConnection.preview).trigger("click"); await flushPromises(); }
function allowed() {
  invoke.mockImplementation(async (command: string, args?: { accountIds: string[] }) => {
    if (command === "calendar_permission") return "granted";
    if (command === "read_calendar_snapshot") return { ...snapshot, meetings: snapshot.meetings.filter((meeting) => args?.accountIds.includes(meeting.account_id)) };
    throw new Error("Unexpected command");
  });
}

describe("calendar email account settings", () => {
  it("matches standard title/subtext spacing and keeps privacy collapsed until requested", async () => {
    render();
    const titleBlock = wrapper.get("h3").element.parentElement!;
    expect(titleBlock.classList.contains("gap-0.5")).toBe(true);
    expect(titleBlock.textContent).toContain(en.calendarConnection.description);
    expect(button(en.calendarConnection.privacy).attributes("aria-expanded")).toBe("false");
    expect(wrapper.text()).not.toContain(en.calendarConnection.permissionHelp);
    expect(wrapper.text()).toContain(en.calendarConnection.checkpoint);
    expect(button("Disconnect")).toBeUndefined();
    await button(en.calendarConnection.privacy).trigger("click"); await flushPromises();
    expect(button(en.calendarConnection.privacy).attributes("aria-expanded")).toBe("true");
    expect(wrapper.text()).toContain(en.calendarConnection.permissionHelp);
    expect(wrapper.get(".calendar-disclosure").attributes("data-state")).toBe("open");
    await button(en.calendarConnection.privacy).trigger("click"); await flushPromises();
    expect(button(en.calendarConnection.privacy).attributes("aria-expanded")).toBe("false");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("shows one email checkbox per account and no calendar hierarchy", async () => {
    allowed(); render(); await button("Connect calendars").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("0 of 2 accounts selected");
    expect(wrapper.findAll("label").map((label) => label.text())).toEqual(["work@example.com", "personal@example.com"]);
    expect(wrapper.findAll('[role="checkbox"]')).toHaveLength(2);
    expect(wrapper.findAll('input[type="checkbox"]')).toHaveLength(0);
    expect(account("work@example.com").attributes("aria-checked")).toBe("false");
    expect(account("personal@example.com").attributes("aria-checked")).toBe("false");
    expect(button(en.calendarConnection.preview).attributes("aria-expanded")).toBe("false");
    await openPreview();
    expect(wrapper.text()).toContain(en.calendarConnection.chooseCalendars);
    expect(wrapper.text()).not.toContain("private title");
    expect(wrapper.text()).not.toContain("Holidays");
    expect(wrapper.text()).not.toContain("Birthdays");
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: [] });
  });

  it("lets Refresh use its normal content width rather than a fixed/minimum width", async () => {
    allowed(); render(); await button("Connect calendars").trigger("click"); await flushPromises();
    expect(button("Refresh").classes().some((name) => /^(min-w-|w-|flex-1$)/.test(name))).toBe(false);
  });

  it("selects one account for all its calendars and excludes other accounts", async () => {
    allowed(); render(); await button("Connect calendars").trigger("click"); await flushPromises();
    await wrapper.findAll("label").find((label) => label.text() === "work@example.com")!.trigger("click"); await flushPromises();
    expect(wrapper.text()).not.toContain("Team meeting");
    await openPreview();
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: ["a1"] });
    expect(wrapper.text()).toContain("1 of 2 accounts selected");
    expect(wrapper.text()).toContain("Google Meet");
    expect(wrapper.text()).toContain("Team meeting");
    expect(wrapper.text()).not.toContain("Personal meeting");
    expect(account("personal@example.com").attributes("aria-checked")).toBe("false");
    expect(account("work@example.com").attributes("aria-checked")).toBe("true");
    expect(wrapper.find("script").exists()).toBe(false);
    await account("work@example.com").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("0 of 2 accounts selected");
    expect(wrapper.text()).not.toContain("private title");
    expect(wrapper.text()).not.toContain("Team meeting");
  });

  it("restores account choices and disconnects without retaining private data", async () => {
    localStorage.setItem(CALENDAR_SELECTION_KEY, JSON.stringify(["a1"])); allowed(); render();
    await button("Connect calendars").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("1 of 2 accounts selected");
    expect(wrapper.text()).not.toContain("<script>private title</script>");
    await openPreview();
    expect(wrapper.text()).toContain("<script>private title</script>");
    expect(wrapper.find("script").exists()).toBe(false);
    await button("Disconnect").trigger("click");
    expect(wrapper.text()).not.toContain("work@example.com");
    expect(wrapper.text()).not.toContain("private title");
    expect(button("Disconnect")).toBeUndefined();
    expect(JSON.parse(localStorage.getItem(CALENDAR_SELECTION_KEY)!)).toEqual(["a1"]);
  });

  it("does not insert a loading row or replace checkbox elements while selection is pending", async () => {
    allowed(); render(); await button("Connect calendars").trigger("click"); await flushPromises();
    const status = wrapper.get('[role="status"]').element;
    const refresh = button("Refresh").element;
    const refreshClasses = button("Refresh").classes();
    expect(refreshClasses).toContain("disabled:opacity-100");
    expect(refreshClasses).not.toContain("disabled:opacity-50");
    const checkbox = account("work@example.com").element;
    const nodes = wrapper.get("section").element.children.length;
    let resolve!: (value: CalendarSnapshot) => void;
    invoke.mockResolvedValueOnce("granted").mockReturnValueOnce(new Promise<CalendarSnapshot>((r) => { resolve = r; }));
    await account("work@example.com").trigger("click"); await flushPromises();
    expect(wrapper.get('[role="status"]').element).toBe(status);
    expect(wrapper.get("section").element.children.length).toBe(nodes);
    expect(account("work@example.com").element).toBe(checkbox);
    expect(account("work@example.com").attributes("aria-checked")).toBe("true");
    expect(button("Refresh").attributes("disabled")).toBeDefined();
    expect(button("Refresh").element).toBe(refresh);
    expect(button("Refresh").classes()).toEqual(refreshClasses);
    expect(wrapper.get('[role="status"]').text()).toBe(en.calendarConnection.checkpoint);
    expect(wrapper.text()).not.toContain(en.calendarConnection.connecting);
    resolve({ ...snapshot, meetings: [] }); await flushPromises();
    expect(wrapper.get('[role="status"]').element).toBe(status);
    expect(wrapper.get("section").element.children.length).toBe(nodes);
    expect(account("work@example.com").element).toBe(checkbox);
    expect(button("Refresh").classes()).toEqual(refreshClasses);
    expect(button("Refresh").attributes("disabled")).toBeUndefined();
    expect(wrapper.get('[role="status"]').text()).toBe(en.calendarConnection.checkpoint);
  });

  it("offers the exact Calendar privacy pane after denial", async () => {
    invoke.mockResolvedValueOnce("denied"); render(); await button("Connect calendars").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain(en.calendarConnection.denied);
    await button("Open System Settings").trigger("click");
    expect(invoke).toHaveBeenCalledWith("open_privacy_pane", { pane: "calendars" });
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain("read_calendar_snapshot");
  });

  it("keeps Disconnect hidden until connection succeeds, including during a failed permission request", async () => {
    let resolve!: (value: string) => void;
    invoke.mockResolvedValueOnce("not-determined").mockReturnValueOnce(new Promise<string>((r) => { resolve = r; })); render();
    const connect = button("Connect calendars").element;
    expect(button("Disconnect")).toBeUndefined();
    await button("Connect calendars").trigger("click"); await flushPromises();
    expect(button("Disconnect")).toBeUndefined();
    expect(button(en.calendarConnection.connecting).element).toBe(connect);
    expect(wrapper.get('[role="status"]').text()).toBe(en.calendarConnection.connecting);
    resolve("not-determined"); await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe(en.calendarConnection.failed);
    expect(button("Disconnect")).toBeUndefined();
    allowed(); await button("Connect calendars").trigger("click"); await flushPromises();
    expect(button("Disconnect").exists()).toBe(true);
  });

  it("distinguishes no identified email accounts from a failed connection", async () => {
    invoke.mockResolvedValueOnce("granted").mockResolvedValueOnce({ accounts: [], meetings: [] }); render();
    await button("Connect calendars").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain(en.calendarConnection.noCalendars);
    invoke.mockResolvedValueOnce("granted").mockRejectedValueOnce(new Error("Private native details"));
    await button("Refresh").trigger("click"); await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe(en.calendarConnection.failed);
    expect(wrapper.text()).not.toContain("Private native details");
  });
});
