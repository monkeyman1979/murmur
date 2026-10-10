import { describe, expect, it } from "vitest";

import {
  anchoredScrollTop,
  routeScrollTarget,
  shouldShowScrollToTop,
} from "./scroll";

describe("route scroll restoration", () => {
  it("resets different top-level destinations to the top", () => {
    expect(routeScrollTarget({
      fromName: "home",
      toName: "history",
      fromPage: "/",
      toPage: "/history",
      capturesListScrollTop: 0,
    })).toBe(0);
  });

  it("restores the Meetings list after viewing a transcript", () => {
    expect(routeScrollTarget({
      fromName: "capture-detail",
      toName: "captures",
      fromPage: "/captures/:captureId",
      toPage: "/captures",
      capturesListScrollTop: 1240,
    })).toBe(1240);
  });

  it("does not reset navigation within one persistent page", () => {
    expect(routeScrollTarget({
      fromName: "settings-general",
      toName: "settings-about",
      fromPage: "/settings",
      toPage: "/settings",
      capturesListScrollTop: 0,
    })).toBeNull();
  });

  it("restores a selected row to its previous viewport position", () => {
    expect(anchoredScrollTop({
      currentScrollTop: 900,
      containerTop: 32,
      rowTop: 412,
      savedRowOffset: 180,
    })).toBe(1100);
  });
});

describe("scroll-to-top visibility", () => {
  it("requires both overflow and the reveal distance", () => {
    expect(shouldShowScrollToTop({
      scrollTop: 479,
      scrollHeight: 1600,
      clientHeight: 800,
    })).toBe(false);
    expect(shouldShowScrollToTop({
      scrollTop: 480,
      scrollHeight: 1600,
      clientHeight: 800,
    })).toBe(true);
    expect(shouldShowScrollToTop({
      scrollTop: 600,
      scrollHeight: 800,
      clientHeight: 800,
    })).toBe(false);
  });
});
