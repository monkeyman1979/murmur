import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ref, watch } from "vue";

export interface AudioOutputDevice {
  name: string;
  is_default: boolean;
}

interface AudioOutputDevicesChanged {
  current_default: string | null;
  devices: AudioOutputDevice[];
}

const STORAGE_KEY = "murmur:preferred-output-device";

function loadStoredSelection(): string | null {
  if (typeof localStorage === "undefined") return null;
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored && stored.length > 0 ? stored : null;
}

const devices = ref<AudioOutputDevice[]>([]);
const selected = ref<string | null>(loadStoredSelection());
let initialized = false;

async function refresh(): Promise<void> {
  try {
    devices.value = await invoke<AudioOutputDevice[]>("list_output_devices");
  } catch (error) {
    console.error("[murmur] list_output_devices failed", error);
    devices.value = [];
  }
}

async function pushSelectionToRust(value: string | null): Promise<void> {
  try {
    await invoke("set_output_device", { name: value });
  } catch (error) {
    console.error("[murmur] set_output_device failed", error);
  }
}

function init(): void {
  if (initialized) return;
  initialized = true;

  watch(
    selected,
    (value) => {
      if (typeof localStorage !== "undefined") {
        if (value === null) localStorage.removeItem(STORAGE_KEY);
        else localStorage.setItem(STORAGE_KEY, value);
      }
      void pushSelectionToRust(value);
    },
    { immediate: true },
  );

  void refresh();
  void listen<AudioOutputDevicesChanged>(
    "murmur://output-devices-changed",
    (event) => {
      devices.value = event.payload.devices;
      if (
        selected.value !== null &&
        !devices.value.some((device) => device.name === selected.value)
      ) {
        selected.value = null;
      }
    },
  ).catch((error) => {
    console.error("[murmur] output device listener failed", error);
  });
}

export function useOutputDevices() {
  init();
  return { devices, selected, refresh };
}
