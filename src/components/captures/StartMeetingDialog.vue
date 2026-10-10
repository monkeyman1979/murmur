<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, ref, watch } from "vue";
import { Mic } from "lucide-vue-next";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import {
  autoMeetingTitle,
  canStartMeeting,
  captureErrorCopy,
  meetingRecordingNoticeSeen,
  setMeetingRecordingNoticeSeen,
} from "@/lib/captures";

const props = defineProps<{ open: boolean }>();
// No `started` event: the capture itself is the signal now. `Captures.vue`
// watches `activeCapture`, so a fast-path start — which never mounts this
// dialog — is treated identically to one begun here.
const emit = defineEmits<{ "update:open": [value: boolean] }>();

const { permissions } = useMurmurEvents();
const {
  activeCapture,
  capabilities,
  action,
  error,
  refreshCapabilities,
  start,
} = useMeetingCapture();

const noticeSeen = ref(meetingRecordingNoticeSeen());
// `starting` covers the whole start now — there is no picker to wait on.
const starting = ref(false);

const microphoneState = computed(() => permissions.value?.microphone ?? capabilities.value?.microphone ?? "unknown");
const canStart = computed(() => canStartMeeting(
  capabilities.value,
  microphoneState.value,
  !!activeCapture.value,
) && action.value !== "start");
const showMicrophonePrompt = computed(() => !!capabilities.value
  && capabilities.value.supported
  && capabilities.value.persistence_durable
  && microphoneState.value !== "granted");

watch(() => props.open, async (open) => {
  if (!open) return;
  if (!props.open) return;
  starting.value = false;
  // This component stays mounted for the whole session, so re-read the notice
  // state each time it opens rather than trusting setup-time state.
  noticeSeen.value = meetingRecordingNoticeSeen();
  void refreshCapabilities();
}, { immediate: true });

async function requestMicrophone(): Promise<void> {
  if (microphoneState.value === "denied") {
    await invoke("open_privacy_pane", { pane: "microphone" });
    return;
  }
  await invoke("request_microphone_prompt");
  window.setTimeout(() => void refreshCapabilities(), 500);
  window.setTimeout(() => void refreshCapabilities(), 2000);
}

async function begin(): Promise<void> {
  if (!canStart.value) return;
  starting.value = true;
  const capture = await start(autoMeetingTitle());
  starting.value = false;
  if (!capture) return;
  setMeetingRecordingNoticeSeen(true);
  noticeSeen.value = true;
  emit("update:open", false);
}

function close(): void {
  if (!starting.value) emit("update:open", false);
}
</script>

<template>
  <Dialog :open="open" @update:open="close">
    <DialogContent class="sm:max-w-lg" :show-close-button="!starting">
      <DialogHeader>
        <DialogTitle>{{ t(noticeSeen ? 'meetingStart.title' : 'meetingStart.firstTitle') }}</DialogTitle>
        <DialogDescription v-if="!noticeSeen" class="space-y-2">
          <span class="block">
            {{ t('meetingStart.notice') }}
          </span>
          <span class="block font-medium text-foreground">
            {{ t('meetingStart.consent') }}
          </span>
        </DialogDescription>
        <DialogDescription v-else class="sr-only">
          {{ t('meetingStart.review') }}
        </DialogDescription>
      </DialogHeader>

      <div class="grid gap-3">
        <div v-if="showMicrophonePrompt" class="flex items-center gap-3 rounded-lg border p-3" :aria-label="t('meetingStart.microphoneRequired')">
          <Mic class="size-4 shrink-0" aria-hidden="true" />
          <div class="min-w-0 flex-1">
            <p class="text-sm font-medium">{{ t('meetingStart.microphoneRequired') }}</p>
            <p class="text-xs text-muted-foreground">{{ t('meetingStart.microphoneHelp') }}</p>
          </div>
          <Button size="sm" variant="outline" @click="requestMicrophone">{{ t(microphoneState === 'denied' ? 'meetingStart.openSettings' : 'meetingStart.allow') }}</Button>
        </div>

        <div v-if="capabilities && !capabilities.supported" class="rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-xs" role="alert">
          {{ t('meetingStart.macosRequired', { version: capabilities.minimum_macos_version }) }}
        </div>
        <div v-else-if="capabilities && !capabilities.persistence_durable" class="rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-xs" role="alert">
          {{ t('meetingStart.databaseUnavailable') }}
        </div>

        <p v-if="error" class="text-xs text-destructive" role="alert">{{ captureErrorCopy(error, t) }}</p>
        <p v-if="starting" class="text-xs text-muted-foreground" role="status">{{ t('meetingStart.startingRecording') }}</p>
      </div>

      <DialogFooter>
        <Button variant="outline" :disabled="starting" @click="close">{{ t('meetingDialogs.cancel') }}</Button>
        <Button :disabled="!canStart || starting" @click="begin">
          {{ t(starting ? 'meetingStart.starting' : 'meetingStart.start') }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
