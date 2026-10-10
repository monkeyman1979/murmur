import { nextTick } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const stableKey = "murmur:onboarding-step-key";
const legacyKey = "murmur:onboarding-step";
const completedKey = "murmur:onboarding-completed";

beforeEach(() => {
  vi.resetModules();
  localStorage.clear();
});
afterEach(() => vi.restoreAllMocks());

async function load() {
  return (await import("./useOnboarding")).useOnboarding();
}

describe("useOnboarding stable progress", () => {
  it("defines the seven approved steps with model before microphone", async () => {
    const { ONBOARDING_STEPS } = await import("./useOnboarding");
    expect(ONBOARDING_STEPS).toEqual([
      "welcome", "productOverview", "permissions", "requiredModel", "microphone", "tryIt", "ready",
    ]);
  });

  it.each([
    ["0", "welcome"], ["1", "permissions"], ["2", "microphone"],
    ["3", "requiredModel"], ["4", "tryIt"], ["5", "ready"],
  ])("migrates v3 index %s to %s before removing the old key", async (legacy, step) => {
    localStorage.setItem(legacyKey, legacy);
    expect((await load()).stepKey.value).toBe(step);
    expect(localStorage.getItem(stableKey)).toBe(step);
    expect(localStorage.getItem(legacyKey)).toBeNull();
  });

  it.each(["-1", "6", "99", "1oops", "1.5", "", "NaN", " 2", "02", "1e0"])("rejects malformed/out-of-range legacy value %s", async (legacy) => {
    localStorage.setItem(legacyKey, legacy);
    expect((await load()).stepKey.value).toBe("welcome");
  });

  it("recognized stable key wins over legacy state", async () => {
    localStorage.setItem(stableKey, "productOverview");
    localStorage.setItem(legacyKey, "4");
    expect((await load()).stepKey.value).toBe("productOverview");
    expect(localStorage.getItem(legacyKey)).toBeNull();
  });

  it("unknown stable key falls back to recognized legacy then welcome", async () => {
    localStorage.setItem(stableKey, "retired");
    localStorage.setItem(legacyKey, "2");
    expect((await load()).stepKey.value).toBe("microphone");
    vi.resetModules();
    localStorage.setItem(stableKey, "retired");
    expect((await load()).stepKey.value).toBe("welcome");
  });

  it("completion wins and ignores progress without migrating it", async () => {
    localStorage.setItem(completedKey, "true");
    localStorage.setItem(stableKey, "ready");
    localStorage.setItem(legacyKey, "1");
    const state = await load();
    expect(state.completed.value).toBe(true);
    expect(state.stepKey.value).toBe("welcome");
    expect(localStorage.getItem(legacyKey)).toBe("1");
    state.stepKey.value = "permissions";
    expect(localStorage.getItem(stableKey)).toBe("ready");
  });

  it("persists stable navigation synchronously and completion clears both keys without touching preferences", async () => {
    const preferences = { "murmur:theme": "light", "murmur:shell-gradient": "desert", "murmur:active-stt": "whisper-small-multilingual", "murmur:hotkey-binding": "custom", "murmur:preferred-input-device": "USB" };
    Object.entries(preferences).forEach(([key, value]) => localStorage.setItem(key, value));
    const state = await load();
    state.stepKey.value = "requiredModel";
    expect(state.stepIndex.value).toBe(3);
    expect(localStorage.getItem(stableKey)).toBe("requiredModel");
    state.stepKey.value = "ready";
    localStorage.setItem(legacyKey, "5");
    state.markComplete();
    await nextTick();
    expect(state.completed.value).toBe(true);
    expect(localStorage.getItem(completedKey)).toBe("true");
    expect(localStorage.getItem(stableKey)).toBeNull();
    expect(localStorage.getItem(legacyKey)).toBeNull();
    Object.entries(preferences).forEach(([key, value]) => expect(localStorage.getItem(key)).toBe(value));
  });

  it("retains hydrated progress when storage writes fail", async () => {
    localStorage.setItem(legacyKey, "2");
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Read only"); });
    const state = await load();
    expect(state.stepKey.value).toBe("microphone");
    expect(localStorage.getItem(legacyKey)).toBe("2");
    expect(() => state.markComplete()).not.toThrow();
    expect(state.completed.value).toBe(true);
  });

  it("remains usable if storage cannot be read", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("Unavailable"); });
    const state = await load();
    expect(state.stepKey.value).toBe("welcome");
    expect(state.completed.value).toBe(false);
  });
});
