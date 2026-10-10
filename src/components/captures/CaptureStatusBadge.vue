<script setup lang="ts">
import { Badge } from "@/components/ui/badge";
import { statePresentation, type CaptureMarkdownStatus, type CaptureState } from "@/lib/captures";
import { computed } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const props = withDefaults(defineProps<{
  status: CaptureState;
  markdownStatus?: CaptureMarkdownStatus;
  hasRecoveryAudio?: boolean;
  canRetryTranscription?: boolean;
  transcriptionPaused?: boolean;
  recordingActive?: boolean;
  transcriptionQueued?: boolean;
  errorCode?: string | null;
  showReady?: boolean;
}>(), {
  showReady: true,
});

// Must stay reactive: a capture's status changes underneath this badge as it
// moves from recording through transcribing to completed. Reading the props
// once at setup froze every badge on the state it first rendered with.
const presentation = computed(() => statePresentation(
  props.status,
  props.markdownStatus,
  props.hasRecoveryAudio,
  props.errorCode,
  props.canRetryTranscription,
  props.transcriptionPaused,
  props.recordingActive,
  props.transcriptionQueued,
));
</script>

<template>
  <Badge
    v-if="props.showReady || presentation.label !== 'Ready'"
    :variant="presentation.tone === 'danger' ? 'destructive' : presentation.tone === 'success' ? 'default' : presentation.tone === 'warning' ? 'outline' : 'secondary'"
    class="whitespace-nowrap"
  >
    {{ t(`captureStatuses.${presentation.label}`) }}
  </Badge>
</template>
