import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises } from "@vue/test-utils";
import { nextTick, ref, type Ref } from "vue";
import type { PermissionSnapshot } from "@/composables/useMurmurEvents";
const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), permissions: null as unknown as Ref<PermissionSnapshot | null> }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));
vi.mock("@/composables/useMurmurEvents", () => ({ useMurmurEvents: () => ({ permissions: native.permissions }) }));
const mic = { name: "Built-in Microphone", is_default: true };
const granted: PermissionSnapshot = { microphone: "granted", input_monitoring: "granted", accessibility: "granted" };
beforeEach(() => {
  vi.resetModules(); vi.resetAllMocks(); localStorage.clear();
  native.permissions = ref({ ...granted });
  native.listen.mockResolvedValue(vi.fn()); native.invoke.mockResolvedValue([]);
});

describe("useInputDevices", () => {
  it.each([null, "not-determined", "denied", "unknown"] as const)("does not mistake pre-consent state for missing hardware (permission=%s)", async (microphone) => {
    native.permissions.value = microphone === null ? null : { ...granted, microphone };
    localStorage.setItem("murmur:preferred-input-device", "Saved USB Microphone");
    const { useInputDevices } = await import("./useInputDevices");
    const input = useInputDevices();
    await flushPromises();
    expect(native.invoke).not.toHaveBeenCalledWith("list_input_devices");
    expect(input.state.value).toBe("loading");
    expect(input.devices.value).toEqual([]);
    expect(input.selected.value).toBe("Saved USB Microphone");
    await input.refresh();
    expect(native.invoke).not.toHaveBeenCalledWith("list_input_devices");
    expect(input.selected.value).toBe("Saved USB Microphone");
  });

  it("refreshes automatically on grant, not on unrelated permission updates", async () => {
    native.permissions.value = { ...granted, microphone: "denied", accessibility: "denied" };
    native.invoke.mockImplementation(async (command) => command === "list_input_devices" ? [mic] : undefined);
    const { useInputDevices } = await import("./useInputDevices");
    const input = useInputDevices();
    await flushPromises();
    native.permissions.value = { ...granted, accessibility: "denied" };
    await flushPromises();
    expect(input.state.value).toBe("ready");
    expect(input.devices.value).toEqual([mic]);
    native.permissions.value = { ...granted };
    await flushPromises();
    expect(native.invoke.mock.calls.filter(([command]) => command === "list_input_devices")).toHaveLength(1);
  });

  it("ignores stale enumeration and hardware events after revocation, then recovers on re-grant", async () => {
    localStorage.setItem("murmur:preferred-input-device", mic.name);
    let finish!: (devices: typeof mic[]) => void;
    let receive!: (event: { payload: { devices: typeof mic[]; current_default: string | null } }) => void;
    native.listen.mockImplementation((_name, callback) => { receive = callback; return Promise.resolve(vi.fn()); });
    native.invoke.mockImplementation((command) => command === "list_input_devices" ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve());
    const { useInputDevices } = await import("./useInputDevices");
    const input = useInputDevices();
    await flushPromises();
    native.permissions.value = { ...granted, microphone: "denied" };
    receive({ payload: { devices: [], current_default: null } });
    finish([mic]);
    await flushPromises();
    expect(input.state.value).toBe("loading");
    expect(input.devices.value).toEqual([]);
    expect(input.selected.value).toBe(mic.name);
    native.invoke.mockImplementation(async (command) => command === "list_input_devices" ? [mic] : undefined);
    native.permissions.value = { ...granted };
    await flushPromises();
    expect(input.state.value).toBe("ready");
    expect(input.devices.value).toEqual([mic]);
    expect(input.selected.value).toBe(mic.name);
  });

  it("distinguishes loading, empty, error, and ready on retry", async () => {
    const { useInputDevices } = await import("./useInputDevices");
    const input = useInputDevices();
    expect(input.state.value).toBe("loading");
    await flushPromises();
    expect(input.state.value).toBe("empty");
    native.invoke.mockRejectedValue(new Error("/Users/private SECRET-LICENSE-TOKEN"));
    await input.refresh();
    expect(input.state.value).toBe("error");
    expect(input.error.value).toBe("Microphone devices are unavailable. Try again.");
    expect(input.error.value).not.toContain("/Users/private");
    native.invoke.mockResolvedValue([mic]);
    await input.refresh();
    expect(input.state.value).toBe("ready");
    expect(input.error.value).toBeNull();
  });
  it("newer device events win over stale enumeration and clear disconnected selection", async () => {
    let receive!: (event: { payload: { devices: typeof mic[]; current_default: string | null } }) => void;
    native.listen.mockImplementation((_name, callback) => { receive = callback; return Promise.resolve(vi.fn()); });
    let finish!: (devices: typeof mic[]) => void;
    native.invoke.mockImplementation((name) => name === "list_input_devices" ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve());
    const { useInputDevices } = await import("./useInputDevices");
    const input = useInputDevices();
    input.selected.value = "Disconnected";
    receive({ payload: { devices: [mic], current_default: mic.name } });
    finish([]);
    await flushPromises();
    expect(input.state.value).toBe("ready");
    expect(input.devices.value).toEqual([mic]);
    expect(input.selected.value).toBeNull();
  });
  it("serializes persisted selection writes and explicit monitor synchronization", async () => {
    const { useInputDevices } = await import("./useInputDevices");
    const input = useInputDevices();
    await flushPromises();
    native.invoke.mockClear();
    let finish!: () => void;
    native.invoke.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; })).mockResolvedValue(undefined);
    input.selected.value = "First";
    await nextTick();
    await flushPromises();
    input.selected.value = "Second";
    await nextTick();
    const synced = input.syncSelection();
    expect(native.invoke).toHaveBeenCalledTimes(1);
    finish();
    await synced;
    expect(native.invoke).toHaveBeenLastCalledWith("set_input_device", { name: "Second" });
  });
});
