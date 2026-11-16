<script setup lang="ts">
import { useId } from "vue";
import { useI18n } from "vue-i18n";
import { INDICATOR_POSITIONS, type IndicatorPosition, type IndicatorStyle } from "@/composables/useIndicatorSettings";

const position = defineModel<IndicatorPosition>({ required: true });
const style = defineModel<IndicatorStyle>("style", { required: true });
defineProps<{ label: string }>();
const id = useId();
const { t } = useI18n();
</script>

<template>
  <div class="flex w-56 max-w-full flex-col gap-2">
    <fieldset class="flex rounded-md bg-muted p-0.5">
      <legend class="sr-only">{{ label }} — {{ t('indicatorPosition.displayStyle') }}</legend>
      <label v-for="mode in (['floating', 'notch'] as const)" :key="mode" class="flex-1 cursor-pointer">
        <input v-model="style" type="radio" :name="`${id}-style`" :value="mode" class="peer sr-only" />
        <span class="flex justify-center rounded-sm px-3 py-1.5 text-xs font-medium text-muted-foreground transition-colors peer-checked:bg-background peer-checked:text-foreground peer-checked:shadow-sm peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-ring">
          {{ t(`indicatorPosition.${mode}`) }}
        </span>
      </label>
    </fieldset>

    <!-- Both previews share one fixed height (h-[6.125rem]) so switching Floating/Notch never
         shifts the settings below. The floating grid's rows + padding + border
         add up to exactly that height. -->
    <fieldset v-if="style === 'floating'" class="placement-screen grid h-[6.125rem] grid-cols-3 grid-rows-[1.75rem_1.5rem_1.75rem] gap-x-3 rounded-lg border border-border bg-muted/30 p-2">
      <legend class="sr-only">{{ label }} — {{ t('indicatorPosition.floatingPosition') }}</legend>
      <label
        v-for="(anchor, index) in INDICATOR_POSITIONS" :key="anchor"
        class="flex cursor-pointer items-center justify-center rounded"
        :style="{ gridRow: index < 3 ? 1 : 3, gridColumn: index % 3 + 1 }"
        :title="t(`indicatorPosition.${anchor}`)"
      >
        <input v-model="position" type="radio" :name="`${id}-position`" :value="anchor" :aria-label="t(`indicatorPosition.${anchor}`)" class="peer sr-only" />
        <span class="flex h-full w-full items-center justify-center rounded text-muted-foreground/45 transition-colors hover:bg-muted peer-checked:bg-primary/10 peer-checked:text-primary peer-focus-visible:outline peer-focus-visible:outline-2 peer-focus-visible:outline-ring">
          <span class="h-1.5 w-5 rounded-full bg-current" aria-hidden="true" />
        </span>
      </label>
    </fieldset>
    <div v-else class="placement-screen grid h-[6.125rem] grid-cols-[1fr_5rem_1fr] grid-rows-[1.25rem_1fr] overflow-hidden rounded-lg border border-border bg-muted/30" aria-hidden="true">
      <!-- Notch tab: shallow corner radius like the real housing, and equal
           insets so the dot and timer pill sit symmetrically on one line. -->
      <div class="notch-preview col-start-2 flex items-center justify-between rounded-b-[5px] bg-black px-2">
        <span class="size-1 rounded-full bg-red-500" />
        <span class="h-1 w-2 rounded-full bg-white/60" />
      </div>
      <span class="col-span-3 row-start-2 self-center justify-self-center text-xs text-muted-foreground">{{ t('indicatorPosition.notch') }}</span>
    </div>
  </div>
</template>
