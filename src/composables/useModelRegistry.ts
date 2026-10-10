import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ref } from "vue";
import type { ModelInfo } from "@/composables/useModelDownload";

export interface ModelEntryDto {
  id: string;
  display_name: string;
  hf_repo_path: string;
  filename: string;
  approx_bytes: number;
  sha256: string | null;
}

export interface ModelListItem {
  entry: ModelEntryDto;
  info: ModelInfo;
}

interface DownloadDoneEvent {
  model_id: string;
  downloaded: number;
  total: number | null;
  done: boolean;
}

const models = ref<ModelListItem[]>([]);
let initialized = false;
let downloadListenerStarted = false;

async function refresh(): Promise<void> {
  try {
    models.value = await invoke<ModelListItem[]>("list_models");
  } catch {
    console.error("[murmur] model registry refresh failed");
  }
}

async function ensureDownloadListener(): Promise<void> {
  if (downloadListenerStarted) return;
  downloadListenerStarted = true;
  // Re-run refresh whenever a download completes so the row flips
  // from missing → ready without requiring a manual page reload.
  await listen<DownloadDoneEvent>("murmur://model-download", (event) => {
    if (event.payload.done) {
      void refresh();
    }
  });
}

function init(): void {
  if (initialized) return;
  initialized = true;

  void refresh();
  void ensureDownloadListener();
}

export function useModelRegistry() {
  init();

  return { models, refresh };
}
