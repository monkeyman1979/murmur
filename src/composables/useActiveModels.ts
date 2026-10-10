import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";
import { MULTILINGUAL_MODEL_ID } from "@/composables/useModelDownload";

const STT_STORAGE_KEY = "murmur:active-stt";

function loadStt(): string {
  if (typeof localStorage === "undefined") return MULTILINGUAL_MODEL_ID;
  return localStorage.getItem(STT_STORAGE_KEY) ?? MULTILINGUAL_MODEL_ID;
}

const activeSttId = ref<string>(loadStt());

let initialized = false;

function init(): void {
  if (initialized) return;
  initialized = true;

  // Push persisted ids to Rust on boot so the hotkey pipeline knows
  // which models to use before the first press, then mirror every change.
  watch(
    activeSttId,
    (value) => {
      if (typeof localStorage !== "undefined") {
        try {
          localStorage.setItem(STT_STORAGE_KEY, value);
        } catch {
          // Quota or private-mode — session-only persistence is acceptable.
        }
      }
      void invoke("set_active_stt", { modelId: value }).catch(() => {
        console.error("[murmur] speech model activation failed");
      });
    },
    { immediate: true },
  );
}

export function useActiveModels() {
  init();
  return { activeSttId };
}
