import { describe, expect, it } from "vitest";
import { shouldOpenMainWindow } from "./startupWindow";

describe("startup window", () => {
  it("shows a deliberate cold launch after onboarding", () => {
    expect(shouldOpenMainWindow(true, false)).toBe(true);
  });

  it("keeps completed installations quiet only on login startup", () => {
    expect(shouldOpenMainWindow(true, true)).toBe(false);
  });

  it.each([false, true])("shows unfinished onboarding even on login startup (login=%s)", (launchedAtLogin) => {
    expect(shouldOpenMainWindow(false, launchedAtLogin)).toBe(true);
  });
});
