<script setup lang="ts">
defineProps<{
  current: number;
  total: number;
  label: string;
}>();
</script>

<template>
  <div
    class="flex items-center gap-2"
    role="progressbar"
    :aria-valuenow="current"
    aria-valuemin="1"
    :aria-valuemax="total"
    :aria-label="label"
    :aria-valuetext="label"
  >
    <span v-for="step in total" :key="step" class="progress-track h-1.5 w-5 overflow-hidden rounded-full" aria-hidden="true">
      <span v-if="step <= current" class="progress-fill block h-full w-full rounded-full" :class="step < current ? 'opacity-50' : ''" />
    </span>
  </div>
</template>
