<script setup lang="ts">
import type { HTMLAttributes } from 'vue'
import { cn } from '@/lib/utils'

const props = defineProps<{
  class?: HTMLAttributes['class']
}>()
</script>

<template>
  <main
    data-slot="sidebar-inset"
    :class="cn(
      'bg-app-content md:peer-data-[variant=inset]:m-2 md:peer-data-[variant=inset]:ml-0 md:peer-data-[variant=inset]:overflow-hidden md:peer-data-[variant=inset]:rounded-2xl md:peer-data-[variant=inset]:shadow-sm relative flex min-h-0 w-full flex-1 flex-col',
      props.class,
    )"
  >
    <slot />
  </main>
</template>
