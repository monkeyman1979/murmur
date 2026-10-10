<script setup lang="ts">
import { Badge } from "@/components/ui/badge";
import { IconTooltip } from "@/components/ui/tooltip";
import type { TranscriptRecord } from "@/composables/useHistory";
import { useTranscriptRowHover } from "@/composables/useTranscriptRowHover";
import { formatCaptureDuration, formatTimestamp } from "@/lib/format";
import { Copy, Trash2, Wand2 } from "lucide-vue-next";
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { pluralCategory } from "@/i18n/plurals";

const { t, locale } = useI18n();

/**
 * Shared transcript row, used for both the Home "Recent transcripts"
 * preview and the `/history` archive. One component so the two
 * surfaces can't drift — timestamp on the left, transcript text +
 * metadata (app attribution, word count, capture duration) in the
 * middle, hover-revealed action buttons (remember-correction / copy
 * / delete) pinned to the right.
 *
 * Actions are emitted as events so the parent owns the side effects
 * — Home opens its own RememberCorrectionDialog, `/history` opens
 * its own, and both share the same `useHistory` composable for
 * copy / delete wiring.
 */

const props = defineProps<{
  row: TranscriptRecord;
}>();

defineEmits<{
  (e: "remember", row: TranscriptRecord): void;
  (e: "copy", row: TranscriptRecord): void;
  (e: "delete", row: TranscriptRecord): void;
}>();

// The app-shell scroll refactor exposed stale per-element hover/leave state in
// WebKit: rows received pointer entry, but not a matching leave, so every row
// visited by the pointer accumulated visible actions until a history refresh.
// One document-level tracker derives the active row from the current event
// target instead. That guarantees at most one hovered row and clears the state
// whenever the pointer moves anywhere outside a transcript row. The action
// cluster uses display:none rather than opacity so WebKit cannot leave a stale
// composited layer painted after the state changes.
const hoveredTranscriptId = useTranscriptRowHover();
const keyboardFocusInside = ref(false);
const wordUnit = computed(() => {
  const category = pluralCategory(props.row.word_count, locale.value);
  return t(`transcriptRow.${category === 'one' ? 'word' : category === 'few' ? 'wordsFew' : category === 'many' ? 'wordsMany' : 'words'}`);
});
const actionsVisible = computed(
  () => hoveredTranscriptId.value === props.row.id || keyboardFocusInside.value,
);

function handleActionsFocusIn(event: FocusEvent): void {
  keyboardFocusInside.value =
    event.target instanceof HTMLElement && event.target.matches(":focus-visible");
}

function handleActionsFocusOut(event: FocusEvent): void {
  const actions = event.currentTarget;
  if (!(actions instanceof HTMLElement)) return;
  if (!(event.relatedTarget instanceof Node) || !actions.contains(event.relatedTarget)) {
    keyboardFocusInside.value = false;
  }
}
</script>

<template>
  <div
    class="flex items-start gap-3 py-2.5 text-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
    :data-transcript-row-id="row.id"
    tabindex="0"
    @focusin="handleActionsFocusIn"
    @focusout="handleActionsFocusOut"
  >
    <span
      class="shrink-0 pt-0.5 text-xs text-muted-foreground tabular-nums w-16"
      :aria-label="t('transcriptRow.time')"
    >
      {{ formatTimestamp(row.created_at, locale) }}
    </span>
    <div class="flex flex-1 flex-col gap-1 leading-snug min-w-0">
      <span class="text-foreground break-words">{{ row.text_final }}</span>
      <div
        class="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground"
      >
        <span v-if="row.app_name">→ {{ row.app_name }}</span>
        <Badge
          variant="secondary"
          class="h-4 font-normal text-[10px] tabular-nums"
        >
          {{ new Intl.NumberFormat(locale).format(row.word_count) }} {{ wordUnit }}
        </Badge>
        <span class="tabular-nums">
          {{ formatCaptureDuration(row.duration_ms) }}
        </span>
      </div>
    </div>
    <div class="w-[5.75rem] shrink-0">
      <div
        v-show="actionsVisible"
        data-transcript-actions
        class="flex items-center gap-1"
      >
        <IconTooltip :label="t('transcriptRow.remember')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('transcriptRow.rememberLabel')"
            @click="$emit('remember', row)"
          >
            <Wand2 class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>
        <IconTooltip :label="t('transcriptRow.copy')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('transcriptRow.copy')"
            @click="$emit('copy', row)"
          >
            <Copy class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>
        <IconTooltip :label="t('transcriptRow.delete')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-destructive/10 hover:text-destructive focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('transcriptRow.delete')"
            @click="$emit('delete', row)"
          >
            <Trash2 class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>
      </div>
    </div>
  </div>
</template>
