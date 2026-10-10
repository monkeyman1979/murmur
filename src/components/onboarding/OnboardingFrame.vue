<script setup lang="ts">
import OnboardingBrandMark from "@/components/onboarding/OnboardingBrandMark.vue";
import OnboardingProgress from "@/components/onboarding/OnboardingProgress.vue";
import { Button } from "@/components/ui/button";
import { usePreferredReducedMotion } from "@vueuse/core";
import { ArrowLeft } from "lucide-vue-next";
import { FocusScope } from "reka-ui";
import { onBeforeUnmount, ref, watch } from "vue";

const props = withDefaults(defineProps<{
  title: string;
  subtitle: string;
  stepLabel: string;
  currentStep: number;
  totalSteps: number;
  primaryLabel: string;
  backLabel?: string;
  primaryDisabled?: boolean;
  showBack?: boolean;
  centered?: boolean;
  centerWelcome?: boolean;
  compact?: boolean;
  bleedRight?: boolean;
  focusDemo?: boolean;
  motionKey?: string;
}>(), {
  backLabel: "Back",
  primaryDisabled: false,
  showBack: false,
  centered: false,
  centerWelcome: false,
  compact: false,
  bleedRight: false,
  focusDemo: false,
  motionKey: "",
});

const emit = defineEmits<{ back: []; primary: [] }>();
const heading = ref<HTMLHeadingElement | null>(null);
const content = ref<HTMLDivElement | null>(null);
const preferredMotion = usePreferredReducedMotion();
let animations: Animation[] = [];

function cancelMotion(): void {
  for (const animation of animations) animation.cancel();
  animations = [];
}

function animateScreen(): void {
  cancelMotion();
  if (preferredMotion.value === "reduce" || !content.value) return;
  // Animate the new content in place, not a keyed/retained outgoing screen.
  // Microphone and demo ownership therefore keep their normal Vue lifecycles.
  // Each section moves within the content padding, avoiding scroll-height jumps.
  for (const section of content.value.children) {
    if (typeof section.animate !== "function") continue;
    animations.push(section.animate([
      { opacity: 0, transform: "translateY(8px)" },
      { opacity: 1, transform: "translateY(0)" },
    ], { id: "onboarding-screen-enter", duration: 350, easing: "cubic-bezier(0.4, 0, 0.2, 1)" }));
  }
}

watch(() => [props.currentStep, props.motionKey], animateScreen, { flush: "post" });
watch(preferredMotion, (preference) => { if (preference === "reduce") cancelMotion(); });
onBeforeUnmount(cancelMotion);

function focusHeading(): void {
  // The live shortcut lesson owns focus in its insertion field.
  if (!props.focusDemo) heading.value?.focus();
}

watch(() => props.currentStep, focusHeading, { flush: "post" });
</script>

<template>
  <FocusScope as-child trapped loop @mount-auto-focus.prevent="focusHeading" @unmount-auto-focus.prevent>
    <div
      class="onboarding-flow dark fixed inset-0 z-40 grid grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden bg-background text-foreground"
      role="dialog"
      aria-modal="true"
      aria-labelledby="onboarding-title"
      @keydown.esc.prevent.stop
    >
      <header class="relative flex h-24 items-end pb-3">
        <div v-if="currentStep > 1 && currentStep < totalSteps" class="flex items-center gap-2 text-sm text-muted-foreground">
          <OnboardingBrandMark class="size-7" />
          <span aria-hidden="true">Murmur</span>
        </div>
      </header>

      <main class="relative min-h-0 overflow-y-auto overscroll-contain" :class="{ 'onboarding-main-bleed': bleedRight }">
        <div ref="content" class="onboarding-content" :class="[centered ? 'onboarding-centered' : 'onboarding-split', { 'onboarding-content-compact': compact, 'onboarding-welcome': centerWelcome }]">
          <section class="flex min-w-0 flex-col gap-5" :class="centered ? 'items-center text-center' : ''">
            <slot name="identity">
              <OnboardingBrandMark v-if="centered" class="size-36" />
            </slot>
            <slot name="eyebrow" />
            <h1
              id="onboarding-title"
              ref="heading"
              tabindex="-1"
              class="max-w-[15ch] text-balance text-[clamp(2.25rem,calc(1.5rem+2.5vw),4rem)] font-semibold leading-[1.1] tracking-[-0.045em] outline-none"
            >{{ title }}</h1>
            <p v-if="subtitle" class="w-[min(100%,55ch)] text-balance text-base leading-6 text-muted-foreground">{{ subtitle }}</p>
            <slot name="left" />
          </section>
          <section class="flex min-w-0 flex-col justify-center" :class="centered ? 'w-[min(100%,36rem)]' : ''">
            <slot name="right" />
          </section>
        </div>
      </main>

      <footer class="relative flex flex-wrap items-center justify-between gap-x-6 gap-y-4 py-5">
        <div class="flex items-center gap-5">
          <Button v-if="showBack" type="button" variant="outline" size="icon" class="size-10 rounded-full" :aria-label="backLabel" @click="emit('back')">
            <ArrowLeft class="size-4" aria-hidden="true" />
          </Button>
          <OnboardingProgress :current="currentStep" :total="totalSteps" :label="stepLabel" />
        </div>
        <div class="ml-auto flex min-w-0 max-w-full flex-wrap items-center justify-end gap-5">
          <slot name="secondary" />
          <Button type="button" class="h-auto min-h-12 w-fit max-w-full whitespace-normal rounded-full px-7 py-2 text-base font-semibold transition-[background-color,color,border-color,box-shadow,opacity]" :disabled="primaryDisabled" data-testid="onboarding-primary" @click="emit('primary')">
            {{ primaryLabel }}
          </Button>
        </div>
      </footer>
    </div>
  </FocusScope>
</template>

<style scoped>
.onboarding-flow {
  color-scheme: dark;
  --onboarding-gutter: clamp(1.25rem, calc(0.75rem + 2vw), 3rem);
  padding-inline: var(--onboarding-gutter);
  background-image: linear-gradient(color-mix(in srgb, var(--background) 93%, transparent), color-mix(in srgb, var(--background) 97%, transparent)), var(--app-shell-background);
}

.onboarding-content {
  width: min(100%, 78rem);
  min-height: 100%;
  margin-inline: auto;
  padding-block: clamp(1.5rem, calc(1rem + 2vw), 3rem);
  gap: clamp(2rem, calc(1rem + 3vw), 4rem);
  align-items: center;
}

.onboarding-content-compact {
  padding-block: 1rem;
}

.onboarding-content-compact > section:first-child {
  gap: 2.5rem;
}

.onboarding-main-bleed {
  margin-inline-end: calc(-1 * var(--onboarding-gutter));
  overflow-x: hidden;
}

.onboarding-main-bleed .onboarding-content {
  width: 100%;
}

.onboarding-split {
  display: flex;
  flex-wrap: wrap;
}

.onboarding-split > section:first-child {
  flex: 0.72 1 18rem;
}

.onboarding-split > section:last-child {
  flex: 1.28 1 26rem;
}

.onboarding-centered {
  display: flex;
  flex-direction: column;
  justify-content: flex-start;
  padding-block-start: clamp(2rem, calc(1rem + 8vh), 6rem);
  gap: 2rem;
}

/* Ready keeps its top anchor; Welcome centers the full stack in the space
   above the footer and grows/scrolls normally when the window is short. */
.onboarding-centered > section:first-child {
  width: 100%;
  gap: 1.5rem;
}

.onboarding-centered.onboarding-welcome {
  justify-content: center;
  padding-block: clamp(1.5rem, calc(1rem + 2vw), 3rem);
}

.onboarding-centered h1 {
  max-width: 20ch;
}

.onboarding-centered > section:empty {
  display: none;
}

.onboarding-flow :deep(:focus-visible:not(h1)) {
  outline: 2px solid var(--primary);
  outline-offset: 4px;
}

@media (prefers-reduced-motion: reduce) {
  .onboarding-flow :deep(*) {
    transition-duration: 0.01ms !important;
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
  }
}

@media (prefers-reduced-transparency: reduce) {
  .onboarding-flow {
    background-image: none;
  }

  .onboarding-flow :deep(*) {
    backdrop-filter: none !important;
  }
}
</style>
