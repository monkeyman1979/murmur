<script setup lang="ts">
// Reusable settings row — matches the Willow pattern: title + short
// description on the left, interactive control on the right. Slot the
// control so callers can drop in a Switch, a segmented button group,
// a "Change" button, whatever fits.

defineProps<{
  title: string;
  description?: string;
  /** When true, renders the row at lower contrast to signal a stub /
   *  coming-soon feature. Slot content remains interactive — decide
   *  there whether to disable the control itself. */
  muted?: boolean;
}>();
</script>

<template>
  <div
    class="flex flex-col gap-3 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4"
    :class="{ 'opacity-60': muted }"
  >
    <div class="flex flex-col gap-0.5 min-w-0">
      <p class="text-sm font-medium leading-tight">{{ title }}</p>
      <p
        v-if="description"
        class="text-xs text-muted-foreground leading-snug text-balance"
      >
        {{ description }}
      </p>
    </div>
    <div class="shrink-0">
      <slot />
    </div>
  </div>
</template>
