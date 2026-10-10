<script setup lang="ts">
import { usePreferredReducedMotion } from "@vueuse/core";
import { onBeforeUnmount, onMounted, ref, watch } from "vue";

const props = defineProps<{ fromOnboarding: boolean }>();
const surface = ref<HTMLElement | null>(null);
const preferredMotion = usePreferredReducedMotion();
let animation: Animation | undefined;

function cancelMotion(): void {
  animation?.cancel();
  animation = undefined;
}

onMounted(() => {
  if (preferredMotion.value === "reduce" || typeof surface.value?.animate !== "function") return;
  // Mount-only: navigation, window focus, and tray reopening never replay this.
  // No retained onboarding screen, delays, or interaction/focus blocking.
  const frames: Keyframe[] = props.fromOnboarding
    ? [{ opacity: 0, transform: "translateY(4px)" }, { opacity: 1, transform: "translateY(0)" }]
    : [{ opacity: 0 }, { opacity: 1 }];
  animation = surface.value.animate(frames, {
    id: "murmur-app-enter",
    duration: props.fromOnboarding ? 300 : 180,
    easing: "cubic-bezier(0.2, 0.7, 0.2, 1)",
  });
});

watch(preferredMotion, (preference) => { if (preference === "reduce") cancelMotion(); });
onBeforeUnmount(cancelMotion);
</script>

<template>
  <div class="app-shell-background grid h-svh overflow-hidden">
    <!-- Keep the saved shell background stationary beneath the incoming UI,
         avoiding a white flash or viewport overflow during the small rise. -->
    <div ref="surface" class="min-h-0" data-testid="app-entrance-surface">
      <slot />
    </div>
  </div>
</template>
