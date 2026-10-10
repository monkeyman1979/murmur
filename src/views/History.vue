<script setup lang="ts">
import RememberCorrectionDialog from "@/components/history/RememberCorrectionDialog.vue";
import TranscriptRow from "@/components/history/TranscriptRow.vue";
import SearchField from "@/components/SearchField.vue";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { IconTooltip } from "@/components/ui/tooltip";
import { useHistory, type TranscriptRecord } from "@/composables/useHistory";
import {
  formatInteger,
  groupByLocalDate,
  localDateKey,
} from "@/lib/format";
import { X } from "lucide-vue-next";
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { toast } from "vue-sonner";
import { useI18n } from "vue-i18n";

const { t, locale } = useI18n();

function countLabel(type: "dictations" | "matches" | "entries", count: number): string {
  const category = new Intl.PluralRules(locale.value).select(count);
  return t(`historyPage.counts.${type}.${category}`);
}

function bucketTitle(key: string, fallback: string): string {
  const today = new Date();
  if (key === localDateKey(today.getTime())) return t("historyPage.today");
  today.setDate(today.getDate() - 1);
  return key === localDateKey(today.getTime()) ? t("historyPage.yesterday") : fallback;
}

const {
  items,
  total,
  loading,
  hasMore,
  searchQuery,
  setSearch,
  loadMore,
  deleteOne,
} = useHistory();

const draftQuery = ref<string>(searchQuery.value);
const DICTATIONS_NOTICE_DISMISSED_KEY = "murmur:dictations-notice-dismissed";
const noticeVisible = ref(localStorage.getItem(DICTATIONS_NOTICE_DISMISSED_KEY) !== "true");

function dismissNotice(): void {
  noticeVisible.value = false;
  localStorage.setItem(DICTATIONS_NOTICE_DISMISSED_KEY, "true");
}

// 180 ms is the sweet spot: long enough that fast typing doesn't fire
// a query per keystroke, short enough that the result set feels
// responsive on a warm FTS index. Tracked as a plain number, not a
// ref, since nothing else watches it.
const SEARCH_DEBOUNCE_MS = 180;
let searchTimer: number | undefined;

watch(draftQuery, (value) => {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    void setSearch(value);
  }, SEARCH_DEBOUNCE_MS);
});

function commitSearch(): void {
  window.clearTimeout(searchTimer);
  void setSearch(draftQuery.value);
}

// The `useHistory` composable is module-scoped — History and Home both
// read from the same `items` ref. Leaving a search filter in place when
// navigating away pollutes Home's "Recent transcripts" card, which
// slices the first 8 off that same shared list. Clear the filter on
// unmount so Home always sees newest-first. Also cancels any
// in-flight debounced typing so a pending `setSearch(value)` can't
// land after we've already reset to "".
onBeforeUnmount(() => {
  window.clearTimeout(searchTimer);
  if (searchQuery.value.trim().length > 0) {
    void setSearch("");
  }
});

async function copyRow(row: TranscriptRecord): Promise<void> {
  try {
    await navigator.clipboard.writeText(row.text_final);
    toast.message(t("historyPage.copied"));
  } catch (err) {
    console.error("[murmur] clipboard copy failed", err);
  }
}

const empty = computed(
  () => !loading.value && items.value.length === 0,
);

const isSearching = computed(() => searchQuery.value.trim().length > 0);

// Items arrive newest-first, and the shared helper preserves that order.
// Search results keep the same grouping so matches across dates stay legible.
const grouped = computed(() => groupByLocalDate(items.value, (row) => row.created_at, locale.value));

// Remember-correction dialog state — single active entry at a time.
// Open/close is a v-model on the dialog; the transcript prop is the
// row the user clicked the wand on.
const correctionTarget = ref<TranscriptRecord | null>(null);
const correctionOpen = ref(false);

function openCorrection(row: TranscriptRecord): void {
  correctionTarget.value = row;
  correctionOpen.value = true;
}
</script>

<template>
  <Teleport defer to="#page-header-actions">
    <SearchField class="w-[min(24rem,100%)]" v-model="draftQuery" :placeholder="t('historyPage.search')" :label="t('historyPage.search')" @submit="commitSearch" />
  </Teleport>

  <div class="mx-auto flex w-full max-w-6xl flex-col gap-4 p-4">
    <Card v-if="noticeVisible" size="sm" class="shrink-0 bg-muted/20">
      <CardContent class="flex items-start justify-between gap-3">
        <i18n-t keypath="historyPage.notice" scope="global" tag="p" class="text-xs leading-snug text-muted-foreground text-balance">
          <template #copy><strong class="font-medium text-foreground">{{ t('historyPage.copyLast') }}</strong></template>
          <template #settings><strong class="font-medium text-foreground">{{ t('historyPage.settingsDictation') }}</strong></template>
        </i18n-t>
        <IconTooltip :label="t('historyPage.dismiss')">
          <Button
            variant="ghost"
            size="icon-xs"
            class="cursor-pointer text-muted-foreground"
            :aria-label="t('historyPage.dismissInformation')"
            @click="dismissNotice"
          >
            <X class="size-3.5" aria-hidden="true" />
          </Button>
        </IconTooltip>
      </CardContent>
    </Card>

    <div class="flex flex-col">
      <p v-if="empty" class="py-8 text-center text-sm text-muted-foreground">
        <template v-if="isSearching">
          <i18n-t keypath="historyPage.noMatches" scope="global" tag="span">
            <template #query><span class="font-mono">{{ searchQuery }}</span></template>
          </i18n-t>
        </template>
        <template v-else>
          {{ t('historyPage.noTranscripts') }}
        </template>
      </p>

      <div v-else class="flex flex-col gap-6">
        <section v-for="bucket in grouped" :key="bucket.key" class="flex flex-col">
          <h2 class="flex items-center gap-2 pb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {{ bucketTitle(bucket.key, bucket.label) }}
            <Badge
              v-if="bucket.rows.length > 1"
              variant="secondary"
              class="h-4 font-normal text-[10px] tabular-nums"
              :aria-label="t('historyPage.dictationCount', { count: bucket.rows.length, label: countLabel('dictations', bucket.rows.length) })"
            >
              {{ bucket.rows.length }}
            </Badge>
          </h2>
          <ul class="m-0 flex list-none flex-col divide-y divide-border p-0">
            <li v-for="row in bucket.rows" :key="row.id">
              <TranscriptRow
                :row="row"
                @remember="openCorrection"
                @copy="copyRow"
                @delete="(r) => deleteOne(r.id)"
              />
            </li>
          </ul>
        </section>
      </div>

      <div v-if="hasMore && !empty" class="flex justify-center pb-1 pt-3">
        <Button variant="ghost" size="sm" :disabled="loading" @click="loadMore">
          {{ t(loading ? 'historyPage.loading' : 'historyPage.loadMore') }}
        </Button>
      </div>

      <div class="mt-4 flex shrink-0 items-center gap-3 border-t border-border pt-3">
        <p class="text-xs text-muted-foreground tabular-nums">
          <template v-if="isSearching">
            {{ t('historyPage.showing', { count: formatInteger(items.length, locale), label: countLabel('matches', items.length) }) }}
          </template>
          <template v-else>
            {{ t('historyPage.total', { count: formatInteger(total, locale), label: countLabel('entries', total) }) }}
          </template>
        </p>
      </div>
    </div>

    <RememberCorrectionDialog v-model:open="correctionOpen" :transcript="correctionTarget" />
  </div>
</template>
