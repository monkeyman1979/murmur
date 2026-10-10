<script setup lang="ts">
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import MeetingTitleEditor from "@/components/captures/MeetingTitleEditor.vue";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { captureErrorCopy } from "@/lib/captures";
import { formatMeetingDuration } from "@/lib/format";
import { MoreHorizontal, Pause, Play, Square } from "lucide-vue-next";
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const {
  activeCapture,
  activeDurationMs,
  action,
  controls,
  error,
  pause,
  resume,
  stop,
  cancel,
} = useMeetingCapture();

const discardDialogOpen = ref(false);
const paused = computed(() => activeCapture.value?.status === "paused");
const busy = computed(() => action.value !== null);
const timer = computed(() => formatMeetingDuration(activeDurationMs.value));

async function confirmDiscard(): Promise<void> {
  discardDialogOpen.value = false;
  await cancel(true);
}
</script>

<template>
  <template v-if="activeCapture">
    <Card class="border-red-500/25 bg-red-500/5">
      <CardContent class="flex flex-wrap items-center gap-3 px-4">
        <span
          class="size-2 shrink-0 rounded-full bg-red-500"
          :class="paused ? 'opacity-60' : 'animate-pulse'"
          aria-hidden="true"
        />

        <div class="flex min-w-40 flex-1 flex-col gap-0.5">
          <MeetingTitleEditor
            :capture-id="activeCapture.id"
            :title="activeCapture.title"
            variant="active"
          />
          <span class="text-xs text-muted-foreground">
            {{ t(paused ? 'navigation.meetingPaused' : 'activeMeeting.recording') }}
          </span>
        </div>

        <time
          class="font-mono text-sm font-semibold tabular-nums"
          :datetime="`PT${Math.floor(activeDurationMs / 1000)}S`"
        >
          {{ timer }}
        </time>

        <div class="flex shrink-0 items-center gap-1">
          <Button
            v-if="controls.includes('resume')"
            type="button"
            variant="outline"
            size="sm"
            :disabled="busy"
            @click="resume"
          >
            <Play class="size-3.5 fill-current" aria-hidden="true" />
            {{ t('navigation.resume') }}
          </Button>
          <Button
            v-else
            type="button"
            variant="outline"
            size="sm"
            :disabled="busy || !controls.includes('pause')"
            @click="pause"
          >
            <Pause class="size-3.5 fill-current" aria-hidden="true" />
            {{ t('navigation.pause') }}
          </Button>
          <Button
            type="button"
            size="sm"
            :aria-label="t('meetingController.stopLabel')"
            :disabled="busy || !controls.includes('stop')"
            @click="stop"
          >
            <Square class="size-3 fill-current" aria-hidden="true" />
            {{ t('meetingController.stop') }}
          </Button>

          <DropdownMenu>
            <DropdownMenuTrigger as-child>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                class="size-8 cursor-pointer"
                :aria-label="t('activeMeeting.more')"
              >
                <MoreHorizontal class="size-4" aria-hidden="true" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" class="w-max min-w-40">
              <DropdownMenuItem
                variant="destructive"
                class="whitespace-nowrap"
                :disabled="busy || !controls.includes('cancel')"
                @select="discardDialogOpen = true"
              >
                {{ t('activeMeeting.discard') }}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        <p v-if="error" class="basis-full text-xs text-destructive" role="alert">
          {{ captureErrorCopy(error, t) }}
        </p>
      </CardContent>
    </Card>

    <AlertDialog :open="discardDialogOpen" @update:open="discardDialogOpen = $event">
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('activeMeeting.discardTitle') }}</AlertDialogTitle>
          <AlertDialogDescription>
            {{ t('activeMeeting.discardDescription') }}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{{ t('activeMeeting.keep') }}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" @click="confirmDiscard">
            {{ t('activeMeeting.discard') }}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </template>
</template>
