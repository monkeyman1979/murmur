import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

const STORAGE_KEY = "murmur:corrections";

/**
 * A single find/replace rule the user has configured (SPEC §2.6).
 * Shape matches the Rust `CorrectionRule` struct; serde (de)serializes
 * through the `set_corrections` Tauri command verbatim.
 */
export interface CorrectionRule {
  find: string;
  replace: string;
  caseSensitive: boolean;
  wholeWord: boolean;
}

/**
 * Reactive control for the user's corrections list — deterministic
 * find/replace applied to the Whisper transcript before paste.
 *
 * Frontend is the source of truth: persisted to `localStorage` and
 * mirrored into Rust via `set_corrections` on every change (plus once
 * on first mount so the state is populated before the first hotkey
 * press). Rust dedupes by (find + caseSensitive + wholeWord) and
 * drops empty `find` entries; the UI does the same so what the user
 * sees matches what the pipeline applies, but the backend remains
 * authoritative.
 */

// Wire-format rule — serde on the Rust side uses snake_case by default.
// Keeping the camelCase shape in TS so Vue templates read naturally.
interface WireRule {
  find: string;
  replace: string;
  case_sensitive: boolean;
  whole_word: boolean;
}

function toWire(r: CorrectionRule): WireRule {
  return {
    find: r.find,
    replace: r.replace,
    case_sensitive: r.caseSensitive,
    whole_word: r.wholeWord,
  };
}

function fromWire(r: WireRule): CorrectionRule {
  return {
    find: r.find,
    replace: r.replace,
    caseSensitive: r.case_sensitive,
    wholeWord: r.whole_word,
  };
}

function isWireRule(x: unknown): x is WireRule {
  if (!x || typeof x !== "object") return false;
  const r = x as Record<string, unknown>;
  return (
    typeof r.find === "string" &&
    typeof r.replace === "string" &&
    typeof r.case_sensitive === "boolean" &&
    typeof r.whole_word === "boolean"
  );
}

function loadStored(): CorrectionRule[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(isWireRule)
      .map(fromWire)
      .filter((r) => r.find.trim().length > 0);
  } catch {
    return [];
  }
}

const rules = ref<CorrectionRule[]>(loadStored());
let initialized = false;

function init(): void {
  if (initialized) return;
  initialized = true;

  watch(
    rules,
    (value) => {
      const wire = value.map(toWire);
      if (typeof localStorage !== "undefined") {
        try {
          localStorage.setItem(STORAGE_KEY, JSON.stringify(wire));
        } catch {
          // Quota or private-mode — session-only persistence is fine.
        }
      }
      void invoke("set_corrections", { rules: wire }).catch((err) => {
        console.error("[murmur] set_corrections failed", err);
      });
    },
    { immediate: true, deep: true },
  );
}

function sameTarget(a: CorrectionRule, b: CorrectionRule): boolean {
  return (
    a.find.trim().toLowerCase() === b.find.trim().toLowerCase() &&
    a.caseSensitive === b.caseSensitive &&
    a.wholeWord === b.wholeWord
  );
}

function addRule(rule: CorrectionRule): boolean {
  const trimmed: CorrectionRule = { ...rule, find: rule.find.trim() };
  if (!trimmed.find) return false;
  if (rules.value.some((existing) => sameTarget(existing, trimmed))) {
    return false;
  }
  // Prepend so the newest rule lands at the top of the list — matches
  // the History page's newest-first sort and gives immediate visual
  // feedback after adding via the wand dialog. Because the Rust
  // pipeline iterates this list top-down in `apply()`, prepending
  // also means newer rules take precedence over older overlapping
  // ones if any ever collide. In practice corrections don't chain,
  // so this is a consistent-direction improvement both visually and
  // semantically.
  rules.value = [trimmed, ...rules.value];
  return true;
}

function removeRule(rule: CorrectionRule): void {
  rules.value = rules.value.filter((r) => !sameTarget(r, rule));
}

function clearAll(): void {
  rules.value = [];
}

export function useCorrections() {
  init();
  return { rules, addRule, removeRule, clearAll };
}
