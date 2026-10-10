<script setup lang="ts">
import CaptureActions from "@/components/captures/CaptureActions.vue";
import CaptureStatusBadge from "@/components/captures/CaptureStatusBadge.vue";
import MeetingTitleEditor from "@/components/captures/MeetingTitleEditor.vue";
import ProcessingProgress from "@/components/captures/ProcessingProgress.vue";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
  AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { useCaptures } from "@/composables/useCaptures";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import {
  audioRetentionLabel, captureDetailBackOrigin, captureErrorCopy, captureStateMessage,
  CAPTURE_ACTIONS, hasUsableRecoveryAudio, isCaptureProgressing, normalizeCaptureError, parseCaptureId,
  parseTranscriptBlocks, primaryCaptureAction, statePresentation,
  type CaptureAction, type CaptureDetail, type CaptureError,
} from "@/lib/captures";
import { formatBytes, formatCaptureDate, formatInteger, formatMeetingDuration, formatTranscriptTimestamp } from "@/lib/format";
import { ArrowLeft, AudioLines, ChevronDown, Pause, Play, Square } from "lucide-vue-next";
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useI18n } from "vue-i18n";

const { t, locale } = useI18n();

const route = useRoute();
const router = useRouter();
const detail = ref<CaptureDetail | null>(null);
const loading = ref(false);
const error = ref<CaptureError | null>(null);
const detailsOpen = ref(false);
const deleteDialogOpen = ref(false);
const audioDeleteDialogOpen = ref(false);
const overwriteDialogOpen = ref(false);

const captures = useCaptures();
const meeting = useMeetingCapture();
const captureId = computed(() => parseCaptureId(route.params.captureId));
const isInvalid = computed(() => captureId.value === null);
const backOrigin = computed(() => {
  void route.fullPath;
  return captureDetailBackOrigin(window.history.state?.back);
});
const backLabel = computed(() => t(backOrigin.value === "home" ? "captureDetail.backHome" : "captureDetail.backMeetings"));
const summary = computed(() => detail.value?.summary ?? null);
const transcriptBlocks = computed(() => parseTranscriptBlocks(detail.value?.transcript_text ?? null));
const usableRecoveryAudio = computed(() => summary.value
  ? hasUsableRecoveryAudio(summary.value)
  : false);
const actions = computed(() => summary.value
  ? statePresentation(summary.value.status, summary.value.markdown_status, usableRecoveryAudio.value, summary.value.error_code, summary.value.can_retry_transcription, summary.value.transcription_paused, !!meeting.activeCapture.value, summary.value.transcription_queued).actions
      .filter((action) => action !== "rename" && !(action === "keep_audio" && summary.value?.audio_kept))
  : []);
const primary = computed(() => summary.value ? primaryCaptureAction(summary.value) : null);
const noticeActions = computed<CaptureAction[]>(() => {
  if (!summary.value || !primary.value) return [];
  return summary.value.status === "finalization_failed"
    ? [primary.value, "choose_folder"]
    : [primary.value];
});
const message = computed(() => !summary.value ? null : summary.value.status === "transcribing"
  ? summary.value.transcription_paused ? t("captureMessages.paused")
    : meeting.activeCapture.value ? t("captureMessages.queued")
    : summary.value.transcription_queued ? t("captureMessages.waiting") : null
  : captureStateMessage(summary.value, t));
const stateLabel = computed(() => summary.value ? t(`captureStatuses.${statePresentation(summary.value.status, summary.value.markdown_status, summary.value.has_recovery_audio, summary.value.error_code, summary.value.can_retry_transcription, summary.value.transcription_paused, !!meeting.activeCapture.value, summary.value.transcription_queued).label}`) : "");
const showsProgress = computed(() => Boolean(summary.value?.processing) && isCaptureProgressing(summary.value!.status));
const isActive = computed(() => summary.value?.id === meeting.activeCapture.value?.id);
const activePaused = computed(() => meeting.activeCapture.value?.status === "paused");
const audioLabel = computed(() => summary.value ? audioRetentionLabel(summary.value, Date.now(), t, locale.value) : null);
const audioDetailLabel = computed(() => {
  if (!summary.value?.has_recovery_audio) return t('captureDetail.noAudio');
  if (summary.value.audio_kept || summary.value.audio_retention === "keep") {
    const size = summary.value.audio_bytes > 0 ? `${formatBytes(summary.value.audio_bytes)} · ` : "";
    return `${size}${t('captureDetail.kept')}`;
  }
  return audioLabel.value ?? t('captureDetail.retained');
});
const showAudioDetails = computed(() => usableRecoveryAudio.value);
const headerPrimary = computed<CaptureAction | null>(() => summary.value?.status === "completed" ? "copy_for_ai" : null);
const allActions = CAPTURE_ACTIONS;
const busyActions = computed(() => summary.value ? allActions.filter((action) => captures.stateFor(summary.value!.id, action).loading) : []);
const hasAction = (action: CaptureAction) => actions.value.includes(action);

async function load(): Promise<void> {
  detail.value = null;
  error.value = null;
  if (captureId.value === null) return;
  loading.value = true;
  try {
    detail.value = await captures.getDetail(captureId.value);
  } catch (cause) {
    error.value = normalizeCaptureError(cause);
  } finally { loading.value = false; }
}

onMounted(() => void load());
watch(captureId, () => void load());
watch(() => (captureId.value ? captures.details.get(captureId.value) : undefined), (updated) => {
  if (updated) detail.value = updated;
}, { deep: true });

async function applySummary(task: () => Promise<unknown>): Promise<void> {
  await task();
  if (captureId.value) await captures.refreshOne(captureId.value);
}

async function handleAction(action: CaptureAction): Promise<void> {
  if (!summary.value) return;
  const id = summary.value.id;
  if (action === "delete") { deleteDialogOpen.value = true; return; }
  if (action === "delete_audio") { audioDeleteDialogOpen.value = true; return; }
  if (action === "retry_cleanup") {
    if (await captures.retryDeleteCleanup(id)) await router.push("/captures");
    return;
  }
  if (action === "retry_transcription") return applySummary(() => captures.retryTranscription(id));
  if (action === "pause_transcription") return applySummary(() => captures.pauseTranscription(id));
  if (action === "resume_transcription") return applySummary(() => captures.resumeTranscription(id));
  if (action === "cancel_transcription") return applySummary(() => captures.cancelTranscription(id));
  if (action === "retry_finalization") return applySummary(() => captures.retryFinalization(id));
  if (action === "choose_folder") return applySummary(() => captures.chooseFolderAndRetry(id));
  if (action === "cleanup_audio") return applySummary(() => captures.retryAudioCleanup(id));
  if (action === "save_audio") { await captures.saveAudioCopy(summary.value); return; }
  if (action === "keep_audio") return applySummary(() => captures.keepAudio(id));
  if (action === "copy_transcript") { await captures.copyTranscript(id); return; }
  if (action === "copy_for_ai") { await captures.copyForAi(id); return; }
  if (action === "copy_path") { await captures.copyPath(id); return; }
  if (action === "reveal") { await captures.revealTranscript(id); return; }
  if (action === "regenerate") {
    const result = await captures.regenerate(id, false);
    if (!result && captures.stateFor(id, "regenerate").error?.code === "output_file_exists") overwriteDialogOpen.value = true;
  }
}

async function confirmDelete(): Promise<void> {
  deleteDialogOpen.value = false;
  if (!summary.value) return;
  if (await captures.deleteCapture(summary.value.id)) {
    await router.push("/captures");
    await nextTick();
    document.querySelector<HTMLElement>("[data-captures-heading]")?.focus();
  }
}
async function confirmAudioDelete(): Promise<void> {
  audioDeleteDialogOpen.value = false;
  if (summary.value) await applySummary(() => captures.deleteAudio(summary.value!.id));
}
async function confirmOverwrite(): Promise<void> {
  overwriteDialogOpen.value = false;
  if (summary.value) await applySummary(() => captures.regenerate(summary.value!.id, true));
}
function navigateBack(): void {
  if (backOrigin.value) {
    router.back();
    return;
  }
  void router.push("/captures");
}
async function pauseOrResume(): Promise<void> { await (activePaused.value ? meeting.resume() : meeting.pause()); }
async function finishMeeting(): Promise<void> { await meeting.stop(); }
</script>

<template>
  <div class="flex flex-col p-4">
    <div class="mx-auto flex w-full max-w-5xl flex-col gap-6">
      <button
        v-if="!summary"
        type="button"
        class="inline-flex w-fit cursor-pointer items-center gap-1.5 rounded-sm text-sm text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        @click="navigateBack"
      >
        <ArrowLeft class="size-4" aria-hidden="true" /> {{ backLabel }}
      </button>

      <div v-if="isInvalid" class="flex flex-col items-center gap-3 py-10 text-center"><p class="text-sm text-muted-foreground">{{ t('captureDetail.invalidLink') }}</p><Button as-child variant="outline" size="sm"><RouterLink to="/captures">{{ t('captureDetail.backMeetings') }}</RouterLink></Button></div>
      <div v-else-if="loading" class="flex flex-col gap-4" aria-busy="true"><div class="h-8 w-2/3 animate-pulse rounded bg-muted/50" /><div class="h-64 animate-pulse rounded bg-muted/50" /></div>
      <div v-else-if="error" class="flex flex-col items-center gap-3 py-10 text-center" role="alert"><p class="text-sm text-muted-foreground">{{ error.code === 'capture_not_found' ? t('captureDetail.notFound') : captureErrorCopy(error, t) }}</p><Button variant="outline" size="sm" @click="load">{{ t('meetingsPage.retry') }}</Button></div>

      <template v-else-if="summary">
        <header class="flex flex-col gap-4 rounded-xl bg-border/40 p-4">
          <button
            type="button"
            class="inline-flex w-fit cursor-pointer items-center gap-1.5 rounded-sm text-sm text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            @click="navigateBack"
          >
            <ArrowLeft class="size-4" aria-hidden="true" /> {{ backLabel }}
          </button>

          <Collapsible v-model:open="detailsOpen">
            <div class="flex flex-wrap items-start justify-between gap-3">
              <div class="min-w-0 flex-1">
                <MeetingTitleEditor
                  :capture-id="summary.id"
                  :title="summary.title"
                  variant="heading"
                  as="h1"
                />
                <div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
                  <span>{{ formatCaptureDate(summary.started_at ?? summary.created_at, locale) }}</span>
                  <span aria-hidden="true">·</span>
                  <span class="tabular-nums">{{ formatMeetingDuration(summary.active_duration_ms) }}</span>
                  <template v-if="audioLabel">
                    <span aria-hidden="true">·</span>
                    <span class="inline-flex items-center gap-1 text-foreground/75">
                      <AudioLines class="size-3.5" aria-hidden="true" />
                      {{ audioLabel }}
                    </span>
                  </template>
                  <CollapsibleTrigger as-child>
                    <Button variant="ghost" size="xs" class="h-5 gap-0.5 px-1.5 text-xs text-muted-foreground active:not-aria-[haspopup]:translate-y-0">
                      {{ t('captureDetail.details') }}
                      <ChevronDown class="size-3 transition-transform" :class="detailsOpen ? 'rotate-180' : ''" aria-hidden="true" />
                    </Button>
                  </CollapsibleTrigger>
                </div>
              </div>
              <div class="flex shrink-0 items-center gap-1">
                <CaptureStatusBadge :status="summary.status" :markdown-status="summary.markdown_status" :has-recovery-audio="summary.has_recovery_audio" :can-retry-transcription="summary.can_retry_transcription" :transcription-paused="summary.transcription_paused" :recording-active="!!meeting.activeCapture.value" :transcription-queued="summary.transcription_queued" :error-code="summary.error_code" :show-ready="false" />
                <CaptureActions
                  :actions="actions"
                  :primary="headerPrimary"
                  :primary-last="headerPrimary === 'copy_for_ai'"
                  :busy-actions="busyActions"
                  @action="handleAction"
                />
              </div>
            </div>

            <CollapsibleContent class="meeting-details">
              <dl class="mt-3 grid grid-cols-[repeat(auto-fit,minmax(min(100%,8.5rem),1fr))] gap-x-4 gap-y-3 text-xs">
                <div class="flex flex-col gap-1"><dt class="text-muted-foreground">{{ t('captureDetail.created') }}</dt><dd class="text-foreground">{{ formatCaptureDate(summary.created_at, locale) }}</dd></div>
                <div class="flex flex-col gap-1"><dt class="text-muted-foreground">{{ t('captureDetail.started') }}</dt><dd class="text-foreground">{{ summary.started_at ? formatCaptureDate(summary.started_at, locale) : t('captureDetail.notStarted') }}</dd></div>
                <div class="flex flex-col gap-1"><dt class="text-muted-foreground">{{ t('captureDetail.ended') }}</dt><dd class="text-foreground">{{ summary.ended_at ? formatCaptureDate(summary.ended_at, locale) : t('captureDetail.inProgress') }}</dd></div>
                <div class="flex flex-col gap-1"><dt class="text-muted-foreground">{{ t('captureDetail.language') }}</dt><dd class="text-foreground">{{ summary.language ?? t('captureDetail.automatic') }}</dd></div>
                <div class="flex flex-col gap-1"><dt class="text-muted-foreground">{{ t('captureDetail.words') }}</dt><dd class="text-foreground">{{ formatInteger(summary.word_count, locale) }}</dd></div>
                <div v-if="showAudioDetails" data-test="retained-audio-details" class="col-span-full grid items-end gap-3 border-t border-border/60 pt-3 [grid-template-columns:repeat(auto-fit,minmax(min(100%,18rem),1fr))]">
                  <dt class="flex min-w-0 flex-col gap-1 text-muted-foreground">
                    {{ t('captureDetail.retainedAudio') }}
                    <span class="text-foreground">{{ audioDetailLabel }}</span>
                  </dt>
                  <dd v-if="summary.has_recovery_audio" class="flex flex-wrap items-center gap-2 sm:justify-end">
                    <Button v-if="hasAction('save_audio')" variant="outline" size="xs" class="cursor-pointer" :disabled="busyActions.includes('save_audio')" @click="handleAction('save_audio')">{{ t('captureActions.save_audio') }}</Button>
                    <Button v-if="hasAction('keep_audio')" variant="outline" size="xs" class="cursor-pointer" :disabled="busyActions.includes('keep_audio')" @click="handleAction('keep_audio')">{{ t('captureActions.keep_audio') }}</Button>
                    <Button v-if="hasAction('delete_audio')" variant="ghost" size="xs" class="cursor-pointer text-destructive hover:bg-destructive/10 hover:text-destructive" :disabled="busyActions.includes('delete_audio')" @click="handleAction('delete_audio')">{{ t('captureActions.delete_audio') }}</Button>
                  </dd>
                </div>
              </dl>
            </CollapsibleContent>
          </Collapsible>

          <div v-if="isActive" class="flex flex-wrap items-center gap-2 rounded-md border bg-muted/20 p-3">
            <span class="mr-auto text-sm font-medium">{{ t(activePaused ? 'navigation.meetingPaused' : 'captureDetail.activeRecording') }} · <span class="tabular-nums">{{ formatMeetingDuration(meeting.activeDurationMs.value) }}</span></span>
            <Button variant="outline" size="sm" :disabled="meeting.action.value !== null" @click="pauseOrResume">
              <Play v-if="activePaused" class="size-3.5" aria-hidden="true" /><Pause v-else class="size-3.5" aria-hidden="true" /> {{ t(activePaused ? 'navigation.resume' : 'navigation.pause') }}
            </Button>
            <Button size="sm" :disabled="meeting.action.value !== null" @click="finishMeeting"><Square class="size-3" aria-hidden="true" /> {{ t('captureDetail.finishTranscribe') }}</Button>
          </div>
        </header>

        <section v-if="message || showsProgress" class="flex flex-col gap-3 rounded-lg border border-border bg-muted/20 p-4" aria-live="polite">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <div><h2 class="text-sm font-medium">{{ stateLabel }}</h2><p v-if="message" class="mt-1 text-xs leading-snug text-muted-foreground">{{ message }}</p></div>
            <ProcessingProgress v-if="showsProgress" :processing="summary.processing!" :status="summary.status" :transcription-paused="summary.transcription_paused" :recording-active="!!meeting.activeCapture.value" :transcription-queued="summary.transcription_queued" />
          </div>
          <CaptureActions :actions="noticeActions" :primary="primary" :busy-actions="busyActions" @action="handleAction" />
        </section>

        <section v-if="transcriptBlocks.length" aria-labelledby="transcript-heading">
          <h2 id="transcript-heading" class="sr-only">{{ t('captureDetail.transcript') }}</h2>
          <ol class="flex w-full max-w-[72ch] flex-col gap-5" :aria-label="t('captureDetail.segments')">
            <li
              v-for="(block, index) in transcriptBlocks"
              :key="`${block.timestamp}-${block.speaker}-${index}`"
              class="transcript-entry flex flex-col gap-0"
              :class="block.overlapping ? 'transcript-entry--overlapping ml-[clamp(0.75rem,4vw,3.5rem)] py-1' : ''"
            >
              <div v-if="block.timestamp || block.speaker" class="flex flex-wrap items-baseline gap-x-2 text-xs leading-5 text-muted-foreground">
                <time
                  v-if="block.timestamp"
                  :class="block.overlapping ? 'sr-only' : 'font-mono tabular-nums text-muted-foreground/80'"
                >
                  <template v-if="block.overlapping">{{ t('captureDetail.overlapping', { time: block.timestamp }) }}</template>
                  <template v-else>
                    <span aria-hidden="true">{{ formatTranscriptTimestamp(block.timestamp) }}</span>
                    <span class="sr-only">{{ block.timestamp }}</span>
                  </template>
                </time>
                <span v-if="!block.overlapping && block.timestamp && block.speaker" aria-hidden="true" class="text-muted-foreground/50">·</span>
                <span v-if="block.speaker" class="font-medium">{{ block.speaker }}</span>
              </div>
              <p class="whitespace-pre-wrap break-words text-[15px] leading-[1.65] text-foreground">{{ block.text }}</p>
            </li>
          </ol>
        </section>
        <section v-else-if="summary.status === 'completed'" class="rounded-md border border-dashed p-4 text-sm text-muted-foreground">{{ t('captureDetail.unavailable') }}</section>

      </template>
    </div>

    <AlertDialog :open="deleteDialogOpen" @update:open="deleteDialogOpen = $event"><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{{ t('meetingDialogs.deleteTitle', { title: summary?.title ?? '' }) }}</AlertDialogTitle><AlertDialogDescription>{{ t('meetingDialogs.deleteDescription') }}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel><AlertDialogAction variant="destructive" @click="confirmDelete">{{ t('meetingDialogs.deleteMeeting') }}</AlertDialogAction></AlertDialogFooter></AlertDialogContent></AlertDialog>
    <AlertDialog :open="audioDeleteDialogOpen" @update:open="audioDeleteDialogOpen = $event"><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{{ t('meetingDialogs.deleteAudioTitle') }}</AlertDialogTitle><AlertDialogDescription>{{ t('meetingDialogs.deleteAudioDescription') }}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel><AlertDialogAction variant="destructive" @click="confirmAudioDelete">{{ t('meetingDialogs.deleteAudio') }}</AlertDialogAction></AlertDialogFooter></AlertDialogContent></AlertDialog>
    <AlertDialog :open="overwriteDialogOpen" @update:open="overwriteDialogOpen = $event"><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{{ t('meetingDialogs.overwriteTitle') }}</AlertDialogTitle><AlertDialogDescription>{{ t('meetingDialogs.overwriteDescription') }}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{{ t('meetingDialogs.keepFile') }}</AlertDialogCancel><AlertDialogAction @click="confirmOverwrite">{{ t('meetingDialogs.replaceFile') }}</AlertDialogAction></AlertDialogFooter></AlertDialogContent></AlertDialog>
  </div>
</template>

<style scoped>
@keyframes meeting-details-open {
  from {
    height: 0;
    opacity: 0;
    transform: translateY(-0.25rem);
  }
  to {
    height: var(--reka-collapsible-content-height);
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes meeting-details-close {
  from {
    height: var(--reka-collapsible-content-height);
    opacity: 1;
    transform: translateY(0);
  }
  to {
    height: 0;
    opacity: 0;
    transform: translateY(-0.25rem);
  }
}

.meeting-details {
  overflow: hidden;
}

.meeting-details[data-state="open"] {
  animation: meeting-details-open 180ms ease-out;
}

.meeting-details[data-state="closed"] {
  animation: meeting-details-close 150ms ease-in;
}

@media (prefers-reduced-motion: reduce) {
  .meeting-details[data-state] {
    animation: none;
  }
}
</style>
