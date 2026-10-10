import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";
import {
  DEFAULT_BINDING,
  isHotkeyBinding,
  type HotkeyBinding,
} from "@/lib/hotkey";

const STORAGE_KEY = "murmur:hotkey-binding";

function loadStored(): HotkeyBinding {
  if (typeof localStorage === "undefined") return DEFAULT_BINDING;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_BINDING;
    const parsed = JSON.parse(raw) as unknown;
    return isHotkeyBinding(parsed) ? parsed : DEFAULT_BINDING;
  } catch {
    return DEFAULT_BINDING;
  }
}

const binding = ref<HotkeyBinding>(loadStored());
let initialized = false;

function init(): void {
  if (initialized) return;
  initialized = true;

  watch(
    binding,
    (value) => {
      if (typeof localStorage !== "undefined") {
        try {
          localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
        } catch {
          // Quota / private mode — session-only is fine.
        }
      }
      void invoke("set_hotkey_binding", { binding: value }).catch((err) => {
        console.error("[murmur] set_hotkey_binding failed", err);
      });
    },
    { immediate: true, deep: true },
  );
}

export function useHotkeyBinding() {
  init();
  return { binding };
}
