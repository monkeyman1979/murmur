<script setup lang="ts">
import type { HTMLAttributes } from 'vue'

import { PanelLeftIcon } from 'lucide-vue-next'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { IconTooltip } from '@/components/ui/tooltip'
import { useSidebar } from './utils'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  class?: HTMLAttributes['class']
}>()

const { toggleSidebar } = useSidebar()
const { t } = useI18n()
</script>

<template>
  <IconTooltip :label="t('navigation.toggleSidebar')">
    <Button
      data-sidebar="trigger"
      data-slot="sidebar-trigger"
      variant="ghost"
      size="icon-sm"
      :class="cn('cursor-pointer text-sidebar-foreground hover:bg-sidebar-hover hover:text-sidebar-foreground dark:hover:bg-sidebar-hover aria-expanded:text-sidebar-foreground', props.class)"
      :aria-label="t('navigation.toggleSidebar')"
      @click="toggleSidebar"
    >
      <PanelLeftIcon class="cn-rtl-flip" />
    </Button>
  </IconTooltip>
</template>
