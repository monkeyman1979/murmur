import { describe, expect, it } from "vitest";
import type { RouteRecordRaw } from "vue-router";

import { createAppRoutes } from "@/router";

interface RouteEntry {
  path: string;
  route: RouteRecordRaw;
}

function flattenRoutes(routes: RouteRecordRaw[], parentPath = ""): RouteEntry[] {
  return routes.flatMap((route) => {
    const path = route.path.startsWith("/")
      ? route.path
      : `${parentPath.replace(/\/$/, "")}/${route.path}`;
    return [
      { path, route },
      ...flattenRoutes(route.children ?? [], path),
    ];
  });
}

function routeEntries(): RouteEntry[] {
  return flattenRoutes(createAppRoutes());
}

function paths(): string[] {
  return routeEntries().map(({ path }) => path);
}

function routeAtPath(path: string): RouteRecordRaw | undefined {
  return routeEntries().find((entry) => entry.path === path)?.route;
}

describe("consolidated navigation routes", () => {
  it("houses vocabulary and corrections under Dictionary", () => {
    const routes = createAppRoutes();
    expect(paths()).toContain("/dictionary");
    expect(routes.find((route) => route.path === "/vocabulary")?.redirect).toEqual({
      name: "dictionary",
      query: { section: "vocabulary" },
    });
    expect(routes.find((route) => route.path === "/corrections")?.redirect).toEqual({
      name: "dictionary",
      query: { section: "corrections" },
    });
  });

  it("keeps Settings tabs under one persistent parent layout", () => {
    const settings = createAppRoutes().find(
      (route) => route.path === "/settings",
    );
    expect(settings?.children?.map((route) => route.name)).toEqual([
      undefined,
      "settings-general",
      "settings-models",
      "settings-permissions",
      "settings-about",
      "settings-acknowledgements",
    ]);
  });

  it("includes offline acknowledgements under Settings", () => {
    expect(paths()).toContain("/settings/acknowledgements");
  });

  it("does not offer the retired snippets placeholder", () => {
    expect(paths()).not.toContain("/snippets");
  });

  it("redirects the retired Models page to the Speech model settings card", () => {
    const route = routeAtPath("/settings/models");
    expect(route?.redirect).toEqual({
      name: "settings-general",
      query: { section: "speech-model" },
    });
  });
});

describe("production meeting routes", () => {
  it("includes the Meetings list and detail routes by default", () => {
    expect(paths()).toContain("/captures");
    expect(paths()).toContain("/captures/:captureId");
  });

  it("keeps detail navigation active through the captures prefix", () => {
    const route = routeAtPath("/captures/:captureId");
    expect(route?.path).toBe("/captures/:captureId");
  });
});
