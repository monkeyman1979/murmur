import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

const STORAGE_KEY = "murmur:mute-while-dictating";

/**
 * Reactive control for the Settings "Mute system audio while
 * dictating" toggle.
 *
 * When ON: Murmur saves the current macOS output volume at hotkey
 * press, sets output to 0 for the duration of the dictation, and
 * restores on every exit path (successful paste, Esc, empty capture,
 * Whisper error, paste error). Our own chimes keep playing because
 * `NSSound::soundNamed` routes through the separate Alert volume
 * channel — a quirk of the macOS audio architecture we rely on
 * deliberately.
 *
 * Defaults to **off** — it's an intervention in the user's audio
 * environment, should be explicitly opt-in. Persisted to localStorage
 * so the choice survives relaunches; mirrored into Rust's
 * `MuteWhileDictatingState` atomic on every change + the first
 * watcher run so the hotkey pipeline can read it without an IPC
 * round-trip on every press.
 *
 * Crash-safe: the Rust side writes a sentinel file before muting and
 * deletes it after restoring. On app launch, `recover_on_startup`
 * checks for a stale sentinel (prior session died mid-dictation) and
 * restores the saved volume so the user doesn't find their system
 * stuck silent.
 */
function loadStored(): boolean {
  if (typeof localStorage === "undefined") return false;
  return localStorage.getItem(STORAGE_KEY) === "true";
}

const enabled = ref<boolean>(loadStored());

// Module-scoped watcher — persists for the app's lifetime regardless
// of which component is mounted. Previously this lived inside an
// `init()` function called from the composable, which Vue tied to the
// first component's effect scope; once that component unmounted the
// watcher was torn down and subsequent toggle flips didn't propagate
// to localStorage or the Rust atomic.
watch(
  enabled,
  (value) => {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, value ? "true" : "false");
    }
    void invoke("set_mute_while_dictating", { enabled: value }).catch((err) => {
      console.error("[murmur] set_mute_while_dictating failed", err);
    });
  },
  { immediate: true },
);

export function useMuteWhileDictating() {
  return { enabled };
}
