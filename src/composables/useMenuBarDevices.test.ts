import { beforeEach, expect, it, vi } from "vitest";
import { ref, type Ref } from "vue";
import { useMenuBarDevices } from "./useMenuBarDevices";

const state = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: string | null }) => void>(),
  input: null as Ref<string | null> | null,
  output: null as Ref<string | null> | null,
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: string | null }) => void) => {
    state.listeners.set(name, callback);
    return () => state.listeners.delete(name);
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: state.invoke }));
vi.mock("@/composables/useInputDevices", () => ({ useInputDevices: () => ({ selected: state.input }) }));
vi.mock("@/composables/useOutputDevices", () => ({ useOutputDevices: () => ({ selected: state.output }) }));

beforeEach(() => {
  state.input = ref<string | null>(null);
  state.output = ref<string | null>(null);
  state.listeners.clear();
  state.invoke.mockClear();
});

it("keeps native device selections without honoring the legacy hidden-icon preference", async () => {
  localStorage.setItem("murmur:menu-bar-icon-visible", "false");
  useMenuBarDevices();
  await vi.waitFor(() => expect(state.listeners.size).toBe(2));

  state.listeners.get("murmur://tray-microphone-selected")?.({ payload: "External Mic" });
  state.listeners.get("murmur://tray-output-selected")?.({ payload: "Speakers" });
  expect(state.input?.value).toBe("External Mic");
  expect(state.output?.value).toBe("Speakers");
  expect(state.invoke).not.toHaveBeenCalled();
});
