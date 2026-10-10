import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";

const eventMocks = vi.hoisted(() => ({
  callbacks: new Map<string, (event: { payload: unknown }) => void>(),
  unlisteners: [] as ReturnType<typeof vi.fn>[],
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    (event: string, callback: (event: { payload: unknown }) => void) => {
      eventMocks.callbacks.set(event, callback);
      const unlisten = vi.fn();
      eventMocks.unlisteners.push(unlisten);
      return Promise.resolve(unlisten);
    },
  ),
}));

import Indicator from "@/components/Indicator.vue";
import { i18n } from "@/i18n";

async function emit(event: string, payload: unknown) {
  eventMocks.callbacks.get(event)?.({ payload });
  await nextTick();
}

beforeEach(() => {
  eventMocks.callbacks.clear();
  eventMocks.unlisteners.length = 0;
});

afterEach(() => {
  document.body.innerHTML = "";
});

describe("Indicator dictation result feedback", () => {
  it("names the effective audio input for both no-result states", async () => {
    const wrapper = mount(Indicator, { global: { plugins: [i18n] } });
    await nextTick();
    await nextTick();

    await emit("murmur://indicator-device", "Galaxy Buds");
    await emit("murmur://indicator-state", "no-audio");

    expect(wrapper.text()).toContain("No audio · Galaxy Buds");
    expect(wrapper.get('[role="status"]').attributes("aria-live")).toBe("polite");
    expect(wrapper.get("[title]").attributes("title")).toBe(
      "No audio · Galaxy Buds",
    );
    expect(wrapper.get("[title]").classes()).toContain("truncate");

    await emit("murmur://indicator-state", "no-speech");
    expect(wrapper.text()).toContain("No speech recognized · Galaxy Buds");
    expect(wrapper.get("[title]").attributes("title")).toBe(
      "No speech recognized · Galaxy Buds",
    );
    wrapper.unmount();
  });

  it("uses concise audio-input fallbacks for both no-result states", async () => {
    const wrapper = mount(Indicator, { global: { plugins: [i18n] } });
    await nextTick();
    await nextTick();

    await emit("murmur://indicator-device", "");
    await emit("murmur://indicator-state", "no-audio");
    expect(wrapper.text()).toContain("No audio · Check audio input");

    await emit("murmur://indicator-state", "no-speech");
    expect(wrapper.text()).toContain(
      "No speech recognized · Check audio input",
    );
    wrapper.unmount();
  });

  it("ignores unknown states, clears stale device context, and unlistens", async () => {
    const wrapper = mount(Indicator, { global: { plugins: [i18n] } });
    await nextTick();
    await nextTick();

    await emit("murmur://indicator-device", "Old Headset");
    await emit("murmur://indicator-state", { state: "no-audio" });
    expect(wrapper.text()).toContain("Recording");

    await emit("murmur://indicator-state", "recording");
    await emit("murmur://indicator-state", "no-audio");
    expect(wrapper.text()).toContain("No audio · Check audio input");

    wrapper.unmount();
    expect(eventMocks.unlisteners).toHaveLength(3);
    for (const unlisten of eventMocks.unlisteners) expect(unlisten).toHaveBeenCalledOnce();
  });
});
