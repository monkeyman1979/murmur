<script setup lang="ts">
import { AudioLines, MessagesSquare } from "lucide-vue-next";
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <div class="flex flex-col gap-7">
    <section v-for="feature in [
      { key: 'dictation', icon: AudioLines },
      { key: 'meeting', icon: MessagesSquare },
    ]" :key="feature.key" class="flex items-start gap-3">
      <component :is="feature.icon" class="mt-1 size-5 shrink-0 text-primary" aria-hidden="true" />
      <div class="flex min-w-0 flex-col gap-1.5">
        <h2 class="text-xl font-semibold tracking-tight">{{ t(`onboarding.productOverview.${feature.key}.title`) }}</h2>
        <p class="text-balance text-sm leading-5 text-muted-foreground">{{ t(`onboarding.productOverview.${feature.key}.body`) }}</p>
        <p class="text-balance text-xs leading-4 text-muted-foreground">{{ t(`onboarding.productOverview.${feature.key}.comparison`) }}</p>
      </div>
    </section>
  </div>
</template>
