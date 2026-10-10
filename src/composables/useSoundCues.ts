import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

const STORAGE_KEY = "murmur:sound-cues-enabled";

/**
 * Reactive control for the Settings "Sound cues" toggle.
 *
 * Plays three short macOS system chimes — `Pop` on recording start,
 * `Bottle` when transcription and corrections complete before insertion,
 * and `Basso` on cancel / empty capture / Whisper error / paste error.
 * Bottle does not claim paste acceptance, so a later insertion failure can
 * produce Bottle followed by Basso.
 *
 * Defaults to **on** for new users. The cues are brief, don't clash
 * with common notification sounds (avoiding `Glass` / `Funk` /
 * `Sosumi` on purpose), and respect system volume — so the "enabled"
 * default is safe. Persisted to localStorage so the choice survives
 * relaunches; mirrored into Rust's `SoundCuesState` atomic on every
 * change + the first watcher run so the hotkey pipeline can read the
 * flag without an IPC round-trip on every press.
 */
function loadStored(): boolean {
  if (typeof localStorage === "undefined") return true;
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === null) return true; // Default ON for first launch.
  return stored === "true";
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
    void invoke("set_sound_cues", { enabled: value }).catch((err) => {
      console.error("[murmur] set_sound_cues failed", err);
    });
  },
  { immediate: true },
);

export function useSoundCues() {
  return { enabled };
}
