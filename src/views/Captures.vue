<script setup lang="ts">
import CaptureRow from "@/components/captures/CaptureRow.vue";
import SearchField from "@/components/SearchField.vue";
import StartMeetingDialog from "@/components/captures/StartMeetingDialog.vue";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
  AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { useCaptures } from "@/composables/useCaptures";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import {
  CAPTURE_ACTIONS, captureErrorCopy, type CaptureAction, type CaptureSummary,
} from "@/lib/captures";
import { groupByLocalDate, localDateKey } from "@/lib/format";
import { computed, nextTick, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { pluralCategory } from "@/i18n/plurals";

const { t, locale } = useI18n();
function meetingCountLabel(count: number): string {
  const category = pluralCategory(count, locale.value);
  const suffix = category[0].toUpperCase() + category.slice(1);
  return t('meetingsPage.meetingCount', { count, label: t(`meetingsPage.meetingCount${suffix}`) });
}
function bucketTitle(key: string, fallback: string): string {
  const today = new Date();
  if (key === localDateKey(today.getTime())) return t("historyPage.today");
  today.setDate(today.getDate() - 1);
  return key === localDateKey(today.getTime()) ? t("historyPage.yesterday") : fallback;
}

const captures = useCaptures();
const {
  summaries, needsAttention, inProgress, completed, query, loading, refreshing,
  loadingMore, hasMore, listError, stateFor, refresh, loadMore, setQuery,
} = captures;
const { activeCapture, startDialogRequested, consumeStartDialogRequest } = useMeetingCapture();

const startDialogOpen = ref(false);
const draftQuery = ref(query.value);
const pendingDelete = ref<CaptureSummary | null>(null);
const deleteDialogOpen = ref(false);
const pendingAudioDelete = ref<CaptureSummary | null>(null);
const audioDeleteDialogOpen = ref(false);
const pendingOverwrite = ref<CaptureSummary | null>(null);
const overwriteDialogOpen = ref(false);
const announcement = ref("");

watch(draftQuery, (value) => void setQuery(value));
watch(startDialogRequested, (requested) => {
  if (!requested) return;
  startDialogOpen.value = true;
  consumeStartDialogRequest();
}, { immediate: true });
watch(() => activeCapture.value?.id ?? null, (id, previous) => {
  if (id && !previous) announce(t("meetingsPage.recordingStarted"));
});

const isSearching = computed(() => query.value.trim().length > 0);
const empty = computed(() => !loading.value && summaries.value.length === 0);
const completedGroups = computed(() => groupByLocalDate(
  completed.value,
  (summary) => summary.started_at ?? summary.created_at,
  locale.value,
));
const knownActions = CAPTURE_ACTIONS;

function announce(message: string): void {
  announcement.value = message;
  window.setTimeout(() => { if (announcement.value === message) announcement.value = ""; }, 3000);
}

function busyActions(summary: CaptureSummary): CaptureAction[] {
  return knownActions.filter((action) => stateFor(summary.id, action).loading);
}

async function run(task: () => Promise<unknown>): Promise<void> {
  await task();
}

async function handleAction(action: CaptureAction, summary: CaptureSummary): Promise<void> {
  if (action === "delete") { pendingDelete.value = summary; deleteDialogOpen.value = true; return; }
  if (action === "delete_audio") { pendingAudioDelete.value = summary; audioDeleteDialogOpen.value = true; return; }
  if (action === "retry_cleanup") { await captures.retryDeleteCleanup(summary.id); return; }
  if (action === "retry_transcription") return run(() => captures.retryTranscription(summary.id));
  if (action === "pause_transcription") return run(() => captures.pauseTranscription(summary.id));
  if (action === "resume_transcription") return run(() => captures.resumeTranscription(summary.id));
  if (action === "cancel_transcription") return run(() => captures.cancelTranscription(summary.id));
  if (action === "retry_finalization") return run(() => captures.retryFinalization(summary.id));
  if (action === "choose_folder") return run(() => captures.chooseFolderAndRetry(summary.id));
  if (action === "cleanup_audio") return run(() => captures.retryAudioCleanup(summary.id));
  if (action === "save_audio") return run(() => captures.saveAudioCopy(summary));
  if (action === "copy_transcript") return run(() => captures.copyTranscript(summary.id));
  if (action === "copy_for_ai") return run(() => captures.copyForAi(summary.id));
  if (action === "copy_path") return run(() => captures.copyPath(summary.id));
  if (action === "reveal") return run(() => captures.revealTranscript(summary.id));
  if (action === "regenerate") {
    const result = await captures.regenerate(summary.id, false);
    const state = stateFor(summary.id, "regenerate");
    if (!result && state.error?.code === "output_file_exists") {
      pendingOverwrite.value = summary;
      overwriteDialogOpen.value = true;
    }
  }
}

async function confirmDelete(): Promise<void> {
  const summary = pendingDelete.value;
  deleteDialogOpen.value = false;
  if (!summary) return;
  const index = summaries.value.findIndex((row) => row.id === summary.id);
  if (await captures.deleteCapture(summary.id)) {
    await nextTick();
    const next = summaries.value[index] ?? summaries.value[index - 1];
    if (next) document.querySelector<HTMLElement>(`[data-capture-row="${next.id}"] a`)?.focus();
    else document.querySelector<HTMLElement>("[data-captures-heading]")?.focus();
  }
}

async function confirmAudioDelete(): Promise<void> {
  const summary = pendingAudioDelete.value;
  audioDeleteDialogOpen.value = false;
  if (summary) await run(() => captures.deleteAudio(summary.id));
}

async function confirmOverwrite(): Promise<void> {
  const summary = pendingOverwrite.value;
  overwriteDialogOpen.value = false;
  if (summary) await captures.regenerate(summary.id, true);
}
</script>

<template>
  <Teleport defer to="#page-header-actions">
    <SearchField class="w-[min(24rem,100%)]" v-model="draftQuery" :placeholder="t('meetingsPage.search')" :label="t('meetingsPage.search')" />
  </Teleport>

  <div class="mx-auto flex w-full max-w-6xl flex-col p-4" :aria-busy="loading || refreshing">
    <div class="flex flex-col">
      <div v-if="loading" class="flex flex-col gap-3" :aria-label="t('meetingsPage.loadingCaptures')">
        <div v-for="n in 5" :key="n" class="h-20 animate-pulse rounded-md bg-muted/50" />
      </div>
      <div v-else-if="listError && summaries.length === 0" class="flex flex-col items-center gap-3 py-10 text-center">
        <p class="text-sm text-muted-foreground">{{ captureErrorCopy(listError, t) }}</p>
        <Button variant="outline" size="sm" @click="refresh">{{ t('meetingsPage.retry') }}</Button>
      </div>
      <div v-else-if="empty" class="py-10 text-center text-sm text-muted-foreground">
        {{ isSearching ? t('meetingsPage.noResults', { query }) : t('meetingsPage.empty') }}
      </div>
      <div v-else class="flex flex-col gap-4">
        <p v-if="refreshing" class="text-xs text-muted-foreground" role="status">{{ t('meetingsPage.refreshing') }}</p>
        <div v-if="listError" class="flex items-center justify-between gap-2 rounded-md border border-destructive/40 bg-destructive/5 p-2 text-xs" role="alert">
          <span>{{ captureErrorCopy(listError, t) }}</span><Button variant="ghost" size="sm" @click="refresh">{{ t('meetingsPage.retry') }}</Button>
        </div>

        <section v-if="needsAttention.length" aria-labelledby="needs-attention-heading">
          <Card class="border-destructive/30 py-0">
            <CardContent class="flex flex-col gap-2 p-3">
              <h2 id="needs-attention-heading" class="px-1 pt-1 text-xs font-semibold uppercase tracking-wide text-destructive">
                {{ t('meetingsPage.needsAttention') }}
              </h2>
              <CaptureRow
                v-for="summary in needsAttention"
                :key="summary.id"
                :summary="summary"
                :allowed-actions="knownActions"
                :busy-actions="busyActions(summary)"
                @action="handleAction"
              />
            </CardContent>
          </Card>
        </section>

        <section v-if="inProgress.length" aria-labelledby="in-progress-heading">
          <Card class="py-0">
            <CardContent class="flex flex-col gap-2 p-3">
              <h2 id="in-progress-heading" class="px-1 pt-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                {{ t('meetingsPage.inProgress') }}
              </h2>
              <CaptureRow
                v-for="summary in inProgress"
                :key="summary.id"
                :summary="summary"
                :recording-active="!!activeCapture"
                :allowed-actions="knownActions"
                :busy-actions="busyActions(summary)"
                @action="handleAction"
              />
            </CardContent>
          </Card>
        </section>

        <section class="flex flex-col gap-2" :aria-label="t(isSearching ? 'meetingsPage.searchResults' : 'meetingsPage.meetings')">
          <h2 v-if="isSearching" class="px-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {{ t('meetingsPage.searchResults') }}
          </h2>
          <p v-if="isSearching && completed.length === 0" class="py-8 text-center text-sm text-muted-foreground">
            {{ t('meetingsPage.noResults', { query }) }}
          </p>
          <div v-if="completed.length" class="flex flex-col gap-6">
            <section
              v-for="bucket in completedGroups"
              :key="bucket.key"
              :aria-labelledby="`meeting-date-${bucket.key}`"
              class="flex flex-col"
            >
              <component
                :is="isSearching ? 'h3' : 'h2'"
                :id="`meeting-date-${bucket.key}`"
                class="flex items-center gap-2 px-1 pb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground"
              >
                {{ bucketTitle(bucket.key, bucket.label) }}
                <Badge
                  v-if="bucket.rows.length > 1"
                  variant="secondary"
                  class="h-4 font-normal text-[10px] tabular-nums"
                  :aria-label="meetingCountLabel(bucket.rows.length)"
                >
                  {{ bucket.rows.length }}
                </Badge>
              </component>
              <div class="flex flex-col gap-2">
                <CaptureRow
                  v-for="summary in bucket.rows"
                  :key="summary.id"
                  :summary="summary"
                  :allowed-actions="knownActions"
                  :busy-actions="busyActions(summary)"
                  @action="handleAction"
                />
              </div>
            </section>
          </div>
          <Button v-if="hasMore && !isSearching" variant="ghost" size="sm" class="mt-3 self-center" :disabled="loadingMore" @click="loadMore">
            {{ t(loadingMore ? 'meetingsPage.loading' : 'meetingsPage.loadMore') }}
          </Button>
        </section>
      </div>
    </div>

    <p class="sr-only" aria-live="polite">{{ announcement }}</p>
    <StartMeetingDialog v-model:open="startDialogOpen" />

    <AlertDialog :open="deleteDialogOpen" @update:open="deleteDialogOpen = $event">
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{{ t('meetingDialogs.deleteTitle', { title: pendingDelete?.title ?? '' }) }}</AlertDialogTitle><AlertDialogDescription>{{ t('meetingDialogs.deleteDescription') }}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel><AlertDialogAction variant="destructive" @click="confirmDelete">{{ t('meetingDialogs.deleteMeeting') }}</AlertDialogAction></AlertDialogFooter></AlertDialogContent>
    </AlertDialog>

    <AlertDialog :open="audioDeleteDialogOpen" @update:open="audioDeleteDialogOpen = $event">
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{{ t('meetingDialogs.deleteAudioTitle') }}</AlertDialogTitle><AlertDialogDescription>{{ t('meetingDialogs.deleteAudioDescription') }}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel><AlertDialogAction variant="destructive" @click="confirmAudioDelete">{{ t('meetingDialogs.deleteAudio') }}</AlertDialogAction></AlertDialogFooter></AlertDialogContent>
    </AlertDialog>

    <AlertDialog :open="overwriteDialogOpen" @update:open="overwriteDialogOpen = $event">
      <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{{ t('meetingDialogs.overwriteTitle') }}</AlertDialogTitle><AlertDialogDescription>{{ t('meetingDialogs.overwriteDescription') }}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{{ t('meetingDialogs.keepFile') }}</AlertDialogCancel><AlertDialogAction @click="confirmOverwrite">{{ t('meetingDialogs.replaceFile') }}</AlertDialogAction></AlertDialogFooter></AlertDialogContent>
    </AlertDialog>
  </div>
</template>
