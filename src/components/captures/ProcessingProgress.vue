<script setup lang="ts">
import type { CaptureProcessingSnapshot, CaptureState } from "@/lib/captures";
import { computed } from "vue";
import { useI18n } from "vue-i18n";

const { t, locale } = useI18n();

const props = withDefaults(defineProps<{
  processing: CaptureProcessingSnapshot;
  status: CaptureState;
  transcriptionPaused?: boolean;
  recordingActive?: boolean;
  transcriptionQueued?: boolean;
  compact?: boolean;
}>(), { compact: false });

const phase = computed(() => {
  if (props.status === "transcribing" && props.transcriptionPaused) return t("captureStatuses.Transcription paused");
  if (props.status === "transcribing" && props.recordingActive) return t("captureStatuses.Queued during recording");
  if (props.status === "transcribing" && props.transcriptionQueued) return t("captureStatuses.Queued for transcription");
  if (props.status === "stopping") return t("captureStatuses.Finishing recording");
  if (props.status === "finalizing") return t("captureStatuses.Saving transcript");
  return props.processing.completed_units === 0
    ? t("captureProgress.starting")
    : t("captureStatuses.Transcribing audio");
});

const progressParts = computed(() => ({
  completed: new Intl.NumberFormat(locale.value).format(props.processing.completed_units),
  total: new Intl.NumberFormat(locale.value).format(props.processing.total_units),
}));
const progressPercent = computed(() =>
  Math.min(100, Math.max(0, props.processing.fraction * 100)),
);
</script>

<template>
  <!-- Wide enough that the label and the count stay on one line together. At
       the old compact width both wrapped, which turned a two-word status into a
       four-line block and made the bar look like a fragment of something. -->
  <div class="flex min-w-48 flex-col gap-1" :class="compact ? 'max-w-xs' : 'max-w-md'">
    <div class="flex items-center justify-between gap-2 text-xs whitespace-nowrap text-muted-foreground">
      <span>{{ phase }}</span>
      <span class="tabular-nums">{{ t('captureProgress.parts', progressParts) }}</span>
    </div>
    <div
      class="progress-track h-1.5 w-full overflow-hidden rounded-full"
      role="progressbar"
      :aria-valuenow="processing.completed_units"
      aria-valuemin="0"
      :aria-valuemax="processing.total_units"
      :aria-label="t('captureProgress.announcement', { phase, ...progressParts })"
    >
      <div
        class="progress-fill h-full rounded-full transition-[width] duration-300 ease-out"
        :style="{ width: `${progressPercent}%` }"
      />
    </div>
  </div>
</template>
