import { mount } from "@vue/test-utils";
import { defineComponent, nextTick } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const DICTATION_KEY = "murmur:dictation-indicator-position";
const MEETING_KEY = "murmur:meeting-indicator-position";

beforeEach(() => {
  vi.resetModules();
  vi.restoreAllMocks();
  invokeMock.mockReset().mockResolvedValue(undefined);
  localStorage.clear();
});

describe("recording indicator positions", () => {
  it("defaults both native windows to bottom center without opening Settings", async () => {
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    const { dictation, meeting } = useIndicatorPositions();
    expect(dictation.value).toBe("bottom-center");
    expect(meeting.value).toBe("bottom-center");
    expect(invokeMock).toHaveBeenCalledWith("set_indicator_positions", {
      dictation: "bottom-center", meeting: "bottom-center", dictationStyle: "floating", meetingStyle: "floating",
    });
  });

  it("offers exactly six localized positions in every catalog", async () => {
    const { INDICATOR_POSITIONS } = await import("./useIndicatorPositions");
    expect(INDICATOR_POSITIONS).toEqual([
      "top-left", "top-center", "top-right", "bottom-left", "bottom-center", "bottom-right",
    ]);
    for (const catalog of [en, es, fr, ru]) {
      for (const position of INDICATOR_POSITIONS) {
        expect(catalog.indicatorPosition[position]).not.toBe("");
      }
    }
  });

  it("restores independent choices on launch", async () => {
    localStorage.setItem(DICTATION_KEY, "top-left");
    localStorage.setItem(MEETING_KEY, "bottom-right");
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    expect(useIndicatorPositions().dictation.value).toBe("top-left");
    expect(useIndicatorPositions().meeting.value).toBe("bottom-right");
    expect(invokeMock).toHaveBeenCalledWith("set_indicator_positions", {
      dictation: "top-left", meeting: "bottom-right", dictationStyle: "floating", meetingStyle: "floating",
    });
  });

  it("replaces an invalid saved choice without changing the other window", async () => {
    localStorage.setItem(DICTATION_KEY, "center");
    localStorage.setItem(MEETING_KEY, "top-center");
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    expect(useIndicatorPositions().dictation.value).toBe("bottom-center");
    expect(useIndicatorPositions().meeting.value).toBe("top-center");
    expect(localStorage.getItem(DICTATION_KEY)).toBe("bottom-center");
  });

  it("keeps persisting and syncing changes after Settings unmounts", async () => {
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    const wrapper = mount(defineComponent({
      setup: () => { useIndicatorPositions(); return () => null; },
    }));
    const { dictation, meeting } = useIndicatorPositions();
    dictation.value = "top-right";
    await nextTick();
    wrapper.unmount();
    meeting.value = "bottom-left";
    await nextTick();
    expect(localStorage.getItem(DICTATION_KEY)).toBe("top-right");
    expect(localStorage.getItem(MEETING_KEY)).toBe("bottom-left");
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_positions", {
      dictation: "top-right", meeting: "bottom-left", dictationStyle: "floating", meetingStyle: "floating",
    });
  });

  it("restores mixed display styles without losing either floating position", async () => {
    localStorage.setItem(DICTATION_KEY, "top-left");
    localStorage.setItem(MEETING_KEY, "bottom-right");
    localStorage.setItem("murmur:dictation-indicator-style", "notch");
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    const settings = useIndicatorPositions();
    expect(settings.dictationStyle.value).toBe("notch");
    expect(settings.meetingStyle.value).toBe("floating");
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_positions", {
      dictation: "top-left", meeting: "bottom-right", dictationStyle: "notch", meetingStyle: "floating",
    });
    settings.dictationStyle.value = "floating";
    settings.meetingStyle.value = "notch";
    await nextTick();
    expect(settings.dictation.value).toBe("top-left");
    expect(settings.meeting.value).toBe("bottom-right");
    expect(localStorage.getItem("murmur:dictation-indicator-style")).toBe("floating");
    expect(localStorage.getItem("murmur:meeting-indicator-style")).toBe("notch");
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_positions", {
      dictation: "top-left", meeting: "bottom-right", dictationStyle: "floating", meetingStyle: "notch",
    });
  });

  it("defaults invalid display styles to floating", async () => {
    localStorage.setItem("murmur:dictation-indicator-style", "unknown");
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    expect(useIndicatorPositions().dictationStyle.value).toBe("floating");
    expect(localStorage.getItem("murmur:dictation-indicator-style")).toBe("floating");
  });

  it("can place windows even when browser storage is unavailable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("unavailable"); });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("unavailable"); });
    const { useIndicatorPositions } = await import("./useIndicatorPositions");
    useIndicatorPositions().meeting.value = "top-left";
    await nextTick();
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_positions", {
      dictation: "bottom-center", meeting: "top-left", dictationStyle: "floating", meetingStyle: "floating",
    });
  });
});
