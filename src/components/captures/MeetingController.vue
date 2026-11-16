<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { Pause, Play, Square, X, TriangleAlert, AudioLines, ChevronUp } from "lucide-vue-next";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { useTheme } from "@/composables/useTheme";
import { useIndicatorAlignment } from "@/composables/useIndicatorAlignment";
import { IconTooltip } from "@/components/ui/tooltip";
import RecordingSurface from "@/components/RecordingSurface.vue";
import { formatMeetingDuration } from "@/lib/format";
import { useI18n } from "vue-i18n";

useTheme();
const alignment = useIndicatorAlignment();
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
// True while the notch plays its exit motion before Stop/Discard runs.
const ending = ref(false);
const busy = computed(() => action.value !== null || ending.value);
const surface = ref<InstanceType<typeof RecordingSurface> | null>(null);

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

/** Shrink the notch into the camera, then end the capture. The native side
 * hides the window when the capture ends, which is invisible by then. If the
 * capture did not actually end, bring the panel back. */
async function endCapture<T>(run: () => Promise<T>, ended: (result: T) => boolean): Promise<T> {
  ending.value = true;
  try {
    await surface.value?.playExit();
    const result = await run();
    if (!ended(result)) surface.value?.cancelExit();
    return result;
  } finally {
    ending.value = false;
  }
}

async function confirmCancel(): Promise<void> {
  if (busy.value) return;
  const result = await endCapture(() => cancel(true), result => result?.status === "cancelled");
  if (result?.status === "cleanup_failed") cleanupWarning.value = true;
  if (result) confirmingCancel.value = false;
}

async function finish(): Promise<void> {
  if (busy.value) return;
  await endCapture(stop, result => result !== null);
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
    class="flex h-screen w-screen items-center bg-transparent"
    :class="alignment"
    style="background: transparent !important"
  >
    <RecordingSurface ref="surface" sync-collapse-paint hover-cue :status-label="issue?.detail ?? statusLabel" @expanded-change="open => { if (!open) confirmingCancel = false; }">
      <template #leading>
        <span class="size-1.5 rounded-full" :class="issue || paused ? 'bg-amber-400' : 'bg-red-500'" />
        <Pause v-if="paused" class="size-3.5 text-white/80" />
        <AudioLines v-else class="size-3.5 text-white/80" />
      </template>
      <template #trailing>
        <TriangleAlert v-if="issue" class="size-4 text-amber-400" />
        <time v-else class="meeting-notch-timer font-mono text-[11px] tabular-nums text-white/90" :datetime="`PT${Math.floor(activeDurationMs / 1000)}S`">{{ timer }}</time>
      </template>
      <template #compact>
        <span v-if="issue" role="alert">{{ issue.label }}</span>
        <span v-else>{{ statusLabel }} {{ timer }}</span>
      </template>
      <template #expanded="{ collapse }">
        <div class="meeting-notch-controls flex w-full flex-col gap-2">
          <div class="flex min-w-0 items-center justify-between gap-4">
            <div class="flex min-w-0 flex-col gap-1">
              <p class="truncate text-sm font-semibold text-white">{{ t('meetingController.meeting') }}</p>
              <p v-if="issue" class="truncate text-xs leading-snug text-amber-300" role="alert" :title="issue.detail">{{ issue.label }}</p>
              <p v-else class="text-xs text-white/50">{{ t(paused ? 'meetingController.paused' : 'meetingController.recording') }}</p>
            </div>
            <button type="button" class="inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-white/5 text-white/60 transition-colors hover:bg-white/10 hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-white/70" :aria-label="t('indicatorPosition.collapse')" @click="collapse">
              <ChevronUp class="size-3.5" aria-hidden="true" />
            </button>
          </div>

          <!-- Both states share one grid cell and an identical 24px text row, so
               switching to the discard confirmation crossfades in place instead
               of swapping instantly and nudging the buttons by a few pixels. -->
          <div class="meeting-notch-swap">
            <Transition name="meeting-notch-swap">
              <div v-if="confirmingCancel" key="confirm" class="flex flex-col gap-2">
                <p class="flex h-6 items-center text-xs text-white/75">{{ t('meetingController.confirmQuestion') }}</p>
                <div class="grid grid-cols-2 gap-2">
                  <button type="button" class="meeting-notch-action bg-white/10 text-white" :disabled="busy" :aria-label="t('meetingController.keepLabel')" @click="confirmingCancel = false">{{ t('meetingController.keep') }}</button>
                  <button type="button" class="meeting-notch-action bg-red-500/20 text-red-300" :disabled="busy" :aria-label="t('meetingController.confirmDiscardLabel')" @click="confirmCancel">{{ t('meetingController.discard') }}</button>
                </div>
              </div>
              <div v-else key="actions" class="flex flex-col gap-2">
                <button type="button" class="inline-flex h-6 self-start items-center rounded text-[11px] text-white/55 underline-offset-4 transition-colors hover:text-red-300 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-white/70 disabled:opacity-40" :disabled="busy || !controls.includes('cancel')" :aria-label="t('meetingController.discardLabel')" @click="requestCancel">
                  {{ t('meetingController.discardAction') }}
                </button>
                <div class="grid grid-cols-2 gap-2">
                  <button v-if="!issue && controls.includes('resume')" type="button" class="meeting-notch-action bg-white/10 text-white/90" :disabled="busy" :aria-label="t('meetingController.resumeLabel')" @click="resume">
                    <Play class="size-3.5 fill-current" aria-hidden="true" />{{ t('meetingController.resume') }}
                  </button>
                  <button v-else type="button" class="meeting-notch-action bg-white/10 text-white/90" :disabled="busy || !!issue || !controls.includes('pause')" :aria-label="t('meetingController.pauseLabel')" @click="pause">
                    <Pause class="size-3.5" aria-hidden="true" />{{ t('meetingController.pause') }}
                  </button>
                  <button type="button" class="meeting-notch-action bg-white text-zinc-950" :disabled="busy || !controls.includes('stop')" :aria-label="t('meetingController.stopLabel')" @click="finish">
                    <Square class="size-3 fill-current" aria-hidden="true" />{{ t('meetingController.stop') }}
                  </button>
                </div>
              </div>
            </Transition>
          </div>
        </div>
      </template>
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
    </RecordingSurface>
  </div>
</template>

<style>
.meeting-notch-action {
  display: inline-flex;
  height: 36px;
  align-items: center;
  justify-content: center;
  gap: 7px;
  border-radius: 11px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: filter 140ms ease, transform 140ms ease;
}
.meeting-notch-action:hover { filter: brightness(.9); }
.meeting-notch-action:active { transform: scale(.98); }
.meeting-notch-action:focus-visible { outline: 2px solid #ffffffa0; outline-offset: 2px; }
.meeting-notch-action:disabled { opacity: .4; cursor: default; }
.meeting-notch-swap { display: grid; }
.meeting-notch-swap > * { grid-area: 1 / 1; }
.meeting-notch-swap-enter-active { transition: opacity 180ms ease 40ms, transform 220ms cubic-bezier(.22, 1, .36, 1) 40ms; }
.meeting-notch-swap-leave-active { transition: opacity 120ms ease, transform 120ms ease; pointer-events: none; }
.meeting-notch-swap-enter-from { opacity: 0; transform: translateY(3px); }
.meeting-notch-swap-leave-to { opacity: 0; transform: translateY(-3px); }
@media (prefers-reduced-motion: reduce) {
  .meeting-notch-swap-enter-active, .meeting-notch-swap-leave-active { transition: opacity 120ms ease; }
  .meeting-notch-swap-enter-from, .meeting-notch-swap-leave-to { transform: none; }
  .meeting-notch-action { transition: none; }
  .meeting-notch-action:active { transform: none; }
}

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
