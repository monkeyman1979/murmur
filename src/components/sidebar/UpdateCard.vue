<script setup lang="ts">
import { Button } from "@/components/ui/button";
import { useSidebar } from "@/components/ui/sidebar";
import { IconTooltip } from "@/components/ui/tooltip";
import { useUpdateActions } from "@/composables/useUpdateActions";
import { Download, RefreshCw, X } from "lucide-vue-next";
import { computed } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const { updater, restartBlocked, statusLabel, actionLabel, runAction } = useUpdateActions();
const { state: sidebarState, isMobile } = useSidebar();

const visible = computed(() => updater.enabled && updater.sidebarVisible.value);
const compact = computed(() => sidebarState.value === "collapsed" && !isMobile.value);
</script>

<template>
  <IconTooltip v-if="visible && compact" :label="statusLabel" side="right">
    <button
      type="button"
      class="mx-auto inline-flex size-8 items-center justify-center rounded-md border border-sidebar-border text-sidebar-foreground transition-colors hover:bg-sidebar-hover disabled:cursor-not-allowed disabled:opacity-50"
      :aria-label="statusLabel"
      :disabled="updater.phase.value === 'downloading' || updater.phase.value === 'installing' || (updater.phase.value === 'ready' && restartBlocked)"
      @click="runAction"
    >
      <RefreshCw v-if="updater.phase.value === 'downloading' || updater.phase.value === 'installing'" class="size-4 animate-spin" aria-hidden="true" />
      <Download v-else class="size-4" aria-hidden="true" />
    </button>
  </IconTooltip>

  <section
    v-else-if="visible"
    class="my-1 flex min-w-0 flex-col gap-2 rounded-lg border border-sidebar-border bg-sidebar-hover p-3 text-sidebar-foreground"
    aria-live="polite"
  >
    <div class="flex items-start gap-2">
      <Download class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <div class="min-w-0 flex-1">
        <p class="text-xs font-semibold leading-5">{{ statusLabel }}</p>
        <p v-if="updater.phase.value === 'available' && updater.notes.value" class="line-clamp-2 text-[11px] leading-4 text-sidebar-foreground/65">
          {{ updater.notes.value }}
        </p>
      </div>
      <button
        v-if="updater.phase.value === 'available'"
        type="button"
        class="rounded-sm text-sidebar-foreground/60 hover:text-sidebar-foreground focus-visible:outline-2 focus-visible:outline-sidebar-ring"
        :aria-label="t('updatesCard.later')"
        @click="updater.dismissUpdate"
      >
        <X class="size-3.5" aria-hidden="true" />
      </button>
    </div>

    <div v-if="updater.phase.value === 'downloading'" class="h-1 overflow-hidden rounded-full bg-sidebar-border" role="progressbar" :aria-label="t('updatesCard.downloading')" :aria-valuenow="updater.progress.value === null ? undefined : Math.round(updater.progress.value * 100)">
      <div
        class="h-full rounded-full bg-sidebar-primary transition-[width]"
        :class="updater.progress.value === null ? 'w-1/3 animate-pulse' : ''"
        :style="updater.progress.value === null ? undefined : { width: `${updater.progress.value * 100}%` }"
      />
    </div>

    <Button
      v-if="actionLabel"
      size="sm"
      variant="sidebar"
      class="h-8 w-full text-xs"
      :disabled="updater.phase.value === 'ready' && restartBlocked"
      @click="runAction"
    >
      {{ actionLabel }}
    </Button>
  </section>
</template>
