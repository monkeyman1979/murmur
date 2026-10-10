import { describe, expect, it } from "vitest";
import { pluralCategory, timeUnitKey } from "./plurals";

describe("localized quantities", () => {
  it("selects Russian one/few/many even when counts exceed twenty", () => {
    expect([1, 2, 5, 21, 22, 25].map((n) => pluralCategory(n, "ru"))).toEqual(["one", "few", "many", "one", "few", "many"]);
    expect([1, 2, 5, 21].map((n) => timeUnitKey("hours", n, "ru"))).toEqual([
      "timeUnits.hour", "timeUnits.hourFew", "timeUnits.hours", "timeUnits.hour",
    ]);
  });
});
