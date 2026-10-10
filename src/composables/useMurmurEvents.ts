import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ref } from "vue";

export interface FocusedApp {
  name: string | null;
  bundle_id: string | null;
}

export type PermissionState =
  | "granted"
  | "denied"
  | "not-determined"
  | "unknown";

export interface PermissionSnapshot {
  microphone: PermissionState;
  input_monitoring: PermissionState;
  accessibility: PermissionState;
}

export interface Transcript {
  attempt_id?: number;
  text: string;
  language: string | null;
  duration_ms: number;
  confidence: number | null;
  app?: FocusedApp | null;
}

// Locally-enriched transcript. `created_at` is a frontend-side
// `Date.now()` captured when the `murmur://transcript` event fires —
// good enough for session-local history. Once M4's SQLite store lands,
// the Rust side will own the authoritative timestamp and this will
// migrate to a field on the wire type.
export interface RecordedTranscript extends Transcript {
  created_at: number;
}

const HISTORY_CAP = 20;

const isRecording = ref<boolean>(false);
const isProcessingDictation = ref<boolean>(false);
const latestTranscript = ref<RecordedTranscript | null>(null);
const history = ref<RecordedTranscript[]>([]);
// `null` = haven't heard from the backend yet. Separate from a fully
// granted snapshot so the banner doesn't flash on first paint before
// the watcher's first emit lands.
const permissions = ref<PermissionSnapshot | null>(null);

let initialized = false;

function init() {
  if (initialized) return;
  initialized = true;

  // Fire-and-forget: the listeners live for the app's lifetime.
  // A single-window Tauri app has no meaningful teardown point where
  // unlistening would be useful, so we intentionally omit cleanup.
  listen<boolean>("murmur://recording", (event) => {
    isRecording.value = event.payload;
  }).catch(() => console.error("[murmur] recording listener failed"));

  listen<boolean>("murmur://dictation-processing", (event) => {
    isProcessingDictation.value = event.payload;
  }).catch(() => console.error("[murmur] dictation processing listener failed"));

  listen<Transcript>("murmur://transcript", (event) => {
    const record: RecordedTranscript = {
      ...event.payload,
      created_at: Date.now(),
    };
    latestTranscript.value = record;
    history.value = [record, ...history.value].slice(0, HISTORY_CAP);
  }).catch(() => console.error("[murmur] transcript listener failed"));

  listen<PermissionSnapshot>("murmur://permissions", (event) => {
    permissions.value = event.payload;
  }).catch(() => console.error("[murmur] permissions listener failed"));

  // Hydrate immediately — the `murmur://permissions` watcher only
  // emits on *change*, and its first emit can fire before our listen()
  // subscription is actually installed on the backend. Without this
  // pull, the Permissions page sits at "Checking…" forever after a
  // relaunch where state hasn't changed since boot. `get_permissions`
  // is synchronous on the Rust side (three TCC / IOHID lookups), so
  // this is cheap.
  invoke<PermissionSnapshot>("get_permissions")
    .then((snapshot) => {
      permissions.value = snapshot;
    })
    .catch((err) => {
      console.error("[murmur] get_permissions hydration failed", err);
    });
}

export function useMurmurEvents() {
  init();

  return {
    isRecording,
    isProcessingDictation,
    latestTranscript,
    history,
    permissions,
  };
}
