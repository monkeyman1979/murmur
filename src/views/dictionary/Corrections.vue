<script setup lang="ts">
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  IconTooltip,
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  useCorrections,
  type CorrectionRule,
} from "@/composables/useCorrections";
import { ArrowRight, HelpCircle, X } from "lucide-vue-next";
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useI18n } from "vue-i18n";
import { pluralCategory } from "@/i18n/plurals";

const { t, locale } = useI18n();

const { rules, addRule, removeRule } = useCorrections();

// Draft rule in the form. Defaults chosen to be the safest generally
// useful combo: case-insensitive (so "tauri" and "Tauri" both match),
// whole-word (so "lol" doesn't match inside "holocaust"). Users with
// specific needs flip the toggles per-rule.
const draftFind = ref("");
const draftReplace = ref("");
const draftCaseSensitive = ref(false);
const draftWholeWord = ref(true);

const canSubmit = computed(() => draftFind.value.trim().length > 0);

function submit() {
  if (!canSubmit.value) return;
  const rule: CorrectionRule = {
    find: draftFind.value,
    replace: draftReplace.value,
    caseSensitive: draftCaseSensitive.value,
    wholeWord: draftWholeWord.value,
  };
  if (addRule(rule)) {
    draftFind.value = "";
    draftReplace.value = "";
    // Leave the toggles as-is — users often add a batch of rules with
    // the same matching semantics (all whole-word, all case-sensitive
    // names, etc.) and resetting the switches every time is friction.
  }
}

const ruleCount = computed(() => rules.value.length);
</script>

<template>
  <div class="flex min-w-0 flex-col gap-4 p-4">
    <div class="flex flex-col gap-2 px-1">
      <i18n-t keypath="correctionsPage.intro" scope="global" tag="p" class="text-xs leading-snug text-muted-foreground text-balance">
        <template #replace><em>{{ t('correctionsPage.replace') }}</em></template>
        <template #vocabulary><RouterLink
          :to="{ name: 'dictionary', query: { section: 'vocabulary' } }"
          class="underline underline-offset-2 hover:text-foreground"
        >{{ t('correctionsPage.vocabulary') }}</RouterLink></template>
      </i18n-t>
    </div>

    <form
      class="flex flex-col gap-3"
      @submit.prevent="submit"
    >
      <div
        class="flex flex-wrap items-center gap-2 min-w-0"
      >
        <Input
          v-model="draftFind"
          :placeholder="t('correctionsPage.heard')"
          :aria-label="t('correctionsPage.find')"
          class="min-w-0 flex-1 basis-40 border-transparent bg-muted font-mono shadow-none focus-visible:border-ring dark:bg-muted"
          autocomplete="off"
          autocorrect="off"
          spellcheck="false"
        />
        <ArrowRight
          class="size-4 text-muted-foreground shrink-0"
          aria-hidden="true"
        />
        <Input
          v-model="draftReplace"
          :placeholder="t('correctionsPage.replacePlaceholder')"
          :aria-label="t('correctionsPage.replacement')"
          class="min-w-0 flex-1 basis-40 border-transparent bg-muted font-mono shadow-none focus-visible:border-ring dark:bg-muted"
          autocomplete="off"
          autocorrect="off"
          spellcheck="false"
        />
        <Button
          type="submit"
          size="sm"
          :disabled="!canSubmit"
          class="shrink-0"
        >
          {{ t('correctionsPage.add') }}
        </Button>
      </div>

      <div class="flex flex-wrap gap-x-6 gap-y-2">
        <div class="flex items-center gap-2">
          <Switch
            id="draft-case-sensitive"
            v-model="draftCaseSensitive"
          />
          <Label
            for="draft-case-sensitive"
            class="text-xs font-normal cursor-pointer"
          >
            {{ t('correctionsPage.case') }}
          </Label>
        </div>
        <div class="flex items-center gap-2">
          <Switch
            id="draft-whole-word"
            v-model="draftWholeWord"
          />
          <Label
            for="draft-whole-word"
            class="text-xs font-normal cursor-pointer"
          >
            {{ t('correctionsPage.wholeWord') }}
          </Label>
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <button
                  type="button"
                  :aria-label="t('correctionsPage.wholeWordQuestion')"
                  class="inline-flex size-4 cursor-pointer items-center justify-center rounded-full text-muted-foreground opacity-70 hover:bg-muted hover:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                >
                  <HelpCircle class="size-3.5" aria-hidden="true" />
                </button>
              </TooltipTrigger>
              <TooltipContent
                side="top"
                :side-offset="6"
                class="text-xs leading-snug"
              >
                <p>{{ t('correctionsPage.wholeWordHelp') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>
      </div>
    </form>

    <Card class="min-w-0 py-0">
      <CardContent class="flex min-w-0 flex-col gap-4 p-4">
        <div v-if="ruleCount === 0" class="text-xs text-muted-foreground">
          {{ t('correctionsPage.empty') }}
        </div>

        <ul
          v-else
          class="m-0 flex list-none flex-wrap items-start gap-2 p-0"
          :aria-label="t('correctionsPage.list')"
        >
          <li
            v-for="rule in rules"
            :key="JSON.stringify([rule.find, rule.caseSensitive, rule.wholeWord])"
            class="inline-flex w-fit max-w-full items-center gap-2 rounded-md border bg-muted/30 px-3 py-2"
          >
            <div
              class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 font-mono text-xs"
            >
              <span class="break-all">{{ rule.find }}</span>
              <ArrowRight
                class="size-3 text-muted-foreground shrink-0"
                aria-hidden="true"
              />
              <span
                v-if="rule.replace"
                class="break-all"
              >{{ rule.replace }}</span>
              <span
                v-else
                class="italic text-muted-foreground"
              >{{ t('correctionsPage.deleted') }}</span>
              <IconTooltip v-if="rule.caseSensitive" :label="t('correctionsPage.caseHint')">
                <Badge
                  variant="secondary"
                  class="h-5 font-sans font-normal text-[10px]"
                >
                  Aa
                </Badge>
              </IconTooltip>
              <IconTooltip v-if="rule.wholeWord" :label="t('correctionsPage.wholeWordHint')">
                <Badge
                  variant="secondary"
                  class="h-5 font-sans font-normal text-[10px]"
                >
                  {{ t('correctionsPage.wordBadge') }}
                </Badge>
              </IconTooltip>
            </div>
            <IconTooltip :label="t('correctionsPage.remove')">
              <button
                type="button"
                class="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground opacity-70 transition-all hover:bg-background/50 hover:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                :aria-label="t('correctionsPage.removeLabel', { find: rule.find, replace: rule.replace })"
                @click="removeRule(rule)"
              >
                <X class="size-3.5" aria-hidden="true" />
              </button>
            </IconTooltip>
          </li>
        </ul>
      </CardContent>
    </Card>

    <p
      v-if="ruleCount > 0"
      class="px-1 text-[11px] text-muted-foreground tabular-nums"
    >
      {{ t('correctionsPage.count', { count: new Intl.NumberFormat(locale).format(ruleCount), label: t(`correctionsPage.${pluralCategory(ruleCount, locale)}`) }) }}
    </p>
  </div>
</template>
