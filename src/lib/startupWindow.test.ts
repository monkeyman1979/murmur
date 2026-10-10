import { describe, expect, it } from "vitest";
import { shouldOpenMainWindow } from "./startupWindow";

describe("startup window", () => {
  it("keeps returning users in the menu bar", () => {
    expect(shouldOpenMainWindow(true)).toBe(false);
  });

  it("shows first-run and unfinished onboarding", () => {
    expect(shouldOpenMainWindow(false)).toBe(true);
  });
});
