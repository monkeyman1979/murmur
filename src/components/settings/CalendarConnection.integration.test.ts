import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import en from "@/locales/en.json";

const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
const ENABLED = "murmur:calendar-reminders-enabled-v1";
const SELECTION = "murmur:calendar-account-selection-v1";
const CONNECTED = "murmur:calendar-connected-v1";
const accounts = [{ id: "a1", email: "work@example.com" }, { id: "a2", email: "personal@example.com" }];
let wrapper: VueWrapper | undefined;
let stop: (() => void) | undefined;

beforeEach(() => {
  vi.resetModules();
  native.invoke.mockReset(); native.listen.mockReset();
  for (const key of [ENABLED, SELECTION, CONNECTED, "murmur:calendar-reminders-handled-v1", "murmur:calendar-reminders-lead-seconds-v1"]) localStorage.removeItem(key);
  native.listen.mockResolvedValue(() => {});
  native.invoke.mockImplementation(async (command: string) => {
    if (command === "calendar_permission") return "granted";
    if (command === "read_calendar_snapshot") return { accounts, meetings: [] };
    if (command === "configure_calendar_reminders") return;
    throw new Error("Unexpected native command");
  });
});
afterEach(async () => {
  wrapper?.unmount(); wrapper = undefined;
  stop?.(); stop = undefined;
  await flushPromises();
  vi.restoreAllMocks();
});

async function render() {
  const api = await import("@/composables/useCalendarReminders");
  stop = await api.startCalendarReminders();
  await mountSettings();
}
async function mountSettings() {
  const { default: CalendarConnection } = await import("./CalendarConnection.vue");
  wrapper = mount(CalendarConnection, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
  await flushPromises();
}
function button(text: string) { return wrapper!.findAll("button").find((candidate) => candidate.text() === text)!; }
async function click(text: string) { await button(text).trigger("click"); await flushPromises(); }
async function select(email: string) {
  await wrapper!.findAll("label").find((label) => label.text() === email)!.get('[role="checkbox"]').trigger("click");
  await flushPromises();
}
function configurations() { return native.invoke.mock.calls.filter(([command]) => command === "configure_calendar_reminders").map(([, value]) => value); }
function lastConfiguration() { const calls = configurations(); return calls[calls.length - 1]; }

// Unlike the isolated component tests, these use BOTH real composables and the
// real main-window listener/queue. Only the OS boundary is mocked.
describe("calendar setup integration", () => {
  it("connects without opting in, then saves selection and enables the actual selected native account", async () => {
    await render();
    expect(wrapper!.find('[role="switch"]').exists()).toBe(false);
    expect(native.invoke.mock.calls.map(([command]) => command)).not.toContain("calendar_permission");
    await click(en.calendarConnection.connect);
    expect(wrapper!.find('[role="switch"]').exists()).toBe(false);
    expect(localStorage.getItem(ENABLED)).toBeNull();
    await select("work@example.com");
    expect(localStorage.getItem(SELECTION)).toBe('["a1"]');
    expect(localStorage.getItem(ENABLED)).toBe("true");
    expect(lastConfiguration()).toEqual({ enabled: true, accountIds: ["a1"], leadSeconds: 15, handled: [] });
    expect(wrapper!.get('[role="switch"]').attributes("aria-checked")).toBe("true");
    expect(wrapper!.get('[role="combobox"]').text()).toBe("15 seconds before");
    expect(native.invoke.mock.calls.filter(([command]) => command === "read_calendar_snapshot").every(([, options]) => options.accountIds.length === 0)).toBe(true);
  });

  it("preserves a real Off choice through Refresh, remount, removing all accounts and reselection", async () => {
    await render(); await click(en.calendarConnection.connect); await select("work@example.com");
    await wrapper!.get('[role="switch"]').trigger("click"); await flushPromises();
    expect(localStorage.getItem(ENABLED)).toBe("false");
    const enabledCalls = configurations().filter((value) => value.enabled).length;
    await select("personal@example.com"); await click(en.calendarConnection.refresh);
    wrapper!.unmount(); await mountSettings();
    expect(wrapper!.get('[role="switch"]').attributes("aria-checked")).toBe("false");
    await click(en.calendarConnection.manageAccounts);
    await select("work@example.com"); await select("personal@example.com");
    expect(wrapper!.find('[role="switch"]').exists()).toBe(false);
    expect(lastConfiguration()).toMatchObject({ enabled: false, accountIds: [] });
    await select("work@example.com");
    expect(wrapper!.get('[role="switch"]').attributes("aria-checked")).toBe("false");
    expect(configurations().filter((value) => value.enabled)).toHaveLength(enabledCalls);
  });

  it("does not silently enable existing users when their saved accounts are restored", async () => {
    localStorage.setItem(SELECTION, '["a1"]'); localStorage.setItem(CONNECTED, "true");
    await render();
    expect(wrapper!.get('[role="switch"]').attributes("aria-checked")).toBe("false");
    expect(localStorage.getItem(ENABLED)).toBeNull();
    await click(en.calendarConnection.manageAccounts);
    expect(configurations().every((value) => !value.enabled)).toBe(true);
    expect(localStorage.getItem(ENABLED)).toBeNull();
  });

  it("fails closed on native enable failure, reports a safe error and allows explicit retry", async () => {
    await render(); await click(en.calendarConnection.connect);
    const original = native.invoke.getMockImplementation()!;
    let rejectEnable = true;
    native.invoke.mockImplementation(async (command, options) => {
      if (command === "configure_calendar_reminders" && options.enabled && rejectEnable) throw new Error("Private EventKit detail");
      return original(command, options);
    });
    await select("work@example.com");
    expect(localStorage.getItem(ENABLED)).toBe("false");
    expect(wrapper!.get('[role="switch"]').attributes("aria-checked")).toBe("false");
    expect(wrapper!.get('[role="alert"]').text()).toBe(en.calendarConnection.remindersFailed);
    expect(wrapper!.text()).not.toContain("Private EventKit detail");
    rejectEnable = false;
    await wrapper!.get('[role="switch"]').trigger("click"); await flushPromises();
    expect(wrapper!.get('[role="switch"]').attributes("aria-checked")).toBe("true");
    expect(wrapper!.find('[role="alert"]').exists()).toBe(false);
    expect(lastConfiguration()).toMatchObject({ enabled: true, accountIds: ["a1"] });
  });
});
