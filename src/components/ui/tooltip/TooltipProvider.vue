<script setup lang="ts">
import type { TooltipProviderProps } from 'reka-ui'
import { TooltipProvider } from 'reka-ui'
import { TOOLTIP_HOVER_DELAY_MS, TOOLTIP_SKIP_DELAY_MS } from './timing'

const props = withDefaults(defineProps<TooltipProviderProps>(), {
  delayDuration: TOOLTIP_HOVER_DELAY_MS,
  skipDelayDuration: TOOLTIP_SKIP_DELAY_MS,
})
</script>

<template>
  <TooltipProvider v-bind="props">
    <slot />
  </TooltipProvider>
</template>
