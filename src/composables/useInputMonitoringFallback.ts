import { computed, onBeforeUnmount, ref, watch, type Ref } from "vue";
import type { PermissionState } from "@/composables/useMurmurEvents";

// On macOS 27 an Accessibility grant also authorizes the shortcut's
// listen-only event tap, and an Input Monitoring request made while
// Accessibility is off is answered silently (see permissions.rs). So the
// Input Monitoring row is only a fallback: shown if listening is still
// missing a few seconds after Accessibility is granted, which is long
// enough for the hotkey tap to start and the 2 s watcher to report it.
export const INPUT_FALLBACK_DELAY_MS = 6000;

export function useInputMonitoringFallback(
  accessState: Ref<PermissionState | null>,
  inputState: Ref<PermissionState | null>,
) {
  const inputFallbackDue = ref(false);
  let inputFallbackTimer: ReturnType<typeof setTimeout> | undefined;
  watch(accessState, (state) => {
    clearTimeout(inputFallbackTimer);
    inputFallbackDue.value = false;
    if (state === "granted") {
      inputFallbackTimer = setTimeout(() => { inputFallbackDue.value = true; }, INPUT_FALLBACK_DELAY_MS);
    }
  }, { immediate: true });
  onBeforeUnmount(() => clearTimeout(inputFallbackTimer));

  return computed(() =>
    inputFallbackDue.value
    && accessState.value === "granted"
    && (inputState.value === "denied" || inputState.value === "not-determined"),
  );
}
