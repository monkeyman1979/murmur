import { mount } from "@vue/test-utils";
import { defineComponent, nextTick } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const SETTINGS_KEY = "murmur:recording-indicator-v1";
const LEGACY_KEYS = [
  "murmur:dictation-indicator-position", "murmur:meeting-indicator-position",
  "murmur:dictation-indicator-style", "murmur:meeting-indicator-style",
] as const;

beforeEach(() => {
  vi.resetModules();
  vi.restoreAllMocks();
  invokeMock.mockReset().mockResolvedValue(undefined);
  localStorage.clear();
});

function storedSettings() {
  return JSON.parse(localStorage.getItem(SETTINGS_KEY)!);
}

describe("shared recording indicator settings", () => {
  it("hydrates one native position and style without opening Settings", async () => {
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    const settings = useIndicatorSettings();
    expect(Object.keys(settings).sort()).toEqual(["position", "style"]);
    expect(settings.position.value).toBe("bottom-center");
    expect(settings.style.value).toBe("floating");
    expect(invokeMock).toHaveBeenCalledExactlyOnceWith("set_indicator_settings", {
      settings: { position: "bottom-center", style: "floating" },
    });
    expect(storedSettings()).toEqual({ position: "bottom-center", style: "floating" });
  });

  it("offers six localized positions and one shared title/description in every catalog", async () => {
    const { INDICATOR_POSITIONS } = await import("./useIndicatorSettings");
    expect(INDICATOR_POSITIONS).toEqual([
      "top-left", "top-center", "top-right", "bottom-left", "bottom-center", "bottom-right",
    ]);
    for (const catalog of [en, es, fr, ru]) {
      for (const position of INDICATOR_POSITIONS) {
        expect(catalog.indicatorPosition[position]).not.toBe("");
      }
      expect(catalog.indicatorPosition.title).not.toBe("");
      expect(catalog.indicatorPosition.description).not.toBe("");
      expect(catalog.indicatorPosition).not.toHaveProperty("dictationHelp");
      expect(catalog.indicatorPosition).not.toHaveProperty("meetingHelp");
    }
  });

  it("restores the shared choice and ignores stale per-type preferences", async () => {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify({ position: "top-center", style: "floating" }));
    for (const key of LEGACY_KEYS) localStorage.setItem(key, key.endsWith("style") ? "notch" : "bottom-right");
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    expect(useIndicatorSettings().position.value).toBe("top-center");
    expect(useIndicatorSettings().style.value).toBe("floating");
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_settings", {
      settings: { position: "top-center", style: "floating" },
    });
    for (const key of LEGACY_KEYS) expect(localStorage.getItem(key)).toBeNull();
  });

  it("migrates matching notch preferences and removes all four old keys", async () => {
    localStorage.setItem(LEGACY_KEYS[0], "top-left");
    localStorage.setItem(LEGACY_KEYS[1], "top-left");
    localStorage.setItem(LEGACY_KEYS[2], "notch");
    localStorage.setItem(LEGACY_KEYS[3], "notch");
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    expect(useIndicatorSettings().style.value).toBe("notch");
    expect(storedSettings()).toEqual({ position: "top-left", style: "notch" });
    for (const key of LEGACY_KEYS) expect(localStorage.getItem(key)).toBeNull();
  });

  it("prefers valid dictation choices when the previous settings disagree", async () => {
    localStorage.setItem(LEGACY_KEYS[0], "top-left");
    localStorage.setItem(LEGACY_KEYS[1], "bottom-right");
    localStorage.setItem(LEGACY_KEYS[2], "floating");
    localStorage.setItem(LEGACY_KEYS[3], "notch");
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    expect(useIndicatorSettings().position.value).toBe("top-left");
    expect(useIndicatorSettings().style.value).toBe("floating");
    expect(storedSettings()).toEqual({ position: "top-left", style: "floating" });
  });

  it("falls back to valid meeting choices if dictation choices are missing or invalid", async () => {
    localStorage.setItem(LEGACY_KEYS[0], "center");
    localStorage.setItem(LEGACY_KEYS[1], "bottom-right");
    localStorage.setItem(LEGACY_KEYS[3], "notch");
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    expect(useIndicatorSettings().position.value).toBe("bottom-right");
    expect(useIndicatorSettings().style.value).toBe("notch");
  });

  it("replaces invalid shared values with defaults rather than resurrecting old choices", async () => {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify({ position: "center", style: "unknown" }));
    localStorage.setItem(LEGACY_KEYS[0], "top-left");
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    expect(useIndicatorSettings().position.value).toBe("bottom-center");
    expect(useIndicatorSettings().style.value).toBe("floating");
    expect(storedSettings()).toEqual({ position: "bottom-center", style: "floating" });
  });

  it.each(["invalid JSON", "null", "42"])("recovers malformed shared data (%s) from legacy choices", async (value) => {
    localStorage.setItem(SETTINGS_KEY, value);
    localStorage.setItem(LEGACY_KEYS[0], "top-right");
    await import("./useIndicatorSettings");
    expect(storedSettings()).toEqual({ position: "top-right", style: "floating" });
  });

  it("keeps one preference alive after Settings unmounts and preserves the floating anchor", async () => {
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    const wrapper = mount(defineComponent({
      setup: () => { useIndicatorSettings(); return () => null; },
    }));
    const { position, style } = useIndicatorSettings();
    position.value = "top-right";
    style.value = "notch";
    await nextTick();
    wrapper.unmount();
    style.value = "floating";
    await nextTick();
    expect(position.value).toBe("top-right");
    expect(storedSettings()).toEqual({ position: "top-right", style: "floating" });
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_settings", {
      settings: { position: "top-right", style: "floating" },
    });
  });

  it("retains legacy choices if saving the migration fails, while syncing native settings", async () => {
    localStorage.setItem(LEGACY_KEYS[0], "top-left");
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("unavailable"); });
    await import("./useIndicatorSettings");
    expect(localStorage.getItem(LEGACY_KEYS[0])).toBe("top-left");
    expect(localStorage.getItem(SETTINGS_KEY)).toBeNull();
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_settings", {
      settings: { position: "top-left", style: "floating" },
    });
  });

  it("can place recordings even when browser storage is unavailable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("unavailable"); });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("unavailable"); });
    const { useIndicatorSettings } = await import("./useIndicatorSettings");
    useIndicatorSettings().position.value = "top-left";
    await nextTick();
    expect(invokeMock).toHaveBeenLastCalledWith("set_indicator_settings", {
      settings: { position: "top-left", style: "floating" },
    });
  });
});
