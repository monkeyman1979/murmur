<script setup lang="ts">
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { CaptureAction } from "@/lib/captures";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { MoreHorizontal, Pause, Play } from "lucide-vue-next";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const props = defineProps<{
  actions: CaptureAction[];
  primary?: CaptureAction | null;
  primaryLast?: boolean;
  detailTo?: string;
  busyActions?: CaptureAction[];
}>();

const emit = defineEmits<{ (event: "action", action: CaptureAction): void }>();

const destructive = new Set<CaptureAction>(["delete", "delete_audio"]);
const overflow = computed(() => props.actions.filter((action) => action !== props.primary));
const busy = (action: CaptureAction) => props.busyActions?.includes(action) ?? false;
const transcriptionToggle = (action: CaptureAction) => action === "pause_transcription" || action === "resume_transcription";
const actionLabel = (action: CaptureAction) => busy(action)
  ? t(action === "cancel_transcription" ? "captureActions.stopping" : "captureActions.working")
  : t(`captureActions.${action}`);
</script>

<template>
  <div class="flex min-w-0 flex-wrap items-center gap-2 lg:flex-nowrap">
    <span v-if="primary && !primaryLast" data-row-interactive class="inline-flex shrink-0">
      <Tooltip v-if="transcriptionToggle(primary)">
        <TooltipTrigger as-child>
          <Button type="button" variant="outline" size="icon" class="size-8 cursor-pointer"
            :aria-label="actionLabel(primary)" :disabled="busy(primary)" @click="emit('action', primary)">
            <Play v-if="primary === 'resume_transcription'" class="size-4" aria-hidden="true" />
            <Pause v-else class="size-4" aria-hidden="true" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>{{ actionLabel(primary) }}</TooltipContent>
      </Tooltip>
      <Button v-else type="button" variant="default" size="sm" class="cursor-pointer"
        :disabled="busy(primary)" @click="emit('action', primary)">
        {{ actionLabel(primary) }}
      </Button>
    </span>

    <slot />

    <span v-if="detailTo || overflow.length" data-row-interactive class="inline-flex shrink-0">
      <DropdownMenu>
        <DropdownMenuTrigger as-child>
          <Button variant="ghost" size="icon" class="size-8 cursor-pointer" :aria-label="t('captureActions.more')">
            <MoreHorizontal class="size-4" aria-hidden="true" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" class="w-max min-w-52">
          <DropdownMenuItem v-if="detailTo" as-child>
            <RouterLink :to="detailTo">{{ t('captureActions.view') }}</RouterLink>
          </DropdownMenuItem>
          <DropdownMenuSeparator v-if="detailTo && overflow.length" />
          <template v-for="(action, index) in overflow" :key="action">
            <DropdownMenuSeparator v-if="index > 0 && destructive.has(action)" />
            <DropdownMenuItem
              :variant="destructive.has(action) ? 'destructive' : 'default'"
              :disabled="busy(action)"
              @select="emit('action', action)"
            >
              {{ actionLabel(action) }}
            </DropdownMenuItem>
          </template>
        </DropdownMenuContent>
      </DropdownMenu>
    </span>

    <span v-if="primary && primaryLast" data-row-interactive class="inline-flex shrink-0">
      <Button
        type="button"
        :variant="primary === 'cancel_transcription' ? 'outline' : 'default'"
        size="sm"
        class="cursor-pointer"
        :disabled="busy(primary)"
        @click="emit('action', primary)"
      >
        {{ actionLabel(primary) }}
      </Button>
    </span>
  </div>
</template>
