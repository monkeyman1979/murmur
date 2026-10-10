import { afterEach, describe, expect, it, vi } from "vitest";

import {
  formatDateBucket,
  formatInteger,
  formatDurationParts,
  groupByLocalDate,
} from "@/lib/format";

const localTime = (year: number, month: number, day: number, hour = 12) =>
  new Date(year, month - 1, day, hour).getTime();

afterEach(() => vi.useRealTimers());

describe("formatDurationParts", () => {
  it("separates day and hour values from their full unit labels", () => {
    expect(formatDurationParts((2 * 24 + 12) * 60 * 60 * 1000)).toEqual([
      { value: "2", unit: "days" },
      { value: "12", unit: "hours" },
    ]);
  });

  it("uses singular labels and omits an empty remainder", () => {
    expect(formatDurationParts(24 * 60 * 60 * 1000)).toEqual([
      { value: "1", unit: "day" },
    ]);
    expect(formatDurationParts(60 * 60 * 1000)).toEqual([
      { value: "1", unit: "hour" },
    ]);
  });
});

describe("date buckets", () => {
  it("uses the selected app locale for numbers and older dates", () => {
    vi.useFakeTimers();
    vi.setSystemTime(localTime(2026, 9, 5));
    const older = localTime(2026, 9, 3);
    expect(formatInteger(1234, "en")).toBe("1,234");
    expect(formatInteger(1234, "es")).toBe("1234");
    expect(formatDateBucket(older, "es")).toBe(new Intl.DateTimeFormat("es", { year: "numeric", month: "long", day: "numeric" }).format(new Date(older)));
  });

  it("uses relative labels for today and yesterday", () => {
    vi.useFakeTimers();
    vi.setSystemTime(localTime(2026, 9, 5));

    expect(formatDateBucket(localTime(2026, 9, 5, 8))).toBe("Today");
    expect(formatDateBucket(localTime(2026, 9, 4, 20))).toBe("Yesterday");
  });

  it("groups rows by local calendar day while preserving their order", () => {
    vi.useFakeTimers();
    vi.setSystemTime(localTime(2026, 9, 5));
    const rows = [
      { id: 3, timestamp: localTime(2026, 9, 5, 10) },
      { id: 2, timestamp: localTime(2026, 9, 5, 8) },
      { id: 1, timestamp: localTime(2026, 9, 4, 18) },
    ];

    const groups = groupByLocalDate(rows, (row) => row.timestamp);

    expect(groups.map((group) => ({
      key: group.key,
      label: group.label,
      ids: group.rows.map((row) => row.id),
    }))).toEqual([
      { key: "2026-09-05", label: "Today", ids: [3, 2] },
      { key: "2026-09-04", label: "Yesterday", ids: [1] },
    ]);
  });
});
