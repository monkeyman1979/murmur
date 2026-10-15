import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  localStorage.clear();
  invokeMock.mockResolvedValue(undefined);
});

describe("useMeetingAudioRetention", () => {
  it("offers bounded windows only, defaulting to 24 hours", async () => {
    const { MEETING_AUDIO_RETENTION_OPTIONS, useMeetingAudioRetention } = await import(
      "@/composables/useMeetingAudioRetention"
    );

    expect(MEETING_AUDIO_RETENTION_OPTIONS.map((option) => option.value)).toEqual([
      "delete_immediately", "after_24_hours", "after_7_days",
    ]);
    expect(useMeetingAudioRetention().retention.value).toBe("after_24_hours");
    expect(invokeMock).toHaveBeenCalledWith("set_meeting_audio_retention", { retention: "after_24_hours" });
  });

  it("moves a saved v0.1.1 Until deleted choice to 7 days and persists it", async () => {
    localStorage.setItem("murmur:meeting-audio-retention", "keep");
    const { useMeetingAudioRetention } = await import("@/composables/useMeetingAudioRetention");

    expect(useMeetingAudioRetention().retention.value).toBe("after_7_days");
    expect(invokeMock).toHaveBeenCalledWith("set_meeting_audio_retention", { retention: "after_7_days" });
    expect(localStorage.getItem("murmur:meeting-audio-retention")).toBe("after_7_days");
  });
});
