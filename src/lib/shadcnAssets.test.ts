import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

describe("retained shadcn-vue UI assets", () => {
  it("keeps the pinned stylesheet byte-for-byte without installing the generator", () => {
    const stylesheet = readFileSync("src/styles/shadcn-vue.css");
    expect(createHash("sha256").update(stylesheet).digest("hex"))
      .toBe("e93f9da1091a7a124b9395adfdba6329c2a7b18aceee1acea95ccfa4b36a23ba");
    const manifest = JSON.parse(readFileSync("package.json", "utf8"));
    expect(manifest.dependencies?.["shadcn-vue"]).toBeUndefined();
    expect(manifest.devDependencies?.["shadcn-vue"]).toBeUndefined();
    expect(readFileSync("src/style.css", "utf8")).toContain('@import "./styles/shadcn-vue.css"');
  });

  it("retains upstream attribution in the source tree and bundled notices", () => {
    const license = readFileSync("third-party/shadcn-vue/LICENSE", "utf8").trim();
    expect(license).toContain("MIT License");
    expect(license).toContain("Copyright (c) 2023 unovue");
    expect(readFileSync("src-tauri/resources/THIRD-PARTY-NOTICES.txt", "utf8")).toContain(license);
    const provenance = readFileSync("third-party/shadcn-vue/README.md", "utf8");
    expect(provenance).toContain("shadcn-vue 2.8.2");
    expect(provenance).toContain("e93f9da1091a7a124b9395adfdba6329c2a7b18aceee1acea95ccfa4b36a23ba");
  });
});
