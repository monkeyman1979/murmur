<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { Pause, Play, Square, X } from "lucide-vue-next";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { useTheme } from "@/composables/useTheme";
import { IconTooltip } from "@/components/ui/tooltip";
import { formatMeetingDuration } from "@/lib/format";
import { useI18n } from "vue-i18n";

useTheme();
const { t } = useI18n();

const {
  activeCapture,
  activeDurationMs,
  action,
  controls,
  recordingIssue,
  hydrate,
  pause,
  resume,
  stop,
  cancel,
} = useMeetingCapture({ enableMainWindowFlows: false });

const cleanupWarning = ref(false);
const confirmingCancel = ref(false);
const timer = computed(() => formatMeetingDuration(activeDurationMs.value));
const paused = computed(() => activeCapture.value?.status === "paused");

// Phase C: the title line and the permanent status subtext are gone. The title
// said "Untitled Meeting" for every capture and now says the same timestamp the
// library already shows; the status line said "Meeting recording in progress"
// while a pulsing red dot and a running timer said it better in no space at all.
//
// Exceptional states must be explicit. A lone amber icon beside a running
// timer made users doubt whether any audio was being saved. When health cannot
// be confirmed, replace the timer and pause control with short issue text while
// retaining Stop and Discard as safe exits; the full explanation remains in
// the tooltip and accessible label.
const issue = computed(() => {
  if (cleanupWarning.value) {
    return {
      severity: "error" as const,
      label: t("meetingController.cleanupLabel"),
      detail: t("meetingController.cleanupDetail"),
    };
  }
  return recordingIssue.value;
});

const statusLabel = computed(() => t(paused.value ? "meetingController.pausedStatus" : "meetingController.recordingStatus"));
const busy = computed(() => action.value !== null);

// The controller is pre-painted offscreen at application startup, so a CSS
// animation attached at component mount would finish before the user ever sees
// it. Arm the highlight when a capture first reaches `recording` instead. Keep
// the capture ID so pause/resume does not replay a "new recording" cue.
const startHighlight = ref(false);
let highlightedCaptureId: number | null = null;
let highlightTimer: ReturnType<typeof setTimeout> | null = null;

watch(
  () => [activeCapture.value?.id ?? null, activeCapture.value?.status ?? null] as const,
  ([captureId, status]) => {
    if (!captureId || status !== "recording" || captureId === highlightedCaptureId) return;

    highlightedCaptureId = captureId;
    startHighlight.value = true;
    if (highlightTimer) clearTimeout(highlightTimer);
    highlightTimer = setTimeout(() => {
      startHighlight.value = false;
      highlightTimer = null;
    }, 2_300);
  },
  { immediate: true },
);

watch(
  () => activeCapture.value?.id ?? null,
  () => {
    confirmingCancel.value = false;
    cleanupWarning.value = false;
  },
);

onUnmounted(() => {
  if (highlightTimer) clearTimeout(highlightTimer);
});

function requestCancel(): void {
  // Native cancel(false) can discard captures under 10 seconds immediately.
  // The floating X always asks first, including for short recordings.
  confirmingCancel.value = true;
}

async function confirmCancel(): Promise<void> {
  const result = await cancel(true);
  if (result?.status === "cleanup_failed") cleanupWarning.value = true;
  if (result) confirmingCancel.value = false;
}

void hydrate();
</script>

<template>
  <!-- Transparent window, pill inside — same pattern as the dictation
       indicator. A rounded shape on an opaque window shows the window's square
       corners, which is what made the old controller read as a panel.
       The inline background is belt-and-suspenders against body-background
       cascade leakage; the <style> block at the bottom is what actually
       carries it. -->
  <div
    class="flex h-screen w-screen items-center justify-end bg-transparent"
    style="background: transparent !important"
    data-tauri-drag-region
  >
    <div
      class="meeting-controller-pill pointer-events-auto flex items-center gap-1.5 rounded-full border border-white/10 bg-zinc-950/95 py-1.5 text-white shadow-lg select-none"
      :class="[confirmingCancel ? 'px-3' : 'px-1.5', { 'meeting-controller-pill--starting': startHighlight }]"
      data-tauri-drag-region
    >
      <!-- Separated from the pause/stop controls; never discards without confirmation.
           The neutral tonal surface keeps this X legible without implying that
           it closes the window while recording continues. -->
      <IconTooltip v-if="!confirmingCancel" :label="t('meetingController.discardTooltip')">
        <button
          type="button"
          class="inline-flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-full bg-white/5 text-white/70 hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/60 disabled:opacity-40"
          :aria-label="t('meetingController.discardLabel')"
          :disabled="busy || !controls.includes('cancel')"
          @click="requestCancel"
        ><X class="size-3.5" aria-hidden="true" /></button>
      </IconTooltip>

      <span
        v-if="!confirmingCancel"
        class="size-2 shrink-0 rounded-full"
        :class="[
          'bg-red-500',
          paused || issue ? 'opacity-70' : 'animate-pulse',
        ]"
        data-tauri-drag-region
        aria-hidden="true"
      />

      <time
        v-if="!confirmingCancel && !issue"
        class="shrink-0 font-mono text-[13px] font-semibold leading-none tabular-nums"
        :datetime="`PT${Math.floor(activeDurationMs / 1000)}S`"
        :aria-label="t('meetingController.activeTime', { status: statusLabel, time: timer })"
        data-tauri-drag-region
      >{{ timer }}</time>

      <IconTooltip v-if="!confirmingCancel && issue" :label="issue.detail">
        <span
          class="max-w-24 shrink-0 truncate text-xs font-semibold"
          :class="issue.severity === 'error' ? 'text-amber-300' : 'text-amber-200'"
          :aria-label="`${issue.label}. ${issue.detail}`"
          role="alert"
          data-tauri-drag-region
        >{{ issue.label }}</span>
      </IconTooltip>

      <template v-if="confirmingCancel">
        <span class="shrink-0 whitespace-nowrap text-xs font-medium" data-tauri-drag-region>
          {{ t('meetingController.confirmQuestion') }}
        </span>
        <div class="flex shrink-0 items-center gap-1">
          <button
            type="button"
            class="inline-flex h-7 cursor-pointer items-center justify-center rounded-full px-2 text-xs font-medium text-white/85 hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/60 disabled:opacity-40"
            :aria-label="t('meetingController.keepLabel')"
            :disabled="busy"
            @click="confirmingCancel = false"
          >{{ t('meetingController.keep') }}</button>
          <button
            type="button"
            class="inline-flex h-7 cursor-pointer items-center justify-center rounded-full bg-red-500/25 px-2 text-xs font-medium text-red-300 hover:bg-red-500/40 hover:text-red-200 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-red-400/60 disabled:opacity-40"
            :aria-label="t('meetingController.confirmDiscardLabel')"
            :disabled="busy"
            @click="confirmCancel"
          >{{ t('meetingController.discard') }}</button>
        </div>
      </template>

      <template v-else>
        <div class="ml-1 flex shrink-0 items-center gap-0.5">
        <IconTooltip v-if="!issue && controls.includes('resume')" :label="t('meetingController.resumeTooltip')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-full text-white/90 hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/60 disabled:opacity-40"
            :aria-label="t('meetingController.resumeLabel')"
            :disabled="busy"
            @click="resume"
          ><Play class="size-3.5 fill-current" aria-hidden="true" /></button>
        </IconTooltip>
        <IconTooltip v-else-if="!issue" :label="t('meetingController.pauseTooltip')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-full text-white/90 hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/60 disabled:opacity-40"
            :aria-label="t('meetingController.pauseLabel')"
            :disabled="busy || !controls.includes('pause')"
            @click="pause"
          ><Pause class="size-3.5 fill-current" aria-hidden="true" /></button>
        </IconTooltip>

        <!-- Stop is the primary action and gets the only filled treatment, so
             it never reads as a peer of the discard button beside it. -->
        <IconTooltip :label="t('meetingController.stopTooltip')">
          <button
            type="button"
            class="inline-flex h-7 cursor-pointer items-center justify-center gap-1.5 rounded-full bg-white/15 px-2 text-white hover:bg-white/25 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-white/60 disabled:opacity-40"
            :aria-label="t('meetingController.stopLabel')"
            :disabled="busy || !controls.includes('stop')"
            @click="stop"
          >
            <Square class="size-3 fill-current" aria-hidden="true" />
            <span class="text-xs font-semibold">{{ t('meetingController.stop') }}</span>
          </button>
        </IconTooltip>

        </div>
      </template>
    </div>
  </div>
</template>

<style>
@property --meeting-controller-border-angle {
  syntax: "<angle>";
  initial-value: 0deg;
  inherits: false;
}

/* A one-shot arrival cue makes the controller easier to find without leaving
   a distracting animation running for the duration of the meeting. The
   pseudo-element is masked down to a 2px border, so it is easy to spot without
   washing over the timer or controls. */
.meeting-controller-pill {
  position: relative;
  isolation: isolate;
}

.meeting-controller-pill::before {
  content: "";
  position: absolute;
  inset: -2px;
  border-radius: inherit;
  padding: 2px;
  background: conic-gradient(
    from var(--meeting-controller-border-angle),
    transparent 0deg,
    transparent 205deg,
    rgb(251 113 133 / 25%) 238deg,
    rgb(251 113 133) 275deg,
    rgb(251 191 36) 310deg,
    rgb(255 255 255 / 85%) 338deg,
    transparent 360deg
  );
  opacity: 0;
  pointer-events: none;
  -webkit-mask:
    linear-gradient(#000 0 0) content-box,
    linear-gradient(#000 0 0);
  -webkit-mask-composite: xor;
  mask-composite: exclude;
}

.meeting-controller-pill--starting {
  animation: meeting-controller-arrival 900ms cubic-bezier(0.2, 0.8, 0.2, 1) both;
}

.meeting-controller-pill--starting::before {
  animation:
    meeting-controller-border-turns 2.2s linear both,
    meeting-controller-border-visibility 2.2s linear both;
}

@keyframes meeting-controller-arrival {
  0% {
    opacity: 0.72;
    transform: scale(0.97);
  }
  35% {
    opacity: 1;
    transform: scale(1.012);
  }
  100% {
    opacity: 1;
    transform: scale(1);
  }
}

@keyframes meeting-controller-border-turns {
  0% {
    --meeting-controller-border-angle: 0deg;
  }
  50% {
    --meeting-controller-border-angle: 360deg;
  }
  100% {
    --meeting-controller-border-angle: 720deg;
  }
}

@keyframes meeting-controller-border-visibility {
  0% {
    opacity: 0;
  }
  6% {
    opacity: 1;
  }
  89% {
    opacity: 1;
  }
  100% {
    opacity: 0;
  }
}

@keyframes meeting-controller-border-fade {
  0% {
    opacity: 1;
  }
  100% {
    opacity: 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .meeting-controller-pill--starting {
    animation: none;
  }

  .meeting-controller-pill--starting::before {
    --meeting-controller-border-angle: 300deg;
    animation: meeting-controller-border-fade 700ms ease-out both;
  }
}

/* Global overrides for the meeting-controller window only — this <style> block
   is non-scoped, but MeetingController.vue is code-split and only loads when
   ?window=meeting-controller, so these rules never reach the main window.

   Without them, `body { @apply bg-background }` from style.css paints an opaque
   rectangle behind the pill and defeats the window's transparency — which is
   exactly what shipped in the first cut of this control, and what the owner saw
   as "a dark black squared out border" with "a lot more space around it".
   `Indicator.vue` has carried the same block since it was written; this window
   simply never got one. */
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
