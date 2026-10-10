import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

const STORAGE_KEY = "murmur:vocabulary";

/**
 * Reactive control for the user's vocabulary list — words / names /
 * acronyms fed to Whisper as an `initial_prompt` bias so they
 * transcribe correctly on the first pass (SPEC §2.5).
 *
 * Frontend is the source of truth: persisted to `localStorage` and
 * mirrored into Rust via `set_vocabulary` on every change (plus once
 * on first mount so the atomic list is populated before the first
 * hotkey press). Rust dedupes case-insensitively and trims whitespace
 * — we do the same here so the UI reflects what the backend actually
 * stores, but the backend's dedupe is the authoritative one.
 */
function loadStored(): string[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((x): x is string => typeof x === "string")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  } catch {
    return [];
  }
}

const words = ref<string[]>(loadStored());
let initialized = false;

function init(): void {
  if (initialized) return;
  initialized = true;

  watch(
    words,
    (value) => {
      if (typeof localStorage !== "undefined") {
        try {
          localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
        } catch {
          // Quota or private-mode — session-only persistence is fine.
        }
      }
      void invoke("set_vocabulary", { words: value }).catch((err) => {
        console.error("[murmur] set_vocabulary failed", err);
      });
    },
    { immediate: true, deep: true },
  );
}

function addWord(raw: string): boolean {
  const trimmed = raw.trim();
  if (!trimmed) return false;
  if (words.value.some((w) => w.toLowerCase() === trimmed.toLowerCase())) {
    return false;
  }
  words.value = [...words.value, trimmed];
  return true;
}

function removeWord(word: string): void {
  words.value = words.value.filter((w) => w !== word);
}

function clearAll(): void {
  words.value = [];
}

export function useVocabulary() {
  init();
  return { words, addWord, removeWord, clearAll };
}
