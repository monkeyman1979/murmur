<script setup lang="ts">
import Tooltip from "./Tooltip.vue";
import TooltipContent from "./TooltipContent.vue";
import TooltipProvider from "./TooltipProvider.vue";
import TooltipTrigger from "./TooltipTrigger.vue";

withDefaults(defineProps<{
  label: string;
  side?: "top" | "right" | "bottom" | "left";
  sideOffset?: number;
  delayDuration?: number;
}>(), {
  side: "top",
  sideOffset: 6,
});
</script>

<template>
  <TooltipProvider :delay-duration="delayDuration">
    <Tooltip>
      <TooltipTrigger as-child>
        <slot />
      </TooltipTrigger>
      <TooltipContent :side="side" :side-offset="sideOffset">
        <p>{{ label }}</p>
      </TooltipContent>
    </Tooltip>
  </TooltipProvider>
</template>
