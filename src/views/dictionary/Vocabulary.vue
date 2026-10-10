<script setup lang="ts">
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { IconTooltip } from "@/components/ui/tooltip";
import { useVocabulary } from "@/composables/useVocabulary";
import { X } from "lucide-vue-next";
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useI18n } from "vue-i18n";

const { t, locale } = useI18n();

const { words, addWord, removeWord } = useVocabulary();

const draft = ref("");
const recentlyAdded = ref<string | null>(null);

// Minimum characters before Enter submits. 2 is the sweet spot — it
// rejects accidental single-letter entries ("a", "i") while still
// allowing valid short acronyms ("OS", "UI", "QA"). One-letter
// vocabulary entries don't bias Whisper usefully anyway — the token
// budget goes to context that actually steers recognition.
const MIN_CHARS = 2;

function submit() {
  if (!canSubmit.value) return;
  const added = addWord(draft.value);
  if (added) {
    recentlyAdded.value = draft.value.trim();
    draft.value = "";
    // Tiny visual nudge; cleared after a bit so a rapid-fire add-spree
    // doesn't leave stale highlight state.
    window.setTimeout(() => {
      recentlyAdded.value = null;
    }, 1500);
  }
}

const wordCount = computed(() => words.value.length);

// Whisper's `initial_prompt` has a hard token budget (~224). At roughly
// 1.3 tokens per short English word + the "Glossary: …." scaffold,
// 50 words is the Rust-side cap — beyond that, added words get
// silently dropped from the bias prompt. Warn the user as they
// approach the ceiling.
const MAX_EFFECTIVE = 50;
const nearingCap = computed(() => wordCount.value >= MAX_EFFECTIVE - 5);
const atCap = computed(() => wordCount.value >= MAX_EFFECTIVE);
const canSubmit = computed(
  () => !atCap.value && draft.value.trim().length >= MIN_CHARS,
);
</script>

<template>
  <div class="flex min-w-0 flex-col gap-4 p-4">
    <div class="flex flex-col gap-2 px-1">
      <i18n-t keypath="vocabularyPage.intro" scope="global" tag="p" class="text-xs leading-snug text-muted-foreground text-balance">
        <template #correction><RouterLink
          :to="{ name: 'dictionary', query: { section: 'corrections' } }"
          class="underline underline-offset-2 hover:text-foreground"
        >{{ t('vocabularyPage.correction') }}</RouterLink></template>
      </i18n-t>
    </div>

    <form
      class="flex w-full max-w-[28rem] items-center gap-2"
      @submit.prevent="submit"
    >
      <Input
        v-model="draft"
        class="min-w-0 flex-1 border-transparent bg-muted shadow-none focus-visible:border-ring dark:bg-muted"
        :placeholder="t('vocabularyPage.placeholder')"
        :aria-label="t('vocabularyPage.input')"
        :disabled="atCap"
        autocomplete="off"
        autocorrect="off"
        spellcheck="false"
      />
      <Button
        type="submit"
        :disabled="!canSubmit"
      >
        {{ t('vocabularyPage.add') }}
      </Button>
    </form>

    <p
      v-if="atCap"
      class="px-1 text-[11px] leading-snug text-destructive"
    >
      {{ t('vocabularyPage.limit') }}
    </p>
    <p
      v-else-if="nearingCap"
      class="px-1 text-[11px] leading-snug text-muted-foreground"
    >
      {{ t('vocabularyPage.nearLimit') }}
    </p>

    <Card class="min-w-0 py-0">
      <CardContent class="flex min-w-0 flex-col gap-4 p-4">
        <div v-if="wordCount === 0" class="text-xs text-muted-foreground">
          {{ t('vocabularyPage.empty') }}
        </div>

        <ul
          v-else
          class="m-0 flex list-none flex-wrap gap-1.5 p-0"
          :aria-label="t('vocabularyPage.list')"
        >
          <li v-for="word in words" :key="word">
            <Badge
              :variant="recentlyAdded === word ? 'default' : 'secondary'"
              class="h-6 pr-1 font-mono"
            >
              <span class="break-all">{{ word }}</span>
              <IconTooltip :label="t('vocabularyPage.remove', { word })">
                <button
                  type="button"
                  class="inline-flex size-4 cursor-pointer items-center justify-center rounded-full opacity-60 transition-all hover:bg-background/30 hover:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                  :aria-label="t('vocabularyPage.remove', { word })"
                  @click="removeWord(word)"
                >
                  <X class="size-3" aria-hidden="true" />
                </button>
              </IconTooltip>
            </Badge>
          </li>
        </ul>
      </CardContent>
    </Card>

    <p
      v-if="wordCount > 0"
      class="px-1 text-[11px] text-muted-foreground tabular-nums"
    >
      {{ t('vocabularyPage.count', { count: new Intl.NumberFormat(locale).format(wordCount), max: MAX_EFFECTIVE }) }}
    </p>
  </div>
</template>
