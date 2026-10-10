import { listen } from "@tauri-apps/api/event";

import { useInputDevices } from "@/composables/useInputDevices";
import { useOutputDevices } from "@/composables/useOutputDevices";

/**
 * Mirror native menu-bar device choices into the frontend's persisted choices.
 * The status item itself is installed by Rust at startup, independently of the
 * webview and of any legacy "hide menu bar icon" localStorage preference.
 */
let initialized = false;

export function useMenuBarDevices(): void {
  if (initialized) return;
  initialized = true;

  const { selected: selectedInputDevice } = useInputDevices();
  const { selected: selectedOutputDevice } = useOutputDevices();

  void listen<string | null>(
    "murmur://tray-microphone-selected",
    (event) => {
      selectedInputDevice.value = event.payload;
    },
  ).catch((err) => {
    console.error("[murmur] tray microphone listener failed", err);
  });
  void listen<string | null>(
    "murmur://tray-output-selected",
    (event) => {
      selectedOutputDevice.value = event.payload;
    },
  ).catch((err) => {
    console.error("[murmur] tray output listener failed", err);
  });
}
