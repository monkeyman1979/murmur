import "@/test/installEnglishI18n";
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import ProcessingProgress from "@/components/captures/ProcessingProgress.vue";

describe("ProcessingProgress", () => {
  it("explains the initial transcription phase and progress units", () => {
    const progress = mount(ProcessingProgress, {
      props: {
        status: "transcribing",
        processing: {
          completed_units: 0,
          total_units: 4,
          fraction: 0,
          estimated_remaining_ms: null,
        },
      },
    });

    expect(progress.text()).toContain("Starting transcription");
    expect(progress.text()).toContain("0 of 4 parts");
    expect(progress.get('[role="progressbar"]').attributes("aria-label")).toBe(
      "Starting transcription: 0 of 4 parts",
    );
    expect(progress.get(".progress-fill").attributes("style")).toContain("width: 0%");
  });

  it("shows the short queued state during a new recording, then resumes progress copy", async () => {
    const progress = mount(ProcessingProgress, {
      props: {
        status: "transcribing",
        recordingActive: true,
        processing: { completed_units: 0, total_units: 4, fraction: 0, estimated_remaining_ms: null },
      },
    });
    expect(progress.text()).toContain("Queued during recording");
    expect(progress.get('[role="progressbar"]').attributes("aria-label")).toBe("Queued during recording: 0 of 4 parts");
    await progress.setProps({ recordingActive: false, transcriptionQueued: true });
    expect(progress.text()).toContain("Queued for transcription");
    await progress.setProps({ transcriptionQueued: false });
    expect(progress.text()).toContain("Starting transcription");
  });

  it("uses saving copy for finalization", () => {
    const progress = mount(ProcessingProgress, {
      props: {
        status: "finalizing",
        processing: {
          completed_units: 4,
          total_units: 4,
          fraction: 1,
          estimated_remaining_ms: null,
        },
      },
    });

    expect(progress.text()).toContain("Saving transcript");
    expect(progress.get(".progress-fill").attributes("style")).toContain("width: 100%");
  });
});
