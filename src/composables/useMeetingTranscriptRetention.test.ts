import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  localStorage.clear();
  invokeMock.mockResolvedValue(0);
});

describe("useMeetingTranscriptRetention", () => {
  it("defaults to keeping meeting transcripts forever and mirrors changes", async () => {
    const { useMeetingTranscriptRetention } = await import(
      "@/composables/useMeetingTranscriptRetention"
    );
    const { days } = useMeetingTranscriptRetention();

    expect(days.value).toBe(0);
    expect(invokeMock).toHaveBeenCalledWith("set_meeting_transcript_retention", { days: 0 });

    days.value = 30;
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_meeting_transcript_retention", { days: 30 });
    });
    expect(localStorage.getItem("murmur:meeting-transcript-retention")).toBe("30");
  });

  it("falls back to keep forever when the stored value is invalid", async () => {
    localStorage.setItem("murmur:meeting-transcript-retention", "999");
    const { useMeetingTranscriptRetention } = await import(
      "@/composables/useMeetingTranscriptRetention"
    );

    expect(useMeetingTranscriptRetention().days.value).toBe(0);
  });
});
