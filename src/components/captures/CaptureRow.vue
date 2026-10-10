<script setup lang="ts">
import CaptureActions from "@/components/captures/CaptureActions.vue";
import CaptureStatusBadge from "@/components/captures/CaptureStatusBadge.vue";
import MeetingTitleEditor from "@/components/captures/MeetingTitleEditor.vue";
import ProcessingProgress from "@/components/captures/ProcessingProgress.vue";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  audioRetentionLabel,
  captureStateMessage,
  hasUsableRecoveryAudio,
  hasViewableTranscript,
  isCaptureProgressing,
  primaryCaptureAction,
  statePresentation,
  type CaptureAction,
  type CaptureSummary,
} from "@/lib/captures";
import {
  formatCaptureDate,
  formatInteger,
  formatMeetingDuration,
} from "@/lib/format";
import { AudioLines } from "lucide-vue-next";
import { computed } from "vue";
import { useRouter } from "vue-router";
import { useI18n } from "vue-i18n";
import { pluralCategory } from "@/i18n/plurals";

const { t, locale } = useI18n();

const router = useRouter();

const props = defineProps<{
  summary: CaptureSummary;
  busyActions?: CaptureAction[];
  allowedActions?: CaptureAction[];
  primaryAction?: CaptureAction | null;
  recordingActive?: boolean;
}>();

defineEmits<{
  (event: "action", action: CaptureAction, summary: CaptureSummary): void;
}>();

const usableRecoveryAudio = computed(() => hasUsableRecoveryAudio(props.summary));
const detailTo = computed(() => hasViewableTranscript(props.summary)
  ? `/captures/${props.summary.id}`
  : undefined);
const actions = computed(() => {
  const available = statePresentation(
    props.summary.status,
    props.summary.markdown_status,
    usableRecoveryAudio.value,
    props.summary.error_code,
    props.summary.can_retry_transcription,
    props.summary.transcription_paused,
    props.recordingActive,
    props.summary.transcription_queued,
  ).actions.filter(
    (action) => action !== "rename" && !(action === "keep_audio" && props.summary.audio_kept),
  );
  return props.allowedActions
    ? available.filter((action) => props.allowedActions!.includes(action))
    : available;
});
const primary = computed(() => {
  const preferred = props.primaryAction === undefined
    ? primaryCaptureAction(props.summary)
    : props.primaryAction;
  // A queued transcription isn't doing work yet. Keep manual hold in More,
  // but do not present Pause as its prominent action until processing starts.
  if (preferred === "pause_transcription" && (props.recordingActive || props.summary.transcription_queued)) return null;
  return preferred && actions.value.includes(preferred) ? preferred : null;
});
const stateMessage = computed(() => props.summary.status === "transcribing"
  ? props.summary.transcription_paused ? t("captureMessages.paused")
    : props.recordingActive ? null // Badge and progress already say the short queued state.
    : props.summary.transcription_queued ? t("captureMessages.waiting") : null
  : captureStateMessage(props.summary, t));
const wordUnit = computed(() => {
  const category = pluralCategory(props.summary.word_count, locale.value);
  return t(`captureRow.${category === 'one' ? 'word' : category === 'few' ? 'wordsFew' : category === 'many' ? 'wordsMany' : 'words'}`);
});
const audioLabel = computed(() => audioRetentionLabel(props.summary, Date.now(), t, locale.value));
const showsProgress = computed(
  () =>
    Boolean(props.summary.processing) &&
    isCaptureProgressing(props.summary.status),
);
const rowInteractiveSelector = "a, button, input, textarea, select, [role='button'], [role='menuitem'], [tabindex]:not([tabindex='-1']), [data-row-interactive]";

function openMeeting(event: MouseEvent): void {
  if (
    event.defaultPrevented ||
    event.button !== 0 ||
    event.metaKey ||
    event.ctrlKey ||
    event.shiftKey ||
    event.altKey ||
    window.getSelection()?.isCollapsed === false
  ) return;
  const target = event.target;
  if (!(target instanceof Element)) return;
  if (target.closest(rowInteractiveSelector)) return;
  void router.push(`/captures/${props.summary.id}`);
}
</script>

<template>
  <article
    :data-capture-row="summary.id"
    class="grid cursor-pointer grid-cols-1 gap-2 rounded-lg bg-muted/50 p-2.5 transition-colors hover:bg-muted/80 focus-within:bg-muted/80 lg:grid-cols-[minmax(0,1fr)_auto_max-content] lg:items-center lg:gap-3"
    @click="openMeeting"
  >
    <div class="flex min-w-0 flex-1 flex-col gap-2 px-1 py-0.5">
      <MeetingTitleEditor
        :capture-id="summary.id"
        :title="summary.title"
        :to="`/captures/${summary.id}`"
      />

      <p
        v-if="stateMessage"
        class="text-xs leading-snug text-muted-foreground"
      >
        {{ stateMessage }}
      </p>
      <ProcessingProgress
        v-if="showsProgress"
        :processing="summary.processing!"
        :status="summary.status"
        :transcription-paused="summary.transcription_paused"
        :recording-active="recordingActive"
        :transcription-queued="summary.transcription_queued"
        compact
      />
    </div>

    <div class="flex min-h-6 items-center lg:justify-end">
      <CaptureStatusBadge
        :status="summary.status"
        :markdown-status="summary.markdown_status"
        :has-recovery-audio="summary.has_recovery_audio"
        :can-retry-transcription="summary.can_retry_transcription"
        :error-code="summary.error_code"
        :transcription-paused="summary.transcription_paused"
        :recording-active="recordingActive"
        :transcription-queued="summary.transcription_queued"
        :show-ready="false"
      />
    </div>

    <CaptureActions
      :actions="actions"
      :primary="primary"
      :primary-last="primary === 'copy_for_ai'"
      :detail-to="detailTo"
      :busy-actions="busyActions"
      class="min-w-0 justify-end"
      @action="$emit('action', $event, summary)"
    >
      <div class="flex shrink-0 items-center gap-2 whitespace-nowrap text-xs text-muted-foreground">
        <Tooltip v-if="audioLabel">
          <TooltipTrigger as-child>
            <span
              tabindex="0"
              :aria-label="audioLabel"
              class="inline-flex size-7 shrink-0 items-center justify-center rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <AudioLines class="size-3.5" aria-hidden="true" />
            </span>
          </TooltipTrigger>
          <TooltipContent><p>{{ audioLabel }}</p></TooltipContent>
        </Tooltip>

        <template v-if="summary.status === 'completed'">
          <span class="tabular-nums">
            {{ formatInteger(summary.word_count, locale) }}
            {{ wordUnit }}
          </span>
          <span aria-hidden="true">·</span>
        </template>
        <span class="tabular-nums">
          {{ formatMeetingDuration(summary.active_duration_ms) }}
        </span>
        <span aria-hidden="true">·</span>
        <time :datetime="new Date(summary.started_at ?? summary.created_at).toISOString()">
          {{ formatCaptureDate(summary.started_at ?? summary.created_at, locale) }}
        </time>
      </div>
    </CaptureActions>
  </article>
</template>
