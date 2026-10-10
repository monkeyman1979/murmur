import { describe, expect, it } from "vitest";

import {
  retentionChangeRequiresConfirmation,
  retentionUpdatedMessage,
} from "@/lib/retention";

describe("retention change confirmation policy", () => {
  it("confirms when changing from forever or a longer window to a shorter window", () => {
    expect(retentionChangeRequiresConfirmation(0, 7)).toBe(true);
    expect(retentionChangeRequiresConfirmation(90, 30)).toBe(true);
  });

  it("does not confirm when keeping, lengthening, or disabling automatic deletion", () => {
    expect(retentionChangeRequiresConfirmation(7, 7)).toBe(false);
    expect(retentionChangeRequiresConfirmation(7, 30)).toBe(false);
    expect(retentionChangeRequiresConfirmation(7, 0)).toBe(false);
    expect(retentionChangeRequiresConfirmation(0, 0)).toBe(false);
  });

  it("describes the applied retention setting concisely", () => {
    expect(retentionUpdatedMessage("dictations", 30, "30 days"))
      .toBe("Dictations will be deleted after 30 days.");
    expect(retentionUpdatedMessage("meetings", 0, "Keep forever"))
      .toBe("Meeting transcripts will be kept forever.");
  });
});
