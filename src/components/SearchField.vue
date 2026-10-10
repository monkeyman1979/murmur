<script setup lang="ts">
import { Search, X } from "lucide-vue-next";
import { nextTick, ref } from "vue";
import { Input } from "@/components/ui/input";
import { IconTooltip } from "@/components/ui/tooltip";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const model = defineModel<string>({ required: true });

defineProps<{
  placeholder: string;
  label: string;
}>();

const emit = defineEmits<{ (event: "submit"): void }>();
const root = ref<HTMLDivElement | null>(null);

async function clear(): Promise<void> {
  model.value = "";
  await nextTick();
  root.value?.querySelector("input")?.focus();
}
</script>

<template>
  <div ref="root" class="relative shrink-0">
    <Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
    <Input
      v-model="model"
      class="border-transparent bg-muted pl-8 pr-8 shadow-none focus-visible:border-ring dark:bg-muted"
      :placeholder="placeholder"
      :aria-label="label"
      autocomplete="off"
      autocorrect="off"
      spellcheck="false"
      @keyup.enter="emit('submit')"
    />
    <IconTooltip v-if="model" :label="t('searchField.clear')">
      <button
        type="button"
        class="absolute right-2 top-1/2 inline-flex size-5 -translate-y-1/2 cursor-pointer items-center justify-center rounded-sm text-muted-foreground hover:bg-muted-foreground/10 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        :aria-label="t('searchField.clear')"
        @click="clear"
      >
        <X class="size-3.5" aria-hidden="true" />
      </button>
    </IconTooltip>
  </div>
</template>
