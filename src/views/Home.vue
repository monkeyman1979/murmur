<script setup lang="ts">
import ActiveMeetingCard from "@/components/home/ActiveMeetingCard.vue";
import CaptureRow from "@/components/captures/CaptureRow.vue";
import PermissionsBanner from "@/components/PermissionsBanner.vue";
import SpeechModelBanner from "@/components/SpeechModelBanner.vue";
import RememberCorrectionDialog from "@/components/history/RememberCorrectionDialog.vue";
import TranscriptRow from "@/components/history/TranscriptRow.vue";
import StatCard from "@/components/home/StatCard.vue";
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
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { useCaptures } from "@/composables/useCaptures";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { useHistory, type TranscriptRecord } from "@/composables/useHistory";
import { useHistoryStats } from "@/composables/useHistoryStats";
import { useHotkeyBinding } from "@/composables/useHotkeyBinding";
import {
  CAPTURE_ACTIONS,
  type CaptureAction,
  type CaptureState,
  type CaptureSummary,
} from "@/lib/captures";
import { formatDurationParts, formatInteger } from "@/lib/format";
import { bindingSegments } from "@/lib/hotkey";
import { ArrowRight, Gauge, MessagesSquare, Mic, Timer, X } from "lucide-vue-next";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { toast } from "vue-sonner";
import { useI18n } from "vue-i18n";
import { pluralCategory, timeUnitKey } from "@/i18n/plurals";

const { t, locale } = useI18n();

const { items: historyItems, total: historyTotal, deleteOne } = useHistory();
const { dictatedWords, timeSavedMs, averageWpm } = useHistoryStats();
const { binding: hotkeyBinding } = useHotkeyBinding();
const captures = useCaptures();
const { activeCapture } = useMeetingCapture();
const { rawSummaries, completedCount, stateFor } = captures;

const hotkeySegments = computed<string[]>(() =>
  bindingSegments(hotkeyBinding.value, t),
);

const recentDictations = computed<TranscriptRecord[]>(() =>
  historyItems.value.slice(0, 3),
);

const homeMeetingStates = new Set<CaptureState>([
  "stopping",
  "transcribing",
  "finalizing",
  "completed",
]);
const eligibleMeetings = computed<CaptureSummary[]>(() =>
  rawSummaries.value.filter((summary) => homeMeetingStates.has(summary.status)),
);
const recentMeetings = computed<CaptureSummary[]>(() => eligibleMeetings.value.slice(0, 3));
// Stats count every completed transcript, including rows beyond the loaded page.
// The visible list also contains still-processing meetings that stats exclude.
const showAllMeetings = computed(() => Math.max(completedCount.value, eligibleMeetings.value.length) > 3);
const showAllDictations = computed(() => Math.max(historyTotal.value, historyItems.value.length) > 3);

const AI_TIP_KEY = "murmur:copy-for-ai-tip-seen";
function tipWasSeen(): boolean {
  try { return localStorage.getItem(AI_TIP_KEY) === "true"; }
  catch { return false; }
}
const aiTipSeen = ref(tipWasSeen());
const aiTipOpen = ref(false);
const firstMeetingReady = computed(() => completedCount.value === 1
  && eligibleMeetings.value.some((summary) => summary.status === "completed"));
watch(firstMeetingReady, (ready) => {
  if (ready && !aiTipSeen.value) aiTipOpen.value = true;
}, { immediate: true });
function dismissAiTip(): void {
  aiTipOpen.value = false;
  aiTipSeen.value = true;
  try { localStorage.setItem(AI_TIP_KEY, "true"); }
  catch { /* Continue without persistence if browser storage is unavailable. */ }
}

const homeMeetingActions = CAPTURE_ACTIONS;

async function handleCopy(row: TranscriptRecord): Promise<void> {
  try {
    await navigator.clipboard.writeText(row.text_final);
    toast.message(t("historyPage.copied"));
  } catch (err) {
    console.error("[murmur] clipboard copy failed", err);
  }
}

function handleDelete(row: TranscriptRecord): void {
  void deleteOne(row.id);
}

const correctionTarget = ref<TranscriptRecord | null>(null);
const correctionOpen = ref(false);

function handleRemember(row: TranscriptRecord): void {
  correctionTarget.value = row;
  correctionOpen.value = true;
}

const pendingMeetingDelete = ref<CaptureSummary | null>(null);
const meetingDeleteDialogOpen = ref(false);
const pendingMeetingAudioDelete = ref<CaptureSummary | null>(null);
const meetingAudioDeleteDialogOpen = ref(false);
const pendingMeetingOverwrite = ref<CaptureSummary | null>(null);
const meetingOverwriteDialogOpen = ref(false);

function busyMeetingActions(summary: CaptureSummary): CaptureAction[] {
  return homeMeetingActions.filter((action) => stateFor(summary.id, action).loading);
}

async function handleMeetingAction(
  action: CaptureAction,
  summary: CaptureSummary,
): Promise<void> {
  if (action === "delete") {
    pendingMeetingDelete.value = summary;
    meetingDeleteDialogOpen.value = true;
    return;
  }
  if (action === "delete_audio") {
    pendingMeetingAudioDelete.value = summary;
    meetingAudioDeleteDialogOpen.value = true;
    return;
  }
  if (action === "pause_transcription") await captures.pauseTranscription(summary.id);
  if (action === "resume_transcription") await captures.resumeTranscription(summary.id);
  if (action === "cancel_transcription") await captures.cancelTranscription(summary.id);
  if (action === "save_audio") await captures.saveAudioCopy(summary);
  if (action === "keep_audio") await captures.keepAudio(summary.id);
  if (action === "copy_for_ai") await captures.copyForAi(summary.id);
  if (action === "copy_transcript") await captures.copyTranscript(summary.id);
  if (action === "copy_path") await captures.copyPath(summary.id);
  if (action === "reveal") await captures.revealTranscript(summary.id);
  if (action === "regenerate") {
    const result = await captures.regenerate(summary.id, false);
    if (!result && stateFor(summary.id, "regenerate").error?.code === "output_file_exists") {
      pendingMeetingOverwrite.value = summary;
      meetingOverwriteDialogOpen.value = true;
    }
  }
}

async function confirmMeetingDelete(): Promise<void> {
  const summary = pendingMeetingDelete.value;
  meetingDeleteDialogOpen.value = false;
  if (!summary) return;
  if (await captures.deleteCapture(summary.id)) {
    await captures.refresh();
  }
  pendingMeetingDelete.value = null;
}

async function confirmMeetingAudioDelete(): Promise<void> {
  const summary = pendingMeetingAudioDelete.value;
  meetingAudioDeleteDialogOpen.value = false;
  if (summary) await captures.deleteAudio(summary.id);
  pendingMeetingAudioDelete.value = null;
}

async function confirmMeetingOverwrite(): Promise<void> {
  const summary = pendingMeetingOverwrite.value;
  meetingOverwriteDialogOpen.value = false;
  if (summary) await captures.regenerate(summary.id, true);
  pendingMeetingOverwrite.value = null;
}

const avgWpmDisplay = computed<string>(() =>
  averageWpm.value === null ? "0" : formatInteger(averageWpm.value, locale.value),
);
const timeSavedParts = computed(() => formatDurationParts(timeSavedMs.value, locale.value).map((part) => ({
  ...part,
  unit: t(timeUnitKey(part.unit, Number(part.value.replace(/\D/g, "")), locale.value)),
})));
const wordUnit = computed(() => {
  const category = pluralCategory(dictatedWords.value, locale.value);
  return t(`captureRow.${category === 'one' ? 'word' : category === 'few' ? 'wordsFew' : category === 'many' ? 'wordsMany' : 'words'}`);
});
const transcriptUnit = computed(() => {
  const category = pluralCategory(completedCount.value, locale.value);
  return t(`homePage.${category === 'one' ? 'transcript' : category === 'few' ? 'transcriptsFew' : 'transcripts'}`);
});
</script>

<template>
  <!-- The shell target is inserted in this same tick after onboarding. -->
  <Teleport defer to="#page-header-actions">
    <p class="hidden items-center whitespace-nowrap text-xs text-muted-foreground sm:flex">
      {{ t('homePage.shortcutBefore') }}
      <template v-for="(seg, i) in hotkeySegments" :key="i">
        <span v-if="i > 0" class="mx-0.5">+</span>
        <kbd class="mx-1 rounded-md bg-muted px-1.5 py-0.5 font-mono text-xs">
          {{ seg }}
        </kbd>
      </template>
      {{ t('homePage.shortcutAfter') }}
    </p>
  </Teleport>

  <div class="flex flex-col gap-4 p-4">
    <SpeechModelBanner />
    <PermissionsBanner />

    <div
      class="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(min(100%,11rem),1fr))]"
    >
      <StatCard
        :label="t('homePage.dictated')"
        :value="formatInteger(dictatedWords, locale)"
        :unit="wordUnit"
        :icon="Mic"
      />
      <StatCard
        :label="t('homePage.timeSaved')"
        :value-parts="timeSavedParts"
        :icon="Timer"
      />
      <StatCard
        :label="t('homePage.averageSpeed')"
        :value="avgWpmDisplay"
        :unit="t('homePage.wpm')"
        :icon="Gauge"
      />
      <StatCard
        :label="t('navigation.meetings')"
        :value="formatInteger(completedCount, locale)"
        :unit="transcriptUnit"
        :icon="MessagesSquare"
      />
    </div>

    <ActiveMeetingCard />

    <Card class="flex flex-col">
      <CardHeader class="flex shrink-0 flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle class="text-base">{{ t('homePage.recentMeetings') }}</CardTitle>
        <RouterLink
          v-if="showAllMeetings"
          to="/captures"
          class="inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          {{ t('homePage.viewAll') }}
          <ArrowRight class="size-3" aria-hidden="true" />
        </RouterLink>
      </CardHeader>
      <CardContent class="flex flex-col gap-2">
        <aside v-if="aiTipOpen && completedCount > 0" id="copy-for-ai-tip" class="rounded-lg border border-border bg-muted/50 p-3 text-sm" :aria-label="t('homePage.aiTipLabel')">
          <div class="flex items-start gap-3">
            <div class="min-w-0 flex-1">
              <p>{{ t('homePage.aiTip') }}</p>
              <RouterLink :to="{ name: 'settings-general', query: { section: 'ai-handoff' } }" class="mt-2 inline-block underline underline-offset-4" @click="dismissAiTip">{{ t('homePage.aiTipLink') }}</RouterLink>
            </div>
            <button type="button" class="shrink-0 rounded-sm p-1 text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" :aria-label="t('homePage.aiTipDismiss')" @click="dismissAiTip"><X class="size-4" aria-hidden="true" /></button>
          </div>
        </aside>
        <p v-if="recentMeetings.length === 0" class="text-sm text-muted-foreground">
          {{ t('homePage.emptyMeetings') }}
        </p>
        <CaptureRow
          v-for="summary in recentMeetings"
          v-else
          :key="summary.id"
          :summary="summary"
          :recording-active="!!activeCapture"
          :allowed-actions="homeMeetingActions"
          :busy-actions="busyMeetingActions(summary)"
          @action="handleMeetingAction"
        />
      </CardContent>
    </Card>

    <Card class="flex flex-col">
      <CardHeader class="flex shrink-0 flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle class="text-base">{{ t('homePage.recentDictations') }}</CardTitle>
        <RouterLink
          v-if="showAllDictations"
          to="/history"
          class="inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          {{ t('homePage.viewAll') }}
          <ArrowRight class="size-3" aria-hidden="true" />
        </RouterLink>
      </CardHeader>
      <CardContent>
        <p v-if="recentDictations.length === 0" class="text-sm text-muted-foreground">
          {{ t('homePage.emptyDictations') }}
        </p>
        <ul v-else class="m-0 flex list-none flex-col divide-y divide-border p-0">
          <li v-for="row in recentDictations" :key="row.id">
            <TranscriptRow
              :row="row"
              @remember="handleRemember"
              @copy="handleCopy"
              @delete="handleDelete"
            />
          </li>
        </ul>
      </CardContent>
    </Card>

    <RememberCorrectionDialog
      v-model:open="correctionOpen"
      :transcript="correctionTarget"
    />

    <AlertDialog
      :open="meetingDeleteDialogOpen"
      @update:open="meetingDeleteDialogOpen = $event"
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('meetingDialogs.deleteTitle', { title: pendingMeetingDelete?.title ?? '' }) }}</AlertDialogTitle>
          <AlertDialogDescription>
            {{ t('meetingDialogs.deleteDescription') }}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" @click="confirmMeetingDelete">
            {{ t('meetingDialogs.deleteMeeting') }}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>

    <AlertDialog
      :open="meetingAudioDeleteDialogOpen"
      @update:open="meetingAudioDeleteDialogOpen = $event"
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('meetingDialogs.deleteAudioTitle') }}</AlertDialogTitle>
          <AlertDialogDescription>
            {{ t('meetingDialogs.deleteAudioDescription') }}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" @click="confirmMeetingAudioDelete">
            {{ t('meetingDialogs.deleteAudio') }}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>

    <AlertDialog
      :open="meetingOverwriteDialogOpen"
      @update:open="meetingOverwriteDialogOpen = $event"
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('meetingDialogs.overwriteTitle') }}</AlertDialogTitle>
          <AlertDialogDescription>
            {{ t('meetingDialogs.overwriteDescription') }}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{{ t('meetingDialogs.keepFile') }}</AlertDialogCancel>
          <AlertDialogAction @click="confirmMeetingOverwrite">{{ t('meetingDialogs.replaceFile') }}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>
</template>
