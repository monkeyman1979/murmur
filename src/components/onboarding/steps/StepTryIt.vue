<script setup lang="ts">
import { createOnboardingDemoSession } from "@/composables/useOnboardingDemoSession";
import { Button } from "@/components/ui/button";
import { useHotkeyBinding } from "@/composables/useHotkeyBinding";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { bindingSegments, formatBindingLabel } from "@/lib/hotkey";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { CheckCircle2, RotateCw } from "lucide-vue-next";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";

type ActivationMode = "hold" | "toggle";
type DemoPhase = ActivationMode | "complete";
const indicatorLabels = {
  recording: "indicator.recording",
  transcribing: "indicator.transcribing",
  cancelled: "indicator.cancelled",
  "microphone-error": "indicator.microphoneError",
  "model-missing": "indicator.modelMissing",
  "recording-limit": "indicator.recordingLimit",
  "no-audio": "indicator.noAudioGeneric",
  "no-speech": "indicator.noSpeechGeneric",
  idle: "onboarding.tryIt.feedback.ready",
} as const;
type IndicatorState = keyof typeof indicatorLabels;

const props = withDefaults(
  defineProps<{
    activePhase?: ActivationMode;
    holdComplete?: boolean;
    toggleComplete?: boolean;
  }>(),
  {
    activePhase: "hold",
    holdComplete: false,
    toggleComplete: false,
  },
);

const emit = defineEmits<{
  holdComplete: [];
  toggleComplete: [];
  modelMissing: [];
}>();

const { t } = useI18n();
const { permissions } = useMurmurEvents();
const { binding } = useHotkeyBinding();


const bindingSegs = computed<string[]>(() => bindingSegments(binding.value, t));
const demoText = ref<string>("");
const textareaRef = ref<HTMLTextAreaElement | null>(null);

const pressed = ref(false);
const demoError = ref<string | null>(null);
const listening = ref(false);
const indicatorState = ref<IndicatorState>("idle");
const inputLevel = ref(0);
const attempts = new Map<number, { mode: ActivationMode; phase: ActivationMode }>();
let newestAttempt = 0;
let disposed = false;
const unlisteners: UnlistenFn[] = [];
const demoSession = createOnboardingDemoSession();
let setupPending = false;

const allPermissionsGranted = computed<boolean>(() => {
  const p = permissions.value;
  return !!p && p.microphone === "granted" && p.input_monitoring === "granted" && p.accessibility === "granted";
});

const missingPermissions = computed<string[]>(() => {
  const p = permissions.value;
  if (!p) return [];
  const missing: string[] = [];
  if (p.microphone !== "granted") missing.push(t("onboarding.permissionStep.labels.mic"));
  if (p.input_monitoring !== "granted") missing.push(t("onboarding.permissionStep.labels.inputMonitoring"));
  if (p.accessibility !== "granted") missing.push(t("onboarding.permissionStep.labels.accessibility"));
  return missing;
});

const phase = computed<DemoPhase>(() => {
  if (props.holdComplete && props.toggleComplete) return "complete";
  return props.activePhase;
});

const currentPhaseComplete = computed<boolean>(() =>
  props.activePhase === "hold" ? props.holdComplete : props.toggleComplete,
);

const phaseTitle = computed<string>(() => t(`onboarding.tryIt.${props.activePhase}Title`));
const phaseBody = computed<string>(() => t(`onboarding.tryIt.${props.activePhase}Body`, { binding: formatBindingLabel(binding.value, t) }));

const demoPlaceholder = computed<string>(() => {
  if (phase.value === "hold") return t("onboarding.tryIt.holdPlaceholder");
  if (phase.value === "toggle") return t("onboarding.tryIt.togglePlaceholder");
  return t("onboarding.tryIt.placeholder");
});

const feedbackLabel = computed(() => {
  if (!listening.value) return t("onboarding.tryIt.feedback.preparing");
  return t(indicatorLabels[indicatorState.value]);
});

function resetFeedback(): void {
  indicatorState.value = "idle";
  inputLevel.value = 0;
}

function pendingAttempt(): boolean {
  return listening.value && allPermissionsGranted.value && attempts.get(newestAttempt)?.phase === props.activePhase;
}

async function relaunch(): Promise<void> {
  try {
    await invoke("restart_murmur");
  } catch (err) {
    console.error("[murmur] restart_murmur failed", err);
  }
}

function markTranscriptForMode(mode: ActivationMode | null): void {
  if (!mode || phase.value === "complete") return;

  // Only the requested gesture can complete the exercise. A different gesture
  // leaves it ready to retry without adding another instructional callout.
  if (props.activePhase !== mode) return;
  if (mode === "hold") {
    emit("holdComplete");
    textareaRef.value?.focus();
    return;
  }

  emit("toggleComplete");
}

async function subscribe<T>(name: string, callback: (payload: T) => void) {
  if (disposed) return;
  const stop = await listen<T>(name, (event) => { if (!disposed) callback(event.payload); });
  if (disposed) stop();
  else unlisteners.push(stop);
}

async function startDemo() {
  if (setupPending || disposed) return;
  setupPending = true;
  listening.value = false;
  demoError.value = null;
  attempts.clear();
  resetFeedback();
  for (const stop of unlisteners.splice(0)) stop();
  try {
    await subscribe<{ attempt_id: number; mode: ActivationMode }>("murmur://activation-mode", (payload) => {
      if (!listening.value || !Number.isSafeInteger(payload.attempt_id) || payload.attempt_id <= newestAttempt || !["hold", "toggle"].includes(payload.mode)) return;
      newestAttempt = payload.attempt_id;
      // Only the newest accepted attempt may update this phase's proof field.
      attempts.clear();
      attempts.set(payload.attempt_id, { mode: payload.mode, phase: props.activePhase });
      resetFeedback();
      demoError.value = null;
    });
    await subscribe<{ pressed: boolean }>("murmur://hotkey-binding-state", (payload) => { pressed.value = payload.pressed; });
    // These native events are session-guarded in Rust, but have no attempt ID.
    // Use them only as decoration for a pending demo attempt, never as proof.
    // Physical key-up deliberately does not change the recording state.
    await subscribe<unknown>("murmur://indicator-state", (payload) => {
      if (!pendingAttempt() || typeof payload !== "string" || !Object.prototype.hasOwnProperty.call(indicatorLabels, payload)) return;
      indicatorState.value = payload as IndicatorState;
      inputLevel.value = 0;
    });
    await subscribe<unknown>("murmur://level", (payload) => {
      if (!pendingAttempt() || indicatorState.value !== "recording") return;
      inputLevel.value = typeof payload === "number" && Number.isFinite(payload) ? Math.min(1, Math.max(0, payload)) : 0;
    });
    await subscribe<{ attempt_id?: number; text: string }>("murmur://transcript", (payload) => {
      if (payload.attempt_id === undefined) return;
      const attempt = attempts.get(payload.attempt_id);
      attempts.delete(payload.attempt_id);
      if (!attempt || payload.attempt_id !== newestAttempt || attempt.phase !== props.activePhase) return;
      resetFeedback();
      if (!allPermissionsGranted.value || currentPhaseComplete.value) return;
      const text = payload.text?.trim();
      if (!text) { demoError.value = "no-speech"; return; }
      if (attempt.mode === props.activePhase) demoText.value = text;
      markTranscriptForMode(attempt.mode);
    });
    await subscribe<{ attempt_id: number; outcome: string }>("murmur://dictation-attempt-ended", (payload) => {
      const attempt = attempts.get(payload.attempt_id);
      attempts.delete(payload.attempt_id);
      if (!attempt || payload.attempt_id !== newestAttempt || attempt.phase !== props.activePhase) return;
      resetFeedback();
      if (currentPhaseComplete.value) return;
      demoError.value = ["cancelled", "no-audio", "no-speech", "model-missing"].includes(payload.outcome) ? payload.outcome : "pipeline-error";
      if (payload.outcome === "model-missing") emit("modelMissing");
    });
    if (disposed) return;
    // Install every listener before enabling native demo suppression. Rust
    // snapshots demo mode and auto language per attempt; saved language stays intact.
    await demoSession.activate();
    if (!disposed) {
      listening.value = true;
      // Focus after Vue has made the input editable, not before that DOM patch.
      await nextTick();
      if (!disposed) textareaRef.value?.focus();
    }
  } catch {
    for (const stop of unlisteners.splice(0)) stop();
    pressed.value = false;
    attempts.clear();
    resetFeedback();
    await demoSession.deactivate().catch(() => {});
    if (!disposed) demoError.value = "setup-error";
  } finally {
    setupPending = false;
  }
}

onMounted(startDemo);

watch(
  () => props.activePhase,
  () => {
    attempts.clear();
    resetFeedback();
    demoError.value = null;
    demoText.value = "";
    textareaRef.value?.focus();
  },
  { flush: "post" },
);

onUnmounted(() => {
  disposed = true;
  listening.value = false;
  pressed.value = false;
  attempts.clear();
  resetFeedback();
  for (const stop of unlisteners.splice(0)) stop();
  void demoSession.dispose().catch(() => {});
});
</script>

<template>
  <div class="flex flex-col gap-4">
    <template v-if="!allPermissionsGranted">
      <div
        class="rounded-[2rem] border border-border bg-muted/50 p-6 text-foreground "
      >
        <p class="text-lg font-semibold">{{ t("onboarding.tryIt.gate.headline") }}</p>
        <p class="mt-3 text-sm leading-6">
          <span class="font-medium">{{ t("onboarding.tryIt.gate.missingLabel") }}</span>
          {{ missingPermissions.join(", ") }}
        </p>
        <div class="mt-5 flex items-center gap-3">
          <Button variant="outline" @click="relaunch">
            <RotateCw class="mr-1.5 size-4" aria-hidden="true" />
            {{ t("onboarding.permissionStep.relaunchButton") }}
          </Button>
          <p class="text-xs leading-5 text-foreground/80">
            {{ t("onboarding.tryIt.gate.relaunchNote") }}
          </p>
        </div>
      </div>
    </template>

    <template v-else>
      <div
        class="flex min-h-[28rem] flex-col gap-6 rounded-[2rem] border border-border bg-card p-6"
        data-testid="gesture-card"
      >
        <div class="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-x-3 gap-y-2" aria-live="polite" aria-atomic="true" data-testid="gesture-instruction">
          <h2 class="text-2xl font-semibold tracking-tight text-foreground">
            {{ phaseTitle }}
          </h2>
          <p class="inline-flex justify-self-end whitespace-nowrap rounded-full border border-input bg-muted px-3 py-1 text-xs font-medium text-foreground" data-testid="gesture-progress">
            {{ t("onboarding.tryIt.gestureStep", { current: activePhase === 'hold' ? 1 : 2, total: 2 }) }}
          </p>
        </div>

        <div class="flex flex-1 flex-col items-center justify-center gap-8 py-3" data-testid="gesture-action">
          <div class="flex w-full flex-wrap items-center justify-center gap-3" :aria-label="formatBindingLabel(binding, t)" :data-pressed="pressed">
            <template v-for="(seg, i) in bindingSegs" :key="i">
              <span v-if="i > 0" class="text-muted-foreground">+</span>
              <kbd
                class="onboarding-keycap inline-flex min-h-24 min-w-24 items-center justify-center rounded-2xl border border-border bg-muted px-6 text-3xl font-semibold tracking-tight text-foreground"
                :class="{ 'onboarding-keycap-pressed': pressed }"
              >{{ seg }}</kbd>
            </template>
          </div>
          <p id="onboarding-gesture-help" class="w-[min(100%,55ch)] text-center text-sm leading-6 text-muted-foreground">
            {{ phaseBody }}
          </p>
        </div>

        <div class="flex flex-col gap-3">
          <div class="grid h-20 shrink-0 place-items-center overflow-y-auto" data-testid="gesture-feedback" :data-state="demoError ? 'error' : indicatorState !== 'idle' ? indicatorState : currentPhaseComplete ? 'complete' : listening ? 'ready' : 'preparing'">
            <p v-if="demoError" role="status" class="text-center text-sm text-destructive">
              {{ t(`onboarding.tryIt.outcomes.${demoError}`) }}
              <Button v-if="demoError === 'setup-error'" size="sm" variant="outline" @click="startDemo">{{ t("onboarding.requiredModel.retryButton") }}</Button>
            </p>
            <div
              v-else-if="currentPhaseComplete && indicatorState === 'idle'"
              role="status"
              data-testid="gesture-success"
              data-gesture-badge
              class="inline-flex items-center gap-2 rounded-full border border-success-border bg-success-muted px-4 py-2 text-sm font-medium leading-none text-success"
            >
              <CheckCircle2 class="size-3 shrink-0" aria-hidden="true" />
              <span>{{ t("onboarding.tryIt.feedback.complete") }}</span>
            </div>
            <template v-else>
              <!-- Reuse Indicator.vue's compact dark pill/red dot/spinner design,
                   not its window-scoped component or global transparency styles. -->
              <div data-gesture-badge class="inline-flex max-w-full items-center gap-2 rounded-full border border-transparent bg-zinc-950 px-4 py-2">
                  <span v-if="indicatorState === 'recording'" class="inline-recording-dot size-2 shrink-0 rounded-full bg-red-500" :style="{ transform: `scale(${1 + inputLevel * 0.6})` }" aria-hidden="true" />
                  <span v-else-if="indicatorState === 'transcribing'" class="inline-transcribing-spinner size-3 shrink-0 rounded-full border-2 border-white/80 border-t-transparent" aria-hidden="true" />
                  <span v-else class="size-2 shrink-0 rounded-full bg-zinc-400" aria-hidden="true" />
                  <span role="status" class="text-center text-sm font-medium leading-none text-white">{{ feedbackLabel }}</span>
              </div>
            </template>
          </div>

          <textarea
            ref="textareaRef"
            v-model="demoText"
            :readonly="!listening"
            :placeholder="demoPlaceholder"
            rows="3"
            class="h-20 max-h-20 min-h-20 w-full resize-none rounded-lg border border-input bg-background px-3 py-2 text-base leading-6 text-foreground shadow-sm transition-colors placeholder:text-muted-foreground placeholder:opacity-100 focus:outline-none focus:ring-2 focus:ring-ring md:text-sm"
            :style="{ color: 'var(--foreground)', WebkitTextFillColor: 'var(--foreground)', caretColor: 'var(--foreground)' }"
            :aria-label="t('onboarding.tryIt.textareaLabel')"
            aria-describedby="onboarding-gesture-help"
          />
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.onboarding-keycap {
  box-shadow: 0 6px 0 var(--border), inset 0 1px 0 var(--muted-foreground);
  transition: transform 150ms, box-shadow 150ms;
}
.onboarding-keycap-pressed {
  transform: translateY(5px);
  box-shadow: 0 1px 0 var(--border), inset 0 0 0 2px var(--primary);
  background-image: linear-gradient(color-mix(in srgb, var(--muted) 94%, transparent), color-mix(in srgb, var(--muted) 94%, transparent)), var(--progress-fill);
}
.inline-recording-dot { transition: transform 75ms; }
.inline-transcribing-spinner { animation: inline-transcribing 1s linear infinite; }
@keyframes inline-transcribing { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) {
  .onboarding-keycap { transition: none; }
  .inline-recording-dot { transform: none !important; transition: none; }
  .inline-transcribing-spinner { animation: none !important; }
}
</style>
