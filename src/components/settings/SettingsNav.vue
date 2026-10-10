<script setup lang="ts">
import { RouterLink, useRoute } from "vue-router";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const route = useRoute();
const sections = [
  { labelKey: "settingsTabs.general", to: "/settings/general" },
  { labelKey: "settingsTabs.permissions", to: "/settings/permissions" },
  { labelKey: "settingsTabs.about", to: "/settings/about" },
];
</script>

<template>
  <Teleport defer to="#page-header-actions">
    <nav :aria-label="t('settingsTabs.label')" class="flex">
      <div class="inline-flex items-center gap-0.5 rounded-md bg-muted p-0.5">
        <RouterLink
          v-for="section in sections"
          :key="section.to"
          :to="section.to"
          :aria-current="route.path === section.to ? 'page' : undefined"
          :class="[
            'rounded-sm px-3 py-1.5 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
            route.path === section.to
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground',
          ]"
        >
          {{ t(section.labelKey) }}
        </RouterLink>
      </div>
    </nav>
  </Teleport>
</template>
