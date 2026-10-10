<script setup lang="ts">
import type { LucideIcon } from "lucide-vue-next";
import { Card, CardContent } from "@/components/ui/card";
import type { DurationPart } from "@/lib/format";

defineProps<{
  label: string;
  value?: string;
  unit?: string;
  valueParts?: DurationPart[];
  hint?: string;
  icon?: LucideIcon;
}>();
</script>

<template>
  <Card>
    <CardContent class="flex flex-col gap-2 px-4">
      <div class="flex items-center justify-between">
        <span class="text-xs font-medium capitalize text-muted-foreground">
          {{ label }}
        </span>
        <component
          v-if="icon"
          :is="icon"
          class="size-4 text-muted-foreground"
          aria-hidden="true"
        />
      </div>
      <div v-if="valueParts?.length" class="flex flex-wrap items-baseline gap-x-2">
        <span
          v-for="part in valueParts"
          :key="`${part.value}-${part.unit}`"
          class="inline-flex items-baseline gap-1 whitespace-nowrap"
        >
          <span class="text-2xl font-semibold tabular-nums leading-none">{{ part.value }}</span>
          <span class="text-xs font-normal text-muted-foreground">{{ part.unit }}</span>
        </span>
      </div>
      <div v-else class="flex items-baseline gap-1">
        <span class="text-2xl font-semibold tabular-nums leading-none">{{ value }}</span>
        <span v-if="unit" class="text-xs font-normal text-muted-foreground">{{ unit }}</span>
      </div>
      <p v-if="hint" class="text-xs text-muted-foreground leading-snug">{{ hint }}</p>
    </CardContent>
  </Card>
</template>
