import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises } from "@vue/test-utils";
const plugin = vi.hoisted(() => ({ enable: vi.fn(), disable: vi.fn(), isEnabled: vi.fn() }));
vi.mock("@/lib/launchAtLogin", () => plugin);
beforeEach(() => { vi.resetModules(); vi.resetAllMocks(); plugin.isEnabled.mockResolvedValue(false); });

describe("useAutostart", () => {
  it("starts neutral and exposes a safe read failure without claiming Off", async () => {
    plugin.isEnabled.mockRejectedValue(new Error("/Users/private SECRET-LICENSE-TOKEN"));
    const { useAutostart } = await import("./useAutostart");
    const state = useAutostart();
    expect(state.enabled.value).toBeNull();
    await flushPromises();
    expect(state.error.value).toBe("Launch at login status is unavailable.");
    expect(state.error.value).not.toContain("/Users/private");
    expect(state.enabled.value).toBeNull();
    expect(state.loading.value).toBe(false);
  });
  it("refreshes actual state after a safely rendered partial write failure", async () => {
    const { useAutostart } = await import("./useAutostart");
    const state = useAutostart();
    await flushPromises();
    plugin.enable.mockRejectedValue(new Error("/Users/private SECRET-LICENSE-TOKEN"));
    await state.setEnabled(true);
    expect(state.enabled.value).toBe(false);
    expect(state.error.value).toBe("Launch at login could not be updated.");
    expect(state.error.value).not.toContain("SECRET-LICENSE-TOKEN");
    expect(state.pending.value).toBe(false);
    plugin.isEnabled.mockResolvedValue(true);
    await state.setEnabled(true);
    expect(state.enabled.value).toBe(true);
    expect(state.error.value).toBe("Launch at login could not be updated.");
  });
  it("serializes rapid toggles across consumers with no optimistic value", async () => {
    const { useAutostart } = await import("./useAutostart");
    const first = useAutostart();
    const second = useAutostart();
    await flushPromises();
    let release!: () => void;
    plugin.enable.mockReturnValue(new Promise<void>((resolve) => { release = resolve; }));
    const on = first.setEnabled(true);
    const off = second.setEnabled(false);
    await flushPromises();
    expect(first.pending.value).toBe(true);
    expect(first.enabled.value).toBe(false);
    expect(plugin.disable).not.toHaveBeenCalled();
    release();
    await Promise.all([on, off]);
    expect(plugin.disable).toHaveBeenCalledOnce();
    expect(first.enabled.value).toBe(false);
    expect(second.pending.value).toBe(false);
  });
});
