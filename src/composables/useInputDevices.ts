import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ref, watch } from "vue";
import { i18n } from "@/i18n";

export interface AudioInputDevice {
  name: string;
  is_default: boolean;
}

interface AudioInputDevicesChanged {
  current_default: string | null;
  devices: AudioInputDevice[];
}

const STORAGE_KEY = "murmur:preferred-input-device";

/**
 * Reactive mic picker state.
 *
 * - `devices` is the current list from cpal. Rust also pushes
 *   `murmur://input-devices-changed` when CoreAudio reports a default
 *   input change or device add/remove.
 * - `selected` is either `null` (use system default) or the cpal
 *   `name` string of a specific device. Persisted to localStorage
 *   across sessions; on boot we push the saved value into Rust so
 *   captures pick it up immediately.
 */
function loadStoredSelection(): string | null {
  if (typeof localStorage === "undefined") return null;
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored && stored.length > 0 ? stored : null;
}

export type InputDevicesState = "loading" | "ready" | "empty" | "error";
const state = ref<InputDevicesState>("loading");
const error = ref<string | null>(null);
let enumeration = 0;
let selectionQueue: Promise<void> = Promise.resolve();
const devices = ref<AudioInputDevice[]>([]);
const selected = ref<string | null>(loadStoredSelection());

let initialized = false;

function acceptDevices(value: AudioInputDevice[]) {
  devices.value = value;
  state.value = value.length ? "ready" : "empty";
  error.value = null;
  if (selected.value !== null && !value.some((device) => device.name === selected.value)) selected.value = null;
}

async function refresh() {
  const request = ++enumeration;
  state.value = "loading";
  error.value = null;
  try {
    const value = await invoke<AudioInputDevice[]>("list_input_devices");
    if (request === enumeration) acceptDevices(value);
  } catch {
    if (request !== enumeration) return;
    state.value = "error";
    error.value = i18n.global.t('runtimeErrors.microphoneList');
  }
}

function pushSelectionToRust(value: string | null): Promise<void> {
  const request = selectionQueue.then(() => invoke<void>("set_input_device", { name: value }));
  selectionQueue = request.catch(() => {});
  return request;
}

function init() {
  if (initialized) return;
  initialized = true;

  // Persist and mirror to Rust on every change. On boot, this also
  // hydrates Rust with whatever was in localStorage so the user's
  // previous choice takes hold before they touch anything.
  watch(
    selected,
    (value) => {
      if (typeof localStorage !== "undefined") {
        if (value === null) {
          localStorage.removeItem(STORAGE_KEY);
        } else {
          localStorage.setItem(STORAGE_KEY, value);
        }
      }
      void pushSelectionToRust(value).catch(() => {
        error.value = i18n.global.t('runtimeErrors.microphoneSelection');
      });
    },
    { immediate: true },
  );

  void refresh();

  void listen<AudioInputDevicesChanged>(
    "murmur://input-devices-changed",
    (event) => {
      ++enumeration;
      acceptDevices(event.payload.devices);
    },
  ).catch((err) => {
    console.error("[murmur] input device listener failed", err);
  });
}

export function useInputDevices() {
  init();
  return {
    devices,
    state,
    error,
    syncSelection: () => pushSelectionToRust(selected.value),
    selected,
    refresh,
  };
}
