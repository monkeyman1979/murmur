import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { createI18n } from "vue-i18n";
import { parse } from "vue/compiler-sfc";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";
import { supportedAppLanguages } from "./appLanguage";

type Catalog = Record<string, unknown>;
const catalogs = { es, fr, ru };

function at(catalog: Catalog, path: string): unknown {
  return path.split(".").reduce<unknown>((value, segment) =>
    typeof value === "object" && value !== null ? (value as Catalog)[segment] : undefined, catalog);
}

function leaves(value: unknown, prefix = ""): string[] {
  if (typeof value === "string") return [prefix];
  if (typeof value !== "object" || value === null || Array.isArray(value)) return [];
  return Object.entries(value).flatMap(([key, child]) => leaves(child, prefix ? `${prefix}.${key}` : key));
}

function placeholders(text: string): string[] {
  return [...text.matchAll(/\{([\w]+)\}/g)].map((match) => match[1]).sort();
}

describe.each(Object.entries(catalogs))("%s trial catalog", (locale, catalog) => {
  it("covers every English key and interpolation without fallback", () => {
    expect(leaves(catalog).sort()).toEqual(leaves(en).sort());
    for (const key of leaves(en)) {
      const source = at(en, key) as string;
      const translated = at(catalog, key) as string;
      expect(translated.trim(), `${locale}.${key}`).not.toBe("");
      expect(placeholders(translated), `${locale}.${key}`).toEqual(placeholders(source));
    }
    const i18n = createI18n({ legacy: false, locale, fallbackLocale: "en", messages: { en, [locale]: catalog } });
    expect(i18n.global.t("meetingController.activeTime", { status: "X", time: "02:00" })).toContain("02:00");
    for (const count of [1, 2, 5]) {
      const plural = new Intl.PluralRules(locale).select(count);
      const label = i18n.global.t(`historyPage.counts.entries.${plural}`);
      expect(label).not.toMatch(/^historyPage\./);
      expect(i18n.global.t("historyPage.total", { count, label })).toContain(String(count));
    }
  });
});

it("offers only complete trial catalogs", () => {
  expect(supportedAppLanguages.map(({ code }) => code)).toEqual(["en", "es", "fr", "ru"]);
});

it("resolves every literal t('key') in Vue and TypeScript sources", () => {
  const missing: string[] = [];
  function visit(directory: string): void {
    for (const name of readdirSync(directory)) {
      const path = join(directory, name);
      if (statSync(path).isDirectory()) visit(path);
      else if (/\.(ts|vue)$/.test(name) && !name.endsWith('.test.ts')) {
        const source = readFileSync(path, 'utf8');
        for (const match of source.matchAll(/(?:\bt|\.t)\(\s*['"]([\w.-]+)['"]/g)) {
          if (match[1].includes('.') && at(en, match[1]) === undefined) missing.push(`${path}: ${match[1]}`);
        }
      }
    }
  }
  visit(join(process.cwd(), 'src'));
  expect(missing).toEqual([]);
});

it("keeps visible Vue copy and accessible attributes out of hardcoded templates", () => {
  const remaining: string[] = [];
  const properNames = new Set(["Murmur", "Whisper", "com.monkeyman.murmur", "monkeyman1979 (GitHub)", "MIT"]);
  function visit(directory: string): void {
    for (const name of readdirSync(directory)) {
      const path = join(directory, name);
      if (statSync(path).isDirectory()) {
        if (name !== 'ui') visit(path);
        continue;
      }
      if (!name.endsWith('.vue')) continue;
      const ast = parse(readFileSync(path, 'utf8')).descriptor.template?.ast;
      function scan(node: { type: number; content?: string; loc?: { start: { line: number } }; props?: Array<{ type: number; name: string; value?: { content: string } }>; children?: unknown[] }): void {
        if (node.type === 2 && node.content) record(node.content, node.loc?.start.line);
        if (node.type === 1) for (const prop of node.props ?? []) {
          if (prop.type === 6 && ['title', 'aria-label', 'placeholder', 'alt', 'aria-description'].includes(prop.name)) {
            record(prop.value?.content ?? '', node.loc?.start.line);
          }
        }
        for (const child of node.children ?? []) scan(child as typeof node);
      }
      function record(value: string, line?: number): void {
        const copy = value.trim();
        if (/[a-z]{3}/i.test(copy) && !properNames.has(copy)) remaining.push(`${path}:${line}: ${copy}`);
      }
      if (ast) scan(ast);
    }
  }
  visit(join(process.cwd(), 'src'));
  expect(remaining).toEqual([]);
});
