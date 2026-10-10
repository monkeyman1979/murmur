<script setup lang="ts">
import { computed } from "vue";
import { Monitor, Moon, Sun } from "lucide-vue-next";
import { Button } from "@/components/ui/button";
import { IconTooltip } from "@/components/ui/tooltip";
import { useTheme } from "@/composables/useTheme";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const { mode, cycle } = useTheme();

const Icon = computed(() => {
  if (mode.value === "light") return Sun;
  if (mode.value === "dark") return Moon;
  return Monitor;
});

const nextLabel = computed(() => {
  if (mode.value === "system") return t('themeToggle.light');
  if (mode.value === "light") return t('themeToggle.dark');
  return t('themeToggle.system');
});
</script>

<template>
  <IconTooltip :label="nextLabel">
    <Button
      variant="ghost"
      size="icon"
      class="cursor-pointer"
      :aria-label="nextLabel"
      @click="cycle"
    >
      <component :is="Icon" />
    </Button>
  </IconTooltip>
</template>
