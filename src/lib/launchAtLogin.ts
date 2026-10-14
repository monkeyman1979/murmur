import { invoke } from "@tauri-apps/api/core";

// Backed by an SMAppService.mainApp login item (see src-tauri/src/login_item.rs),
// so System Settings lists it as Murmur under Open at Login.
export function isEnabled(): Promise<boolean> {
  return invoke<boolean>("launch_at_login_enabled");
}

export async function enable(): Promise<void> {
  await invoke<boolean>("set_launch_at_login", { enabled: true });
}

export async function disable(): Promise<void> {
  await invoke<boolean>("set_launch_at_login", { enabled: false });
}
