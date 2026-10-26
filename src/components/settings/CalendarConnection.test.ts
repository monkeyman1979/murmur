import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { ref, type Ref } from "vue";
import CalendarConnection from "./CalendarConnection.vue";
import { CALENDAR_CONNECTED_KEY, CALENDAR_SELECTION_KEY, type CalendarSnapshot } from "@/composables/useCalendarConnection";
import en from "@/locales/en.json";
import { Select } from "@/components/ui/select";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
const reminders = vi.hoisted(() => ({ enabled: null as unknown as Ref<boolean>, pending: null as unknown as Ref<boolean>, failed: null as unknown as Ref<boolean>, leadSeconds: null as unknown as Ref<number>, setLeadSeconds: vi.fn(), setEnabled: vi.fn() }));
vi.mock("@/composables/useCalendarReminders", () => ({ REMINDER_TIMINGS: [0, 15, 30, 60], useCalendarReminders: () => reminders }));
let wrapper: VueWrapper;
const snapshot: CalendarSnapshot = {
  accounts: [{ id: "a1", email: "work@example.com" }, { id: "a2", email: "personal@example.com" }],
  meetings: [{ id: "m1", account_id: "a1", calendar_id: "clients", title: "<script>private title</script>", start_ms: 1000, end_ms: 2000, provider: "GoogleMeet" }],
};
function render() {
  wrapper = mount(CalendarConnection, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
}
beforeEach(() => {
  invoke.mockReset(); localStorage.removeItem(CALENDAR_SELECTION_KEY); localStorage.removeItem(CALENDAR_CONNECTED_KEY);
  reminders.enabled = ref(false); reminders.pending = ref(false); reminders.failed = ref(false);
  reminders.leadSeconds = ref(15);
  reminders.setLeadSeconds.mockReset().mockImplementation(async (value) => { reminders.leadSeconds.value = value; });
  reminders.setEnabled.mockReset().mockImplementation(async (enabled) => { reminders.enabled.value = enabled; });
});
afterEach(() => { wrapper?.unmount(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
function button(text: string) { return wrapper.findAll("button").find((b) => b.text() === text)!; }
function account(email: string) { return wrapper.findAll("label").find((label) => label.text() === email)!.get('[role="checkbox"]'); }
function allowed() {
  invoke.mockImplementation(async (command: string) => {
    if (command === "calendar_permission") return "granted";
    if (command === "read_calendar_snapshot") return snapshot;
    throw new Error("Unexpected command");
  });
}
async function connect() {
  await button(en.calendarConnection.connect).trigger("click"); await flushPromises();
}
const disconnectLabel = en.calendarConnection.disconnect;
const manageLabel = en.calendarConnection.manageAccounts;

describe("calendar email account settings", () => {
  it("matches standard title/subtext spacing and keeps privacy collapsed until requested", async () => {
    render();
    const titleBlock = wrapper.get("h3").element.parentElement!;
    expect(titleBlock.classList.contains("gap-0.5")).toBe(true);
    expect(titleBlock.textContent).toContain(en.calendarConnection.description);
    expect(en.calendarConnection.description).toContain("Apple Calendar");
    expect(button(en.calendarConnection.privacy).attributes("aria-expanded")).toBe("false");
    expect(wrapper.get('[role="switch"]').element.compareDocumentPosition(button(en.calendarConnection.privacy).element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(wrapper.text()).not.toContain(en.calendarConnection.permissionHelp);
    expect(wrapper.text()).not.toContain("Reminders aren’t enabled yet.");
    expect(wrapper.find('[role="status"]').exists()).toBe(false);
    expect(button(disconnectLabel)).toBeUndefined();
    await button(en.calendarConnection.privacy).trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain(en.calendarConnection.permissionHelp);
    expect(wrapper.get(".calendar-disclosure").attributes("data-state")).toBe("open");
    await button(en.calendarConnection.privacy).trigger("click"); await flushPromises();
    expect(button(en.calendarConnection.privacy).attributes("aria-expanded")).toBe("false");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("shows one email checkbox per account without previewing meetings or exposing private titles", async () => {
    allowed(); render(); await connect();
    expect(wrapper.text()).toContain("0 of 2 accounts selected");
    expect(wrapper.findAll("label").map((label) => label.text())).toEqual(["work@example.com", "personal@example.com"]);
    expect(wrapper.findAll('[role="checkbox"]')).toHaveLength(2);
    expect(wrapper.findAll('input[type="checkbox"]')).toHaveLength(0);
    expect(account("work@example.com").attributes("aria-checked")).toBe("false");
    expect(account("personal@example.com").attributes("aria-checked")).toBe("false");
    expect(wrapper.text()).not.toContain("Preview meetings");
    expect(wrapper.text()).not.toContain("private title");
    expect(wrapper.find("script").exists()).toBe(false);
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: [] });
  });

  it("keeps only Manage accounts in the header and puts Refresh and Disconnect after account selection", async () => {
    allowed(); render(); await connect();
    expect(wrapper.get("header").findAll("button").map((b) => b.text())).toEqual([manageLabel]);
    const panel = wrapper.get(`#${button(manageLabel).attributes("aria-controls")}`);
    const refresh = button(en.calendarConnection.refresh);
    const disconnect = button(disconnectLabel);
    expect(panel.element.contains(refresh.element)).toBe(true);
    expect(panel.element.contains(disconnect.element)).toBe(true);
    expect(account("personal@example.com").element.compareDocumentPosition(refresh.element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(disconnect.attributes("class")).toContain("text-muted-foreground");
    expect(refresh.classes().some((name) => /^(min-w-|w-|flex-1$)/.test(name))).toBe(false);

    invoke.mockClear();
    await button(manageLabel).trigger("click"); await flushPromises();
    expect(button(manageLabel).attributes("aria-expanded")).toBe("false");
    expect(button(en.calendarConnection.refresh)).toBeUndefined();
    expect(button(disconnectLabel)).toBeUndefined();
    expect(wrapper.find('[role="checkbox"]').exists()).toBe(false);
    await button(manageLabel).trigger("click"); await flushPromises();
    expect(button(manageLabel).attributes("aria-expanded")).toBe("true");
    expect(button(en.calendarConnection.refresh).exists()).toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("animates both directions and retains closing content until its animation finishes", async () => {
    // These fixed animation names are already valid CSS identifiers.
    vi.stubGlobal("CSS", { escape: (value: string) => value });
    const nativeStyle = window.getComputedStyle.bind(window);
    vi.spyOn(window, "getComputedStyle").mockImplementation((element, pseudo) => {
      const style = nativeStyle(element, pseudo);
      if (element.getAttribute("data-slot") === "collapsible-content"
        && element.parentElement?.id.endsWith("-accounts")) {
        // JSDOM has no running CSS animations. Expose their state so Reka's
        // presence lifecycle can be exercised without timing-dependent sleeps.
        style.display = "block";
        style.animationName = element.getAttribute("data-state") === "open"
          ? "calendar-disclosure-open" : "calendar-disclosure-close";
      }
      return style;
    });
    allowed(); render(); await connect();
    const panel = wrapper.get(`#${button(manageLabel).attributes("aria-controls")}`);
    const content = panel.get('[data-slot="collapsible-content"]');
    expect(content.classes()).toContain("calendar-disclosure");
    expect(content.attributes("data-state")).toBe("open");
    expect(panel.classes()).toContain("-mt-3");
    expect(content.get(":scope > div").classes()).toContain("pt-3");

    invoke.mockClear();
    await button(manageLabel).trigger("click"); await flushPromises();
    expect(content.attributes("data-state")).toBe("closed");
    expect(button(en.calendarConnection.refresh).exists()).toBe(true);
    expect(button(manageLabel).attributes("aria-expanded")).toBe("false");
    await content.trigger("animationend", { animationName: "calendar-disclosure-close" });
    await flushPromises();
    expect(button(en.calendarConnection.refresh)).toBeUndefined();
    expect(wrapper.find('[role="checkbox"]').exists()).toBe(false);

    await button(manageLabel).trigger("click"); await flushPromises();
    expect(content.attributes("data-state")).toBe("open");
    expect(button(en.calendarConnection.refresh).exists()).toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("selects an account without fetching events and excludes unknown accounts", async () => {
    allowed(); render(); await connect();
    await account("work@example.com").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("1 of 2 accounts selected");
    expect(account("work@example.com").attributes("aria-checked")).toBe("true");
    expect(account("personal@example.com").attributes("aria-checked")).toBe("false");
    expect(JSON.parse(localStorage.getItem(CALENDAR_SELECTION_KEY)!)).toEqual(["a1"]);
    expect(invoke).toHaveBeenLastCalledWith("read_calendar_snapshot", { accountIds: [] });
    expect(wrapper.text()).not.toContain("private title");
    await account("work@example.com").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("0 of 2 accounts selected");
  });

  it("restores account choices and disconnects from the account panel without retaining private data", async () => {
    localStorage.setItem(CALENDAR_SELECTION_KEY, JSON.stringify(["a1"])); allowed(); render();
    expect(button(disconnectLabel)).toBeUndefined();
    await button(manageLabel).trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("1 of 2 accounts selected");
    await button(disconnectLabel).trigger("click"); await flushPromises();
    expect(wrapper.text()).not.toContain("work@example.com");
    expect(button(disconnectLabel)).toBeUndefined();
    expect(button(en.calendarConnection.refresh)).toBeUndefined();
    expect(button(en.calendarConnection.connect).exists()).toBe(true);
    expect(JSON.parse(localStorage.getItem(CALENDAR_SELECTION_KEY)!)).toEqual(["a1"]);
    expect(localStorage.getItem(CALENDAR_CONNECTED_KEY)).toBe("false");
  });

  it("keeps account rows and both management buttons stable during refresh", async () => {
    allowed(); render(); await connect();
    const status = wrapper.get('[role="status"]').element;
    const refresh = button(en.calendarConnection.refresh);
    const header = button(manageLabel).element;
    const refreshClasses = refresh.classes();
    const checkbox = account("work@example.com").element;
    const nodes = wrapper.get("section").element.children.length;
    let resolve!: (value: CalendarSnapshot) => void;
    invoke.mockResolvedValueOnce("granted").mockReturnValueOnce(new Promise<CalendarSnapshot>((r) => { resolve = r; }));
    await account("work@example.com").trigger("click"); await flushPromises();
    expect(wrapper.get('[role="status"]').element).toBe(status);
    expect(wrapper.get("section").element.children.length).toBe(nodes);
    expect(account("work@example.com").element).toBe(checkbox);
    expect(account("work@example.com").attributes("aria-checked")).toBe("true");
    expect(button(en.calendarConnection.refresh).attributes("disabled")).toBeDefined();
    expect(button(en.calendarConnection.refresh).element).toBe(refresh.element);
    expect(button(en.calendarConnection.refresh).classes()).toEqual(refreshClasses);
    expect(button(manageLabel).element).toBe(header);
    expect(wrapper.text()).not.toContain(en.calendarConnection.connecting);
    resolve(snapshot); await flushPromises();
    expect(account("work@example.com").element).toBe(checkbox);
    expect(button(en.calendarConnection.refresh).classes()).toEqual(refreshClasses);
    expect(button(en.calendarConnection.refresh).attributes("disabled")).toBeUndefined();
    expect(wrapper.get('[role="status"]').text()).toBe("1 of 2 accounts selected");
  });

  it("requires selected accounts to enable reminders and always allows turning them off", async () => {
    allowed(); render();
    expect(wrapper.get('[role="switch"]').attributes("disabled")).toBeDefined();
    await connect();
    expect(wrapper.get('[role="switch"]').attributes("disabled")).toBeDefined();
    await account("work@example.com").trigger("click"); await flushPromises();
    await wrapper.get('[role="switch"]').trigger("click"); await flushPromises();
    expect(reminders.setEnabled).toHaveBeenCalledWith(true);
    expect(wrapper.text()).not.toContain("Reminders on.");
    await button(disconnectLabel).trigger("click"); await flushPromises();
    expect(reminders.setEnabled).toHaveBeenLastCalledWith(false);
  });

  it("shows saved timing left of the switch only when reminders are enabled", async () => {
    allowed(); render();
    expect(wrapper.find('[role="combobox"]').exists()).toBe(false);
    await connect();
    await account("work@example.com").trigger("click"); await flushPromises();
    await wrapper.get('[role="switch"]').trigger("click"); await flushPromises();
    const checkbox = account("work@example.com").element;
    const timing = wrapper.get('[role="combobox"]');
    const toggle = wrapper.get('[role="switch"]').element;
    expect(timing.attributes("aria-label")).toBe(en.calendarConnection.timingTitle);
    expect(timing.text()).toBe("15 seconds before");
    expect(timing.element.parentElement).toBe(toggle.parentElement);
    expect(timing.element.compareDocumentPosition(toggle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(toggle.compareDocumentPosition(button(en.calendarConnection.privacy).element) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    wrapper.getComponent(Select).vm.$emit("update:modelValue", "30"); await flushPromises();
    expect(reminders.setLeadSeconds).toHaveBeenCalledWith(30);
    expect(wrapper.get('[role="combobox"]').text()).toBe("30 seconds before");
    expect(account("work@example.com").element).toBe(checkbox);
    await wrapper.get('[role="switch"]').trigger("click"); await flushPromises();
    expect(wrapper.find('[role="combobox"]').exists()).toBe(false);
    expect(wrapper.get('[role="switch"]').element).toBe(toggle);
  });

  it("keeps calendars connected when reminders are disabled after reopening Settings", async () => {
    allowed(); render(); await connect();
    await account("work@example.com").trigger("click"); await flushPromises();
    await wrapper.get('[role="switch"]').trigger("click"); await flushPromises();
    wrapper.unmount(); invoke.mockClear(); render(); await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(button(manageLabel).exists()).toBe(true);
    await wrapper.get('[role="switch"]').trigger("click"); await flushPromises();
    expect(reminders.setEnabled).toHaveBeenLastCalledWith(false);
    expect(button(en.calendarConnection.connect)).toBeUndefined();
    expect(button(disconnectLabel)).toBeUndefined();
    expect(button(manageLabel).exists()).toBe(true);
    expect(wrapper.get('[role="switch"]').attributes("disabled")).toBeUndefined();
    expect(localStorage.getItem(CALENDAR_CONNECTED_KEY)).toBe("true");
    await wrapper.get('[role="switch"]').trigger("click"); await flushPromises();
    expect(reminders.setEnabled).toHaveBeenLastCalledWith(true);
    expect(invoke).not.toHaveBeenCalled();
    await button(manageLabel).trigger("click"); await flushPromises();
    expect(account("work@example.com").attributes("aria-checked")).toBe("true");
    expect(button(disconnectLabel).exists()).toBe(true);
  });

  it("offers the exact Calendar privacy pane after denial", async () => {
    invoke.mockResolvedValueOnce("denied"); render(); await connect();
    expect(wrapper.text()).toContain(en.calendarConnection.denied);
    await button(en.calendarConnection.openSettings).trigger("click");
    expect(invoke).toHaveBeenCalledWith("open_privacy_pane", { pane: "calendars" });
    expect(invoke.mock.calls.map(([command]) => command)).not.toContain("read_calendar_snapshot");
  });

  it("keeps management actions hidden during a failed connection and allows retrying", async () => {
    let resolve!: (value: string) => void;
    invoke.mockResolvedValueOnce("not-determined").mockReturnValueOnce(new Promise<string>((r) => { resolve = r; })); render();
    const initial = button(en.calendarConnection.connect).element;
    const work = connect();
    await flushPromises();
    expect(button(disconnectLabel)).toBeUndefined();
    expect(button(en.calendarConnection.refresh)).toBeUndefined();
    expect(button(en.calendarConnection.connecting).element).toBe(initial);
    resolve("not-determined"); await work;
    expect(wrapper.get('[role="alert"]').text()).toBe(en.calendarConnection.failed);
    allowed(); await connect();
    expect(button(disconnectLabel).exists()).toBe(true);
  });

  it("distinguishes no email accounts from a failed Refresh and offers a header retry", async () => {
    invoke.mockResolvedValueOnce("granted").mockResolvedValueOnce({ accounts: [], meetings: [] }); render(); await connect();
    expect(wrapper.text()).toContain(en.calendarConnection.noCalendars);
    invoke.mockResolvedValueOnce("granted").mockRejectedValueOnce(new Error("Private native details"));
    await button(en.calendarConnection.refresh).trigger("click"); await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe(en.calendarConnection.failed);
    expect(wrapper.text()).not.toContain("Private native details");
    expect(button(disconnectLabel)).toBeUndefined();
    allowed();
    await button(manageLabel).trigger("click"); await flushPromises();
    expect(account("work@example.com").attributes("role")).toBe("checkbox");
    expect(wrapper.find('[role="alert"]').exists()).toBe(false);
  });
});
