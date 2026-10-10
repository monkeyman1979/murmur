<script setup lang="ts">
import Corrections from "@/views/dictionary/Corrections.vue";
import Vocabulary from "@/views/dictionary/Vocabulary.vue";
import { computed } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const route = useRoute();
const activeSection = computed<"vocabulary" | "corrections">(() =>
  route.query.section === "corrections" ? "corrections" : "vocabulary",
);
</script>

<template>
  <div class="flex min-w-0 flex-col">
    <Teleport defer to="#page-header-actions">
      <nav
        class="inline-flex items-center gap-0.5 rounded-md bg-muted p-0.5"
        :aria-label="t('dictionaryTabs.label')"
      >
        <RouterLink
          :to="{ name: 'dictionary', query: { section: 'vocabulary' } }"
          :aria-current="activeSection === 'vocabulary' ? 'page' : undefined"
          :class="[
            'rounded-sm px-3 py-1 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
            activeSection === 'vocabulary'
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground',
          ]"
        >
          {{ t('dictionaryTabs.vocabulary') }}
        </RouterLink>
        <RouterLink
          :to="{ name: 'dictionary', query: { section: 'corrections' } }"
          :aria-current="activeSection === 'corrections' ? 'page' : undefined"
          :class="[
            'rounded-sm px-3 py-1 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
            activeSection === 'corrections'
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground',
          ]"
        >
          {{ t('dictionaryTabs.corrections') }}
        </RouterLink>
      </nav>
    </Teleport>

    <div class="flex min-w-0 flex-col">
      <Transition name="tab-fade" mode="out-in">
        <Vocabulary
          v-if="activeSection === 'vocabulary'"
          key="vocabulary"
        />
        <Corrections v-else key="corrections" />
      </Transition>
    </div>
  </div>
</template>
