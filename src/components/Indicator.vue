<script setup lang="ts">
import { computed, ref, onMounted, onUnmounted } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const { t } = useI18n();

// No imports from the main app tree — this component is intentionally isolated.
// The window being visible IS the recording state; Rust controls show/hide via
// `show_indicator` / `hide_indicator` in `src-tauri/src/lib.rs`. The state event
// below drives pill *content* only; window visibility stays Rust-side so a
// dropped event can never leave a ghost pill visible.

// Current pipeline state, fed by `murmur://indicator-state` events from Rust.
// Default `recording` covers the one race where the window mounts faster than
// the first state event — the window is only ever shown on hotkey-press, so
// "recording" is the correct opening label.
type IndicatorState =
  | "recording"
  | "transcribing"
  | "cancelled"
  | "microphone-error"
  | "model-missing"
  | "recording-limit"
  | "no-audio"
  | "no-speech"
  | "idle";
const state = ref<IndicatorState>("recording");
const deviceName = ref("");

const noResultLabel = computed(() => {
  if (state.value === "no-audio") {
    return deviceName.value
      ? t("indicator.noAudio", { device: deviceName.value })
      : t("indicator.noAudioGeneric");
  }
  if (state.value === "no-speech") {
    return deviceName.value
      ? t("indicator.noSpeech", { device: deviceName.value })
      : t("indicator.noSpeechGeneric");
  }
  return "";
});

// Live audio level in [0, 1], driven by `murmur://level` events that the
// audio thread emits at ~30 Hz while recording. On Cmd::Stop/Cancel the Rust
// side snaps this back to 0 so the next capture starts from rest. Only
// consumed in the `recording` visual; other states swap to a spinner / icon.
const level = ref(0);

let unlistenLevel: UnlistenFn | null = null;
let unlistenState: UnlistenFn | null = null;
let unlistenDevice: UnlistenFn | null = null;

onMounted(async () => {
  unlistenLevel = await listen<number>("murmur://level", (e) => {
    level.value = typeof e.payload === "number" ? e.payload : 0;
  });
  unlistenDevice = await listen<string>("murmur://indicator-device", (e) => {
    deviceName.value = typeof e.payload === "string" ? e.payload : "";
  });
  unlistenState = await listen<IndicatorState>(
    "murmur://indicator-state",
    (e) => {
      if (
        e.payload === "recording" ||
        e.payload === "transcribing" ||
        e.payload === "cancelled" ||
        e.payload === "microphone-error" ||
        e.payload === "model-missing" ||
        e.payload === "recording-limit" ||
        e.payload === "no-audio" ||
        e.payload === "no-speech" ||
        e.payload === "idle"
      ) {
        state.value = e.payload;
        if (e.payload === "recording") deviceName.value = "";
      }
    },
  );
});
onUnmounted(() => {
  unlistenLevel?.();
  unlistenState?.();
  unlistenDevice?.();
  unlistenLevel = null;
  unlistenState = null;
  unlistenDevice = null;
});
</script>

<template>
  <!-- Outer fills the full window (360×40 per tauri.conf); transparent.
       Centers the content-sized pill inside. The inline transparent
       background is belt-and-suspenders against any body-background
       cascade leakage — production transparency is carried by the
       <style> block below plus the app-level `macOSPrivateApi` flag. -->
  <div
    class="flex h-screen w-screen items-center justify-center bg-transparent"
    style="background: transparent !important"
  >
    <div
      role="status"
      aria-live="polite"
      aria-atomic="true"
      class="flex max-w-full items-center gap-2 rounded-full bg-zinc-950 px-4 py-2 select-none"
      style="-webkit-app-region: drag; cursor: default"
    >
      <!-- Recording: red dot scaled by live mic level. 75 ms transition
           smooths the 30 Hz event cadence into continuous motion. -->
      <template v-if="state === 'recording'">
        <span
          class="size-2 shrink-0 rounded-full bg-red-500 transition-transform duration-75"
          :style="{ transform: `scale(${1 + level * 0.6})` }"
          aria-hidden="true"
        />
        <span class="text-sm font-medium text-white leading-none">
          {{ t("indicator.recording") }}
        </span>
      </template>

      <!-- Transcribing: CSS-only spinner so we don't pull an icon library
           into this intentionally-isolated component. The
           `border-t-transparent` gap + `animate-spin` produces a continuous
           ring animation. -->
      <template v-else-if="state === 'transcribing'">
        <span
          class="size-3 shrink-0 rounded-full border-2 border-white/80 border-t-transparent animate-spin"
          aria-hidden="true"
        />
        <span class="text-sm font-medium text-white leading-none">
          {{ t("indicator.transcribing") }}
        </span>
      </template>

      <!-- Cancelled / error states: red × mark + label. Rust holds the
           transient error pills briefly before transitioning to idle + hiding
           the window, except recording-limit which stays until release. -->
      <template v-else-if="state === 'cancelled' || state === 'microphone-error' || state === 'model-missing' || state === 'recording-limit' || state === 'no-audio' || state === 'no-speech'">
        <span
          class="shrink-0 text-sm font-bold leading-none text-red-500"
          aria-hidden="true"
        >
          &times;
        </span>
        <span
          class="min-w-0 truncate text-sm font-medium text-white leading-none"
          :title="state === 'no-audio' || state === 'no-speech' ? noResultLabel : undefined"
        >
          {{
            state === "microphone-error"
              ? t("indicator.microphoneError")
              : state === "model-missing"
                ? t("indicator.modelMissing")
                : state === "recording-limit"
                  ? t("indicator.recordingLimit")
                  : state === "no-audio" || state === "no-speech"
                    ? noResultLabel
                    : t("indicator.cancelled")
          }}
        </span>
      </template>

      <!-- Idle: the Rust-side `hide_indicator` should have hidden the
           window by the time state transitions to idle, so this branch
           renders nothing visible. A blank pill also covers the brief
           window where an event arrives just before the hide. -->
    </div>
  </div>
</template>

<style>
/* Global overrides for the indicator window only — this <style> block is
   non-scoped, but Indicator.vue is code-split and only loads when
   ?window=indicator, so these rules never reach the main settings window.
   Without them, `body { @apply bg-background }` from style.css paints an
   opaque rectangle behind the pill and defeats the window's transparency. */
html,
body,
#app {
  background: transparent !important;
  background-color: transparent !important;
  margin: 0 !important;
  padding: 0 !important;
  overflow: hidden !important;
}
</style>
