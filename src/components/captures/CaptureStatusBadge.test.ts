import "@/test/installEnglishI18n";
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import CaptureStatusBadge from "@/components/captures/CaptureStatusBadge.vue";

describe("CaptureStatusBadge", () => {
  it("follows a capture through its whole lifecycle", async () => {
    // A capture's status changes underneath this badge while the row stays
    // mounted. Reading the props once at setup left every finished capture
    // still labelled "Recording" in the library.
    const badge = mount(CaptureStatusBadge, {
      props: { status: "recording" as const, markdownStatus: "not_applicable" as const },
    });
    expect(badge.text()).toBe("Recording");

    await badge.setProps({ status: "transcribing" as const });
    expect(badge.text()).toBe("Transcribing audio");

    await badge.setProps({ status: "completed" as const, markdownStatus: "available" as const });
    expect(badge.text()).toBe("Ready");
  });

  it("can omit an assumed-ready state while retaining completed warnings", async () => {
    const badge = mount(CaptureStatusBadge, {
      props: {
        status: "completed" as const,
        markdownStatus: "available" as const,
        showReady: false,
      },
    });
    expect(badge.text()).toBe("");

    await badge.setProps({ markdownStatus: "missing" as const });
    expect(badge.text()).toBe("Missing file");
  });
});
