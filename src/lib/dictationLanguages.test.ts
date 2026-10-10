import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { DICTATION_LANGUAGE_CODES, displayDictationLanguages } from "./dictationLanguages";

const whisperSource = join(process.cwd(), "src-tauri/vendor/whisper-rs-sys/whisper.cpp/src/whisper.cpp");

describe("informational language list for pinned Whisper Small Multilingual", () => {
  it("tracks model language tokens 0–98 in the vendored decoder, not an incomplete UI picker", () => {
    const source = readFileSync(whisperSource, "utf8");
    const map = source.match(/static const std::map<[^\n]+g_lang = \{([\s\S]*?)\n\};/)?.[1];
    expect(map).toBeDefined();
    const entries = [...map!.matchAll(/\{\s*"([a-z]+)",\s*\{\s*(\d+),/g)]
      .map(([, code, id]) => ({ code, id: Number(id) }));
    expect(entries).toHaveLength(100); // decoder also knows yue (ID 99)
    expect(entries.filter(({ id }) => id < 99).map(({ code }) => code)).toEqual([...DICTATION_LANGUAGE_CODES]);
    expect(DICTATION_LANGUAGE_CODES).toHaveLength(99);
  });

  it("lists each code once, including Portuguese, with names following App language", () => {
    for (const locale of ["en", "es", "fr", "ru"]) {
      const languages = displayDictationLanguages(locale);
      expect(languages).toHaveLength(99);
      expect(new Set(languages.map(({ code }) => code)).size).toBe(99);
      expect(languages.every(({ name }) => name.length > 1)).toBe(true);
    }
    expect(displayDictationLanguages("en").find(({ code }) => code === "pt")?.name).toBe("Portuguese");
    expect(displayDictationLanguages("ru").find(({ code }) => code === "pt")?.name).toBe("португальский");
  });
});
