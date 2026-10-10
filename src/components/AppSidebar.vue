<script setup lang="ts">
import type { SidebarProps } from "@/components/ui/sidebar";
import type { LucideIcon } from "lucide-vue-next";
import {
  BookA,
  Circle,
  Clock,
  Home,
  MessagesSquare,
  Pause,
  Play,
  Settings2,
  Square,
} from "lucide-vue-next";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { useI18n } from "vue-i18n";
import { computed, onUnmounted, ref, watch } from "vue";

import UpdateCard from "@/components/sidebar/UpdateCard.vue";
import MicAudioLines from "@/components/icons/MicAudioLines.vue";
import { Button } from "@/components/ui/button";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { formatMeetingDuration } from "@/lib/format";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";

const props = withDefaults(defineProps<SidebarProps>(), { variant: "inset" });
const { t } = useI18n();
const route = useRoute();
const router = useRouter();
const { state: sidebarState, isMobile } = useSidebar();
const iconRail = computed(() => sidebarState.value === "collapsed" && !isMobile.value);
const railControlsVisible = ref(false);
const railControlsPaused = ref(false);
const {
  action,
  activeCapture,
  activeDurationMs,
  controls,
  pause,
  requestStartFlow,
  resume,
  stop,
} = useMeetingCapture();
watch(activeCapture, (capture) => {
  if (capture) railControlsPaused.value = capture.status === "paused";
}, { immediate: true });

let revealGeneration = 0;
watch([activeCapture, iconRail], ([capture, inRail]) => {
  const generation = ++revealGeneration;
  if (!capture || !inRail) {
    railControlsVisible.value = false;
    return;
  }
  if (railControlsVisible.value) return;

  // Let the newly mounted controls paint closed before transitioning open.
  requestAnimationFrame(() => requestAnimationFrame(() => {
    if (generation === revealGeneration && activeCapture.value && iconRail.value) {
      railControlsVisible.value = true;
    }
  }));
}, { immediate: true, flush: "post" });
onUnmounted(() => { revealGeneration++; });

interface NavItem {
  titleKey: string;
  to: string;
  icon: LucideIcon;
}

const navigation: NavItem[] = [
  { titleKey: "navigation.home", to: "/", icon: Home },
  { titleKey: "navigation.dictations", to: "/history", icon: Clock },
  { titleKey: "navigation.meetings", to: "/captures", icon: MessagesSquare },
  { titleKey: "navigation.dictionary", to: "/dictionary", icon: BookA },
];

const activeNavClass =
  "font-normal text-sidebar-foreground/75 data-active:font-medium data-active:text-sidebar-accent-foreground";

function isNavActive(to: string): boolean {
  if (to === "/captures") {
    return route.path === to || route.path.startsWith(`${to}/`);
  }
  if (to === "/settings/general") {
    return route.path.startsWith("/settings/");
  }
  return route.path === to;
}

function openActiveMeeting(): void {
  if (activeCapture.value) void router.push(`/captures/${activeCapture.value.id}`);
}

async function pauseOrResumeMeeting(): Promise<void> {
  if (activeCapture.value?.status === "paused") await resume();
  else await pause();
}

async function finishMeeting(): Promise<void> {
  await stop();
}
</script>

<template>
  <Sidebar v-bind="props">
    <SidebarHeader class="gap-4 pb-3 group-data-[collapsible=icon]:gap-2 group-data-[collapsible=icon]:pb-0">
      <div
        class="flex h-12 w-full items-center gap-3 px-2 text-sidebar-foreground group-data-[collapsible=icon]:justify-center group-data-[collapsible=icon]:px-0"
        role="img"
        aria-label="Murmur"
      >
        <span class="murmur-brand-mark size-10 shrink-0 group-data-[collapsible=icon]:size-8" aria-hidden="true" />
        <span class="murmur-brand-name truncate text-xl leading-none group-data-[collapsible=icon]:hidden">
          Murmur
        </span>
      </div>

      <div v-if="iconRail" class="flex flex-col items-center gap-2">
        <SidebarMenuButton
          v-if="!activeCapture"
          :tooltip="t('navigation.recordMeeting')"
          :aria-label="t('navigation.recordMeeting')"
          class="justify-center bg-sidebar-primary text-sidebar-primary-foreground hover:bg-sidebar-primary/90"
          @click="requestStartFlow"
        >
          <MicAudioLines aria-hidden="true" />
        </SidebarMenuButton>
        <SidebarMenuButton
          v-else
          :tooltip="t('navigation.showMeetingControls')"
          :aria-label="t('navigation.showMeetingControls')"
          aria-controls="rail-meeting-controls"
          :aria-expanded="railControlsVisible"
          class="justify-center bg-sidebar-primary text-sidebar-primary-foreground hover:bg-sidebar-primary/90"
          @click="railControlsVisible = true"
        >
          <span
            class="relative size-2.5 shrink-0 rounded-full"
            :class="activeCapture.status === 'paused' ? 'bg-amber-500' : 'recording-pulse-dot bg-red-500'"
            aria-hidden="true"
          />
        </SidebarMenuButton>
        <div
          id="rail-meeting-controls"
          class="rail-controls-grid w-full"
          :class="{ 'rail-controls-open': railControlsVisible }"
          :aria-hidden="!railControlsVisible"
          :inert="!railControlsVisible"
        >
          <div class="rail-controls-inner min-h-0 overflow-hidden">
            <div class="rail-control-buttons flex flex-col gap-1 pt-1">
              <SidebarMenuButton
                :tooltip="t(railControlsPaused ? 'meetingController.resumeTooltip' : 'meetingController.pauseTooltip')"
                :aria-label="t(railControlsPaused ? 'meetingController.resumeLabel' : 'meetingController.pauseLabel')"
                :disabled="!activeCapture || action !== null || !(controls.includes('pause') || controls.includes('resume'))"
                @click="pauseOrResumeMeeting"
              >
                <Play v-if="railControlsPaused" class="fill-current" aria-hidden="true" />
                <Pause v-else class="fill-current" aria-hidden="true" />
              </SidebarMenuButton>
              <SidebarMenuButton
                :tooltip="t('meetingController.stopTooltip')"
                :aria-label="t('meetingController.stopLabel')"
                :disabled="!activeCapture || action !== null || !controls.includes('stop')"
                @click="finishMeeting"
              >
                <Square class="fill-current" aria-hidden="true" />
              </SidebarMenuButton>
            </div>
          </div>
        </div>
        <div class="h-px w-full bg-sidebar-border" role="separator" />
      </div>

      <Button
        v-else-if="!activeCapture"
        size="lg"
        variant="sidebar"
        class="w-full cursor-pointer"
        @click="requestStartFlow"
      >
        {{ t('navigation.recordMeeting') }}
      </Button>

      <div
        v-else
        class="flex flex-col gap-2 rounded-lg border border-sidebar-border bg-sidebar-hover p-2"
      >
        <button
          type="button"
          class="flex w-full items-center gap-1.5 rounded-md text-left text-xs outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
          :aria-label="t('navigation.openMeeting', { status: t(activeCapture.status === 'paused' ? 'navigation.paused' : 'navigation.active'), duration: formatMeetingDuration(activeDurationMs) })"
          @click="openActiveMeeting"
        >
          <Circle
            class="size-2 shrink-0 fill-red-500 text-red-500"
            :class="activeCapture.status === 'paused' ? 'opacity-60' : 'animate-pulse'"
            aria-hidden="true"
          />
          <span class="truncate font-medium">
            {{ t(activeCapture.status === 'paused' ? 'navigation.meetingPaused' : 'navigation.meetingActive') }}
          </span>
          <span class="ml-auto shrink-0 tabular-nums text-sidebar-foreground/65">
            {{ formatMeetingDuration(activeDurationMs) }}
          </span>
        </button>
        <div class="grid grid-cols-2 gap-1.5">
          <Button
            variant="sidebarOutline"
            size="sm"
            :disabled="action !== null || !(controls.includes('pause') || controls.includes('resume'))"
            @click="pauseOrResumeMeeting"
          >
            <Play v-if="activeCapture.status === 'paused'" class="size-3.5 fill-current" aria-hidden="true" />
            <Pause v-else class="size-3.5 fill-current" aria-hidden="true" />
            {{ t(activeCapture.status === 'paused' ? 'navigation.resume' : 'navigation.pause') }}
          </Button>
          <Button
            variant="sidebar"
            size="sm"
            :disabled="action !== null || !controls.includes('stop')"
            @click="finishMeeting"
          >
            <Square class="size-3 fill-current" aria-hidden="true" />
            {{ t('meetingController.stop') }}
          </Button>
        </div>
      </div>
    </SidebarHeader>

    <SidebarContent class="pt-3 group-data-[collapsible=icon]:pt-2">
      <SidebarGroup>
        <SidebarMenu class="gap-0.5">
          <template v-for="item in navigation" :key="item.to">
            <SidebarMenuItem>
              <SidebarMenuButton
                as-child
                :tooltip="t(item.titleKey)"
                :is-active="isNavActive(item.to)"
                :class="activeNavClass"
              >
                <RouterLink :to="item.to">
                  <component :is="item.icon" />
                  <span>{{ t(item.titleKey) }}</span>
                </RouterLink>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </template>
        </SidebarMenu>
      </SidebarGroup>
    </SidebarContent>

    <SidebarFooter>
      <UpdateCard />
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton
            as-child
            :tooltip="t('navigation.settings')"
            :is-active="isNavActive('/settings/general')"
            :class="activeNavClass"
          >
            <RouterLink to="/settings/general">
              <Settings2 />
              <span>{{ t('navigation.settings') }}</span>
            </RouterLink>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarFooter>
  </Sidebar>
</template>

<style scoped>
.recording-pulse-dot::after {
  content: "";
  position: absolute;
  inset: 0;
  border: 1px solid var(--color-red-500);
  border-radius: 50%;
  animation: recording-pulse 2s ease-out infinite;
}

@keyframes recording-pulse {
  0% { opacity: 0.85; transform: scale(1); }
  75%, 100% { opacity: 0; transform: scale(2.7); }
}

.rail-controls-grid {
  display: grid;
  grid-template-rows: 0fr;
  transition: grid-template-rows 180ms ease;
}

.rail-controls-grid.rail-controls-open {
  grid-template-rows: 1fr;
}

.rail-control-buttons {
  opacity: 0;
  transform: translateY(-6px);
  transition: opacity 180ms ease, transform 180ms ease;
}

.rail-controls-open .rail-control-buttons {
  opacity: 1;
  transform: translateY(0);
}

@media (prefers-reduced-motion: reduce) {
  .rail-controls-grid, .rail-control-buttons {
    transition: none;
  }
  .recording-pulse-dot::after {
    animation: none;
    opacity: 0;
  }
}

.murmur-brand-mark {
  background-color: currentColor;
  -webkit-mask: url("@/assets/brand/murmur-logo-black.svg") center / contain no-repeat;
  mask: url("@/assets/brand/murmur-logo-black.svg") center / contain no-repeat;
}

.murmur-brand-name {
  font-family: "Google Sans Flex", sans-serif;
  font-weight: 500;
}
</style>
