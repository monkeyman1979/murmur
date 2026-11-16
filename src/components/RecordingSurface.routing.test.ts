import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import Surface from "./RecordingSurface.vue";

const routing = vi.hoisted(() => ({
  label: "indicator",
  listeners: [] as { label?: string; callback: (e: { payload: unknown }) => void }[],
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ label: routing.label }) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (_event, callback, options) => {
  const entry = { label: options?.target?.label, callback };
  routing.listeners.push(entry);
  return () => { routing.listeners = routing.listeners.filter(item => item !== entry); };
}) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => ({
  style: "floating", expanded: false, compact_width: 360, notch_width: 0, notch_height: 0,
})) }));

function mountWindow(label: string) {
  routing.label = label;
  return mount(Surface, {
    // Only the dictation laptop mirror is passive; the laptop meeting notch is
    // a full controller.
    props: { interactive: label !== "indicator-notch" },
    slots: { default: '<div class="floating-pill">Recording</div>', compact: "Recording" },
    global: { plugins: [createI18n({ legacy: false, locale: "en", messages: { en } })] },
  });
}

async function emitTo(label: string, payload: unknown) {
  // Tauri's unscoped Any listener receives every emit_to event. A listener
  // explicitly targeting one WebviewWindow receives only that label.
  for (const entry of routing.listeners) {
    if (!entry.label || entry.label === label) entry.callback({ payload });
  }
  await flushPromises();
}
const notch = { style: "notch", expanded: false, compact_width: 329, notch_width: 185, notch_height: 32, expanded_width: 400, expanded_height: 200 };
const floating = { ...notch, style: "floating", notch_width: 0, notch_height: 0 };

beforeEach(() => { routing.listeners = []; });

describe("window-scoped recording presentations", () => {
  it("keeps an external floating pill while the laptop mirror receives notch styling", async () => {
    const external = mountWindow("indicator");
    const laptop = mountWindow("indicator-notch");
    await flushPromises();
    await emitTo("indicator", floating);
    await emitTo("indicator-notch", notch);
    expect(external.find(".floating-pill").exists()).toBe(true);
    expect(external.find(".notch-surface").exists()).toBe(false);
    expect(laptop.find(".notch-surface").exists()).toBe(true);
    expect(laptop.find(".floating-pill").exists()).toBe(false);
    await emitTo("indicator", floating);
    expect(laptop.find(".notch-surface").exists()).toBe(true);
    external.unmount();
    laptop.unmount();
    expect(routing.listeners).toHaveLength(0);
  });

  it("keeps external meeting controls floating while the laptop notch stays a usable controller", async () => {
    const external = mountWindow("meeting-controller");
    const laptop = mountWindow("meeting-notch");
    await flushPromises();
    await emitTo("meeting-controller", floating);
    await emitTo("meeting-notch", notch);
    expect(external.find(".floating-pill").exists()).toBe(true);
    expect(external.find(".notch-surface").exists()).toBe(false);
    expect(laptop.find(".notch-surface--passive").exists()).toBe(false);
    expect(laptop.get("button.notch-surface__handle").attributes("aria-expanded")).toBe("false");
    await emitTo("meeting-controller", { ...notch, expanded: true });
    expect(laptop.find(".notch-surface__details").exists()).toBe(false);
    external.unmount();
    laptop.unmount();
  });

  it("does not apply meeting expansion or collapse to either dictation window", async () => {
    const external = mountWindow("indicator");
    const laptop = mountWindow("indicator-notch");
    const meeting = mountWindow("meeting-controller");
    await flushPromises();
    await emitTo("indicator", floating);
    await emitTo("indicator-notch", notch);
    await emitTo("meeting-controller", { ...notch, expanded: true });
    expect(meeting.get(".notch-surface__handle").attributes("aria-expanded")).toBe("true");
    expect(external.find(".notch-surface").exists()).toBe(false);
    expect(laptop.find("button").exists()).toBe(false);
    await emitTo("meeting-controller", floating);
    expect(meeting.find(".notch-surface").exists()).toBe(false);
    expect(laptop.find(".notch-surface").exists()).toBe(true);
    external.unmount();
    laptop.unmount();
    meeting.unmount();
  });
});
