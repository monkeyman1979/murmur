import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

function svg(name: string) {
  return new DOMParser().parseFromString(
    readFileSync(resolve("src/assets/brand", name), "utf8"),
    "image/svg+xml",
  );
}
function geometry(document: Document) {
  return Array.from(document.querySelectorAll("path"), (path) => ({
    id: path.id,
    d: path.getAttribute("d"),
    fillRule: path.getAttribute("fill-rule"),
  }));
}

describe("Murmur brand assets", () => {
  it("keeps the black and white source marks geometrically identical, including the corrected nose", () => {
    const black = svg("murmur-logo-black.svg");
    const white = svg("murmur-brand-white.svg");
    expect(black.querySelector("parsererror")).toBeNull();
    expect(white.querySelector("parsererror")).toBeNull();
    expect(geometry(black)).toEqual(geometry(white));
    expect(black.querySelector("path#nose")).not.toBeNull();
    expect(black.documentElement.getAttribute("viewBox")).toBe(white.documentElement.getAttribute("viewBox"));
    expect(black.querySelector("g")!.getAttribute("fill")!.toLowerCase()).toBe("#000000");
    expect(white.querySelector("g")!.getAttribute("fill")!.toLowerCase()).toBe("#ffffff");
  });

  it("embeds the current white mark in the app icon without the obsolete coordinate transform", () => {
    const icon = svg("murmur-app-icon.svg");
    expect(icon.querySelector("parsererror")).toBeNull();
    expect(geometry(icon)).toEqual(geometry(svg("murmur-brand-white.svg")));
    expect(icon.documentElement.getAttribute("viewBox")).toBe("0 0 1024 1024");
    const transforms = Array.from(icon.querySelectorAll("[transform]"), (element) => element.getAttribute("transform"));
    expect(transforms).toEqual(["translate(112 111) scale(0.75)"]);
    expect(icon.querySelector("path#nose")!.parentElement!.getAttribute("fill")!.toLowerCase()).toBe("#ffffff");
    expect(icon.querySelectorAll("linearGradient, radialGradient")).toHaveLength(2);
  });

  it("uses the composed Murmur icon for the active favicon", () => {
    const html = readFileSync(resolve("index.html"), "utf8");
    expect(html).toContain('href="/src/assets/brand/murmur-app-icon.svg"');
    expect(html).toContain("<title>Murmur</title>");
    expect(html).not.toContain("/vite.svg");
  });
});
