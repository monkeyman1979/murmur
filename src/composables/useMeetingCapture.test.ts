import { flushPromises } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn().mockResolvedValue(() => {}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { MEETING_RECORDING_NOTICE_KEY } from "@/lib/captures";

beforeEach(() => invokeMock.mockReset());

describe("useMeetingCapture authoritative command boundary", () => {
  it("clears a transient status issue after recorder status is confirmed again", async () => {
    const activeStatus = {
      capture: { id: 8, status: "recording", title: "Planning" },
      active_duration_ms: 4_000,
      paused_duration_ms: 0,
      paused: false,
    };
    invokeMock.mockResolvedValue(activeStatus);
    const meeting = useMeetingCapture();
    await meeting.hydrate();
    await vi.waitFor(() => expect(meeting.activeCapture.value?.id).toBe(8));

    invokeMock.mockRejectedValueOnce({ code: "persistence_unavailable", message: "offline" });
    await meeting.hydrate();
    expect(meeting.recordingIssue.value?.label).toBe("Status unavailable");

    invokeMock.mockResolvedValue(activeStatus);
    await meeting.hydrate();
    expect(meeting.recordingIssue.value).toBeNull();
  });

  it("synchronizes a renamed active capture without replacing another meeting", async () => {
    const activeStatus = { capture: { id: 8, status: "recording", title: "Planning" } };
    invokeMock.mockResolvedValue(activeStatus);
    const meeting = useMeetingCapture();
    await meeting.hydrate();

    const activeCapture = meeting.activeCapture.value!;
    meeting.syncActiveCapture({ ...activeCapture, title: "Planning review" });
    expect(meeting.activeCapture.value?.title).toBe("Planning review");
    meeting.syncActiveCapture({ ...activeCapture, id: 99, title: "Other meeting" });
    expect(meeting.activeCapture.value?.title).toBe("Planning review");
  });

  it("uses recorder command results and rehydrates status after controls", async () => {
    let status = {
      capture: null as Record<string, unknown> | null,
      active_duration_ms: 0,
      paused_duration_ms: 0,
      paused: false,
    };
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "meeting_recorder_status") return status;
      if (command === "meeting_recorder_start") {
        status = { ...status, capture: { id: 9, status: "recording", title: args?.title } };
        return status.capture;
      }
      if (command === "meeting_recorder_pause") {
        status = { ...status, capture: { ...(status.capture ?? {}), status: "paused" }, paused: true };
        return status.capture;
      }
      if (command === "meeting_recorder_cancel") return { status: "confirmation_required", capture: status.capture };
      if (!command) return null; // Test matcher probes the mock without invoking a native command.
      throw new Error(`unexpected command ${command}`);
    });

    const meeting = useMeetingCapture();
    await meeting.hydrate();
    expect(await meeting.start(" Planning ")).toMatchObject({ id: 9, status: "recording" });
    expect(invokeMock).toHaveBeenCalledWith("meeting_recorder_start", { title: "Planning" });
    expect(await meeting.pause()).toMatchObject({ id: 9, status: "paused" });
    expect(invokeMock).toHaveBeenCalledWith("meeting_recorder_pause", { captureId: 9 });
    expect(meeting.controls.value).toEqual(["resume", "stop", "cancel"]);
    expect((await meeting.cancel(false))?.status).toBe("confirmation_required");
    expect(invokeMock).toHaveBeenCalledWith("meeting_recorder_cancel", { captureId: 9, confirmed: false });
  });
});

describe("floating controller free capture", () => {
  it("does not run main-window recovery or subscribe to tray start requests", async () => {
    vi.resetModules();
    listenMock.mockClear();
    invokeMock.mockImplementation(async (command) => command === "meeting_recorder_status" ? { capture: null } : null);
    const { useMeetingCapture: controllerCapture } = await import("./useMeetingCapture");
    controllerCapture({ enableMainWindowFlows: false });
    await flushPromises();
    expect(invokeMock.mock.calls.map(([command]) => command)).not.toContain("capture_recovery_discover");
    expect(listenMock.mock.calls.map(([event]) => event)).not.toContain("murmur://meeting-start-requested");
    expect(invokeMock.mock.calls.map(([command]) => command)).toContain("meeting_recorder_status");
  });

  it.each(["active", "expired", "purchased", "corrupt", "missing"])(
    "resumes a paused meeting without inspecting legacy %s license state",
    async () => {
      vi.resetModules();
      const paused = { id: 71, status: "paused", title: "Controller fixture" };
      invokeMock.mockImplementation(async (command) => {
        if (command === "meeting_recorder_status") return { capture: paused };
        if (command === "meeting_recorder_resume") return { ...paused, status: "recording" };
        if (command === "get_trial_state") throw new Error("legacy trial state must not be read");
        return null;
      });
      const { useMeetingCapture: controllerCapture } = await import("./useMeetingCapture");
      const controller = controllerCapture();
      await vi.waitFor(() => expect(controller.activeCapture.value?.status).toBe("paused"));
      expect(await controller.resume()).toMatchObject({ id: 71, status: "recording" });
      expect(invokeMock.mock.calls.filter(([command]) => command === "meeting_recorder_resume")).toHaveLength(1);
      expect(invokeMock.mock.calls.map(([command]) => command)).not.toContain("get_trial_state");
    },
  );

  it.each(["start", "resume"] as const)("claims %s before async native completion so repeated clicks do not duplicate capture", async (operation) => {
    vi.resetModules();
    let capture: { id: number; status: string } | null = operation === "resume" ? { id: 73, status: "paused" } : null;
    let complete!: (value: unknown) => void;
    invokeMock.mockImplementation(async (command) => {
      if (command === "meeting_recorder_status") return { capture };
      if (command === `meeting_recorder_${operation}`) {
        const result = await new Promise((resolve) => { complete = resolve; });
        capture = result as typeof capture;
        return result;
      }
      return null;
    });
    const { useMeetingCapture: controllerCapture } = await import("./useMeetingCapture");
    const controller = controllerCapture();
    await vi.waitFor(() => expect(controller.activeCapture.value?.id ?? null).toBe(operation === "resume" ? 73 : null));
    const first = operation === "start" ? controller.start("Fixture") : controller.resume();
    const second = operation === "start" ? controller.start("Fixture") : controller.resume();
    expect(controller.action.value).toBe(operation);
    expect(await second).toBeNull();
    complete({ id: 73, status: "recording" });
    await first;
    expect(invokeMock.mock.calls.filter(([command]) => command === `meeting_recorder_${operation}`)).toHaveLength(1);
    expect(controller.action.value).toBeNull();
    expect(controller.activeCapture.value?.status).toBe("recording");
  });
});

describe("meeting start fast path", () => {
  const READY = {
    feature_enabled: true,
    supported: true,
    minimum_macos_version: "14.4",
    microphone: "granted",
    persistence_durable: true,
    stt_model_ready: true,
    settings_ready: true,
  };

  function mockBackend(capabilities: Record<string, unknown>, startFails = false) {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "meeting_capture_capabilities") return capabilities;
      if (command === "meeting_recorder_status") return { capture: null, active_duration_ms: 0, paused_duration_ms: 0, paused: false };
      if (command === "meeting_recorder_start") {
        if (startFails) throw { code: "capture_busy", message: "busy" };
        return { id: 11, status: "recording", title: "Meeting 2026-08-02 15:04" };
      }
      if (command === "capture_recovery_discover") return null;
      if (command === "get_trial_state") throw new Error("legacy trial state must not be read");
      if (!command) return null;
      throw new Error(`unexpected command ${command}`);
    });
  }

  beforeEach(async () => {
    localStorage.setItem(MEETING_RECORDING_NOTICE_KEY, "true");
    mockBackend(READY);
    await useMeetingCapture().hydrate();
  });

  it("starts without a license read, with a timestamp title, when every recording prerequisite is met", async () => {
    mockBackend(READY);
    expect(await useMeetingCapture().resolveStartFlow()).toBe("started");
    const startCall = invokeMock.mock.calls.find(([command]) => command === "meeting_recorder_start");
    expect(startCall?.[1].title).toMatch(/^Meeting \d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
    expect(invokeMock.mock.calls.map(([command]) => command)).not.toContain("get_trial_state");
  });

  it("re-reads capabilities rather than trusting the cached ref", async () => {
    mockBackend(READY);
    await useMeetingCapture().resolveStartFlow();
    expect(invokeMock).toHaveBeenCalledWith("meeting_capture_capabilities");
  });

  it.each([
    ["microphone not granted", { ...READY, microphone: "not-determined" }],
    ["microphone denied", { ...READY, microphone: "denied" }],
    ["unsupported macOS", { ...READY, supported: false }],
    ["store not durable", { ...READY, persistence_durable: false }],
    ["feature disabled", { ...READY, feature_enabled: false }],
    ["speech model missing", { ...READY, stt_model_ready: false, settings_ready: false }],
    ["settings loading", { ...READY, settings_ready: false }],
  ])("falls back to the dialog when %s", async (_label, capabilities) => {
    mockBackend(capabilities);
    expect(await useMeetingCapture().resolveStartFlow()).toBe("dialog");
    expect(invokeMock).not.toHaveBeenCalledWith("meeting_recorder_start", expect.anything());
  });

  it("falls back to the dialog until the first-recording consent notice has been shown", async () => {
    localStorage.removeItem(MEETING_RECORDING_NOTICE_KEY);
    mockBackend(READY);
    expect(await useMeetingCapture().resolveStartFlow()).toBe("dialog");
    expect(invokeMock).not.toHaveBeenCalledWith("meeting_recorder_start", expect.anything());
  });

  it("falls back to the dialog when the start fails, so the error is visible", async () => {
    mockBackend(READY, true);
    const meeting = useMeetingCapture();
    expect(await meeting.resolveStartFlow()).toBe("dialog");
    expect(meeting.error.value?.code).toBe("capture_busy");
    await meeting.refreshCapabilities();
    expect(meeting.error.value?.code).toBe("capture_busy");
  });
});
