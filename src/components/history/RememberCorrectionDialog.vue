<script setup lang="ts">
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  useCorrections,
  type CorrectionRule,
} from "@/composables/useCorrections";
import type { TranscriptRecord } from "@/composables/useHistory";
import { ArrowRight, HelpCircle } from "lucide-vue-next";
import { computed, nextTick, ref, useTemplateRef, watch } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

/**
 * Quick-add correction dialog. Mirrors the Dictionary Corrections section's inline
 * form (Find → Replace + Add + toggles) so the two flows look and
 * feel identical — clicking Add commits one rule, clears the inputs,
 * keeps the toggles as the user set them, and refocuses Find so the
 * user can rip through multiple corrections from the same transcript
 * without reopening the dialog. Done closes the dialog when they're
 * finished.
 *
 * Toggle defaults match the Dictionary Corrections section's new-rule defaults:
 * case-sensitive OFF (so "tauri" / "Tauri" both match), whole-word
 * ON (so `"um" → ""` doesn't also chew through `"umbrella"`).
 */

const props = defineProps<{
  open: boolean;
  transcript: TranscriptRecord | null;
}>();

const emit = defineEmits<{
  (e: "update:open", value: boolean): void;
}>();

const { addRule } = useCorrections();

const draftFind = ref("");
const draftReplace = ref("");
const draftCaseSensitive = ref(false);
const draftWholeWord = ref(true);
const error = ref<string | null>(null);
const findInputRef = useTemplateRef<HTMLInputElement>("findInput");

// Reset state when the dialog opens — each invocation starts clean.
// Toggles reset to defaults too so a previous session's choices don't
// silently carry over into a new transcript. Auto-focus Find.
watch(
  () => props.open,
  async (isOpen) => {
    if (!isOpen) return;
    draftFind.value = "";
    draftReplace.value = "";
    draftCaseSensitive.value = false;
    draftWholeWord.value = true;
    error.value = null;
    await nextTick();
    findInputRef.value?.focus();
  },
);

const canAdd = computed(() => draftFind.value.trim().length > 0);

async function submit() {
  error.value = null;
  const find = draftFind.value.trim();
  if (!find) {
    error.value = t('rememberCorrection.required');
    return;
  }
  const rule: CorrectionRule = {
    find,
    replace: draftReplace.value,
    caseSensitive: draftCaseSensitive.value,
    wholeWord: draftWholeWord.value,
  };
  if (!addRule(rule)) {
    error.value = t('rememberCorrection.duplicate');
    return;
  }
  // Success — clear the inputs + refocus Find so the user can immediately
  // type the next correction. Toggles intentionally preserved: users
  // fixing a batch of mis-hears usually want the same match semantics
  // across the batch (all case-insensitive proper-noun fixes, all
  // case-sensitive acronym fixes, etc.).
  draftFind.value = "";
  draftReplace.value = "";
  await nextTick();
  findInputRef.value?.focus();
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-2xl">
      <DialogHeader>
        <DialogTitle>{{ t('rememberCorrection.title') }}</DialogTitle>
      </DialogHeader>

      <div v-if="transcript" class="flex flex-col gap-5">
        <div class="flex flex-col gap-2">
          <Label class="text-xs text-muted-foreground">
            {{ t('rememberCorrection.pasted') }}
          </Label>
          <blockquote
            class="max-h-32 overflow-y-auto text-sm leading-relaxed break-words"
          >
            “{{ transcript.text_final }}”
          </blockquote>
        </div>

        <div class="rounded-lg bg-muted/40 p-3">
          <p class="mb-3 text-xs leading-snug text-muted-foreground">
            {{ t('rememberCorrection.help') }}
          </p>

          <form
            class="flex flex-col gap-3"
            @submit.prevent="submit"
          >
          <div class="flex flex-wrap items-center gap-2 min-w-0">
            <Input
              ref="findInput"
              v-model="draftFind"
              :placeholder="t('correctionsPage.heard')"
              :aria-label="t('correctionsPage.find')"
              class="flex-1 min-w-0 basis-40 border-transparent bg-background/60 font-mono shadow-none focus-visible:border-ring"
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
              class="flex-1 min-w-0 basis-40 border-transparent bg-background/60 font-mono shadow-none focus-visible:border-ring"
              autocomplete="off"
              autocorrect="off"
              spellcheck="false"
            />
            <Button
              type="submit"
              size="sm"
              :disabled="!canAdd"
              class="shrink-0"
            >
              {{ t('correctionsPage.add') }}
            </Button>
          </div>

          <div class="flex flex-wrap gap-x-6 gap-y-2">
            <div class="flex items-center gap-2">
              <Switch
                id="remember-case-sensitive"
                v-model="draftCaseSensitive"
              />
              <Label
                for="remember-case-sensitive"
                class="text-xs font-normal cursor-pointer"
              >
                {{ t('correctionsPage.case') }}
              </Label>
            </div>
            <div class="flex items-center gap-2">
              <Switch
                id="remember-whole-word"
                v-model="draftWholeWord"
              />
              <Label
                for="remember-whole-word"
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
                      class="inline-flex size-4 items-center justify-center rounded-full text-muted-foreground opacity-70 hover:opacity-100 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
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

          <p
            v-if="error"
            class="text-[11px] text-destructive leading-snug"
          >
            {{ error }}
          </p>
          </form>
        </div>
      </div>

      <DialogFooter>
        <Button @click="emit('update:open', false)">
          {{ t('rememberCorrection.done') }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
