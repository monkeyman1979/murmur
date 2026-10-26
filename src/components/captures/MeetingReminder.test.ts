import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";
import { createI18n } from "vue-i18n";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import MeetingReminder from "./MeetingReminder.vue";
import en from "@/locales/en.json";

const mock = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), callback: null as null | ((event: { payload: unknown }) => void) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mock.listen }));
vi.mock("@/composables/useTheme", () => ({ useTheme: () => ({}) }));
const offer = { token: "a".repeat(32), title: "<script>Private meeting</script>", start_ms: Date.UTC(2026, 9, 26, 22), provider: "GoogleMeet" };
let wrapper: VueWrapper;
beforeEach(() => {
  mock.invoke.mockReset(); mock.listen.mockReset(); mock.callback = null;
  mock.invoke.mockImplementation(async (command) => command === "calendar_reminder_current" ? offer : undefined);
  mock.listen.mockImplementation(async (_name, callback) => { mock.callback = callback; return () => { mock.callback = null; }; });
});
afterEach(() => { wrapper?.unmount(); });
async function render() {
  wrapper = mount(MeetingReminder, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
  await flushPromises();
}
function button(text: string) { return wrapper.findAll("button").find((b) => b.text() === text)!; }

describe("meeting reminder: offer, never automatic recording", () => {
  it("renders escaped meeting text without requesting any recorder action", async () => {
    await render();
    expect(wrapper.text()).toContain(offer.title);
    expect(wrapper.find("script").exists()).toBe(false);
    expect(mock.invoke.mock.calls.map(([command]) => command)).toEqual(["calendar_reminder_current"]);
    expect(button(en.meetingReminder.start).exists()).toBe(true);
  });

  it("only an explicit start click responds with this offer's token", async () => {
    await render(); await button(en.meetingReminder.start).trigger("click"); await flushPromises();
    expect(mock.invoke).toHaveBeenCalledWith("calendar_reminder_action", { token: offer.token, action: "start_recording" });
    expect(mock.invoke.mock.calls.map(([command]) => command)).not.toContain("meeting_recorder_start");
    expect(wrapper.find("section").exists()).toBe(false);
  });

  it("dismisses without asking to record", async () => {
    await render(); await button(en.meetingReminder.dismiss).trigger("click"); await flushPromises();
    expect(mock.invoke).toHaveBeenCalledWith("calendar_reminder_action", { token: offer.token, action: "dismiss" });
    expect(mock.invoke).not.toHaveBeenCalledWith("calendar_reminder_action", expect.objectContaining({ action: "start_recording" }));
  });

  it("removes canceled/expired offers and ignores a late initial snapshot", async () => {
    let resolve!: (value: typeof offer) => void;
    mock.invoke.mockReturnValueOnce(new Promise((r) => { resolve = r; }));
    wrapper = mount(MeetingReminder, { global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] } });
    await flushPromises(); mock.callback!({ payload: null }); resolve(offer); await flushPromises();
    expect(wrapper.find("section").exists()).toBe(false);
  });

  it("suppresses duplicate clicks while the response is pending", async () => {
    await render(); let resolve!: () => void;
    mock.invoke.mockReturnValueOnce(new Promise<void>((r) => { resolve = r; }));
    await button(en.meetingReminder.start).trigger("click"); await button(en.meetingReminder.start).trigger("click");
    expect(mock.invoke.mock.calls.filter(([command]) => command === "calendar_reminder_action")).toHaveLength(1);
    resolve(); await flushPromises();
  });
});
