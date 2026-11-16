<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useI18n } from "vue-i18n";
import { ChevronDown } from "lucide-vue-next";

const { t } = useI18n();
// `hoverCue` grows the compact notch slightly and lifts it with a shadow so
// it reads as clickable/scrollable. Only the meeting controller opts in: the
// dictation indicator is transient and has nothing more to reveal.
const props = withDefaults(defineProps<{ interactive?: boolean; statusLabel?: string; syncCollapsePaint?: boolean; hoverCue?: boolean }>(), { interactive: true, syncCollapsePaint: false, hoverCue: false });
// Keep in sync with the `.notch-surface--hovered` width in <style>.
const HOVER_GROW_WIDTH = 10;
// Hover comes from a native AppKit tracking area, not CSS :hover: WKWebView
// stops updating :hover whenever another app is active, which is most of the
// time for a notch surface.
const nativeHover = ref(false);
const hovered = computed(() => props.hoverCue && props.interactive && nativeHover.value);
type Presentation = { style: "floating" | "notch"; expanded: boolean; compact_width: number; notch_width: number; notch_height: number; expanded_width?: number; expanded_height?: number; revision?: number; fresh?: boolean };
const emit = defineEmits<{ 'expanded-change': [expanded: boolean] }>();
const arrivalKey = ref(0);
const surface = ref<HTMLElement | null>(null);
const closing = ref(false);
// Ending a capture from the expanded panel (Finish/Discard) first shrinks the
// surface into the exact camera housing, so the native hide that follows is
// invisible instead of the panel vanishing mid-air.
const exiting = ref(false);
let exitDone: (() => void) | undefined;
let exitFallback: ReturnType<typeof setTimeout> | undefined;
let closeFallback: ReturnType<typeof setTimeout> | undefined;
const presentation = ref<Presentation>({ style: "floating", expanded: false, compact_width: 360, notch_width: 0, notch_height: 0 });
const headerWidth = computed(() => Math.max(presentation.value.compact_width, presentation.value.expanded_width ?? 400));
const cameraStyle = computed(() => ({
  '--camera-width': `${presentation.value.notch_width}px`,
  '--camera-height': `${presentation.value.notch_height}px`,
  '--compact-width': `${presentation.value.compact_width}px`,
  '--header-width': `${headerWidth.value}px`,
  '--expanded-body-height': `${(presentation.value.expanded_height ?? presentation.value.notch_height + 64) - presentation.value.notch_height}px`,
  '--arrival-width': `${presentation.value.notch_width + 96}px`,
  '--surface-width': `${exiting.value ? presentation.value.notch_width : props.interactive && presentation.value.expanded ? headerWidth.value : presentation.value.compact_width}px`,
  '--surface-height': `${exiting.value ? presentation.value.notch_height : props.interactive && presentation.value.expanded ? (presentation.value.expanded_height ?? presentation.value.notch_height + 64) : presentation.value.notch_height}px`,
}));
const notch = computed(() => presentation.value.style === "notch");
const expanded = computed(() => props.interactive && presentation.value.expanded);
const contentExpanded = computed(() => expanded.value || closing.value);
function clearCloseWait() { clearTimeout(closeFallback); closeFallback = undefined; }
watch(expanded, (value, previous) => {
  emit('expanded-change', value);
  clearCloseWait();
  closing.value = props.interactive && !value && previous && notch.value && !presentation.value.fresh;
  if (closing.value) {
    // Transition events can be absent with Reduce Motion or a cancelled CSS
    // transition. This client-side fallback starts after the event is received.
    closeFallback = setTimeout(() => settleCollapse(undefined, true), 360);
    void nextTick(() => { if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) settleCollapse(undefined, true); });
  }
});

async function paintedFrame() {
  await nextTick();
  if (typeof window.requestAnimationFrame !== 'function') return;
  await new Promise<void>(resolve => {
    window.requestAnimationFrame(() => window.requestAnimationFrame(() => resolve()));
  });
}

async function settleCollapse(event?: TransitionEvent, force = false) {
  if (disposed || !closing.value || expanded.value || !props.interactive) return;
  if (event && (event.target !== event.currentTarget || !['width', 'height'].includes(event.propertyName))) return;
  if (!force) {
    const bounds = surface.value?.getBoundingClientRect();
    const hoverWidth = hovered.value ? HOVER_GROW_WIDTH : 0;
    if (!bounds || Math.abs(bounds.width - presentation.value.compact_width - hoverWidth) > .5 || bounds.height > presentation.value.notch_height + 6.5) return;
  }
  closing.value = false;
  clearCloseWait();
  const revision = presentation.value.revision;
  const stillCurrent = () => !disposed && !expanded.value && presentation.value.revision === revision;
  try {
    if (props.syncCollapsePaint) await paintedFrame();
    if (!stillCurrent()) return;
    if (typeof revision === 'number') await invoke('finish_indicator_collapse', { revision });
    // Keep the stable header continuously visible. Synchronize the native
    // resize and its new viewport without a post-collapse opacity dip.
    if (props.syncCollapsePaint && stillCurrent()) await paintedFrame();
  } catch (error) {
    console.error('[murmur] collapse settlement failed', error);
  }
}
let disposed = false;
let unlisten: UnlistenFn | undefined;
let unlistenHover: UnlistenFn | undefined;
let pending = false;
let receivedEvent = false;
let wheelTotal = 0;
let lastWheel = 0;

function finishExit() {
  clearTimeout(exitFallback);
  exitFallback = undefined;
  const done = exitDone;
  exitDone = undefined;
  done?.();
}

/** Shrink into the camera housing; resolves once the motion has finished.
 * Floating surfaces, passive mirrors and Reduce Motion resolve immediately. */
function playExit(): Promise<void> {
  if (!notch.value || !props.interactive || disposed) return Promise.resolve();
  finishExit();
  clearCloseWait();
  closing.value = false;
  exiting.value = true;
  if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return Promise.resolve();
  return new Promise<void>(resolve => {
    exitDone = resolve;
    // Transition events can be lost (cancelled transition, hidden window).
    exitFallback = setTimeout(finishExit, 520);
  });
}

/** Restore the panel when ending the capture did not succeed. */
function cancelExit() {
  finishExit();
  exiting.value = false;
}

function onTransitionEnd(event: TransitionEvent) {
  if (exiting.value) {
    if (event.target === event.currentTarget && event.propertyName === 'width') finishExit();
    return;
  }
  void settleCollapse(event);
}

defineExpose({ playExit, cancelExit });

function accept(payload: Presentation) {
  if (payload && ["floating", "notch"].includes(payload.style) && typeof payload.expanded === "boolean") {
    if (payload.fresh && payload.revision !== presentation.value.revision) {
      clearCloseWait();
      cancelExit();
      closing.value = false;
      arrivalKey.value++;
    }
    presentation.value = payload;
  }
}

onMounted(async () => {
  if (props.hoverCue) {
    void listen<boolean>("murmur://indicator-hover", ({ payload }) => { nativeHover.value = payload === true; },
      { target: { kind: "WebviewWindow", label: getCurrentWindow().label } })
      .then(cleanup => { if (disposed) cleanup(); else unlistenHover = cleanup; })
      .catch(error => console.error("[murmur] notch hover unavailable", error));
  }
  try {
    const cleanup = await listen<Presentation>("murmur://indicator-presentation", ({ payload }) => {
      receivedEvent = true;
      if (!disposed) accept(payload);
    }, { target: { kind: "WebviewWindow", label: getCurrentWindow().label } });
    if (disposed) { cleanup(); return; }
    unlisten = cleanup;
    // The native snapshot covers a first-show event sent before this view
    // registered. Never let an older snapshot overwrite a newer event.
    const current = await invoke<Presentation>("get_indicator_presentation");
    if (!disposed && !receivedEvent) accept(current);
  } catch (error) {
    console.error("[murmur] recording presentation unavailable", error);
  }
});
onUnmounted(() => { disposed = true; clearCloseWait(); finishExit(); unlisten?.(); unlistenHover?.(); });

async function setExpanded(value: boolean) {
  if (!props.interactive || pending || exiting.value || !notch.value || expanded.value === value) return;
  pending = true;
  try { await invoke("set_indicator_expanded", { expanded: value }); }
  catch (error) { console.error("[murmur] cannot resize recording surface", error); }
  finally { pending = false; }
}

function collapse() { void setExpanded(false); }

function onWheel(event: WheelEvent) {
  // Only consume scrolling over this surface. Ignore horizontal scrolling,
  // tiny trackpad noise, and modifier gestures; never monitor globally.
  if (!props.interactive || exiting.value || event.ctrlKey || event.metaKey || event.altKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
  const now = Date.now();
  if (now - lastWheel > 250 || Math.sign(event.deltaY) !== Math.sign(wheelTotal)) wheelTotal = 0;
  lastWheel = now;
  wheelTotal += event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 100 : 1);
  if (Math.abs(wheelTotal) < 24) return;
  event.preventDefault();
  void setExpanded(wheelTotal > 0);
  wheelTotal = 0;
}
</script>

<template>
  <template v-if="!notch"><slot v-if="interactive" /></template>
  <section v-else :key="arrivalKey" ref="surface" class="notch-surface" @transitionend="onTransitionEnd" :style="cameraStyle" :class="{ 'notch-surface--expanded': expanded, 'notch-surface--exiting': exiting, 'notch-surface--passive': !interactive, 'notch-surface--hovered': hovered && !expanded }" @wheel="onWheel" @keydown.esc.stop="setExpanded(false)">
    <component
      :is="interactive ? 'button' : 'div'"
      :type="interactive ? 'button' : undefined" class="notch-surface__handle"
      :aria-expanded="interactive ? expanded : undefined"
      :aria-label="interactive ? t(expanded ? 'indicatorPosition.collapse' : 'indicatorPosition.expand') : undefined"
      :title="statusLabel"
      @click="setExpanded(!expanded)"
    >
      <span class="notch-surface__wing" aria-hidden="true"><slot name="leading" :expanded="contentExpanded"><span class="size-2 rounded-full bg-red-500" /></slot></span>
      <!-- Measured camera housing: no text, icons or controls in this column. -->
      <span class="notch-surface__camera" aria-hidden="true" />
      <span class="notch-surface__wing" aria-hidden="true"><slot name="trailing" :expanded="contentExpanded"><ChevronDown class="notch-surface__chevron size-3 text-white/60" :class="{ 'rotate-180': expanded }" /></slot></span>
    </component>
    <Transition name="notch-details">
      <div v-if="expanded" class="notch-surface__details">
        <slot name="expanded" :collapse="collapse"><slot /></slot>
        <slot name="details" />
      </div>
    </Transition>
  </section>
  <!-- Keep screen-reader-only content out of the animated clipping tree. -->
  <span v-if="notch" class="sr-only" role="status"><slot name="compact" /></span>
</template>

<style scoped>
.notch-surface {
  display: flex;
  flex-direction: column;
  align-self: flex-start;
  flex-shrink: 0;
  width: var(--surface-width);
  height: var(--surface-height);
  margin-inline: auto;
  overflow: hidden;
  color: white;
  background: #000;
  border-radius: 0 0 12px 12px;
  /* Same shadow geometry as the hover state so only its alpha animates. */
  box-shadow: 0 2px 6px rgb(0 0 0 / 0);
  animation: notch-arrive 340ms cubic-bezier(.22, 1.12, .36, 1);
  transition: width 320ms cubic-bezier(.22, 1.12, .36, 1), height 320ms cubic-bezier(.22, 1.12, .36, 1), border-radius 220ms ease, box-shadow 140ms ease;
}
.notch-surface:not(.notch-surface--expanded) {
  transition-duration: 200ms;
  transition-timing-function: cubic-bezier(.4, 0, .2, 1);
}
/* Hover cue: +10pt wide, +3pt tall and a soft drop shadow. The native window
   keeps 12pt of transparent headroom on each side and below (HOVER_SLACK /
   EDGE_HEADROOM in indicator_presentation.rs) so the shadow is never clipped. */
.notch-surface--hovered {
  width: calc(var(--compact-width) + 10px);
  height: calc(var(--camera-height) + 3px);
  box-shadow: 0 2px 6px rgb(0 0 0 / .45);
}
.notch-surface__handle {
  flex-shrink: 0;
  display: grid;
  grid-template-columns: minmax(0, 1fr) var(--camera-width) minmax(0, 1fr);
  grid-template-rows: var(--camera-height);
  align-self: center;
  width: var(--header-width);
  cursor: pointer;
}
.notch-surface--passive .notch-surface__handle { cursor: default; }
.notch-surface__wing {
  display: flex;
  align-items: center;
  justify-content: flex-start;
  min-width: 0;
  gap: 6px;
  padding: 0 16px;
  overflow: hidden;
  opacity: 1;
  transform: translateX(calc((var(--header-width) - var(--surface-width)) / 2));
  will-change: transform;
  transition: transform 320ms cubic-bezier(.22, 1.12, .36, 1);
}
.notch-surface__wing:nth-child(3) {
  justify-content: flex-end;
  transform: translateX(calc((var(--surface-width) - var(--header-width)) / 2));
}
.notch-surface:not(.notch-surface--expanded) .notch-surface__wing {
  transition-duration: 200ms;
  transition-timing-function: cubic-bezier(.4, 0, .2, 1);
}
.notch-surface--hovered .notch-surface__wing {
  transform: translateX(calc((var(--header-width) - var(--compact-width) - 10px) / 2));
}
.notch-surface--hovered .notch-surface__wing:nth-child(3) {
  transform: translateX(calc((var(--compact-width) + 10px - var(--header-width)) / 2));
}
.notch-surface__camera { width: var(--camera-width); height: var(--camera-height); }
.notch-surface__handle:focus-visible { outline: 2px solid #ffffff90; outline-offset: -3px; }
.notch-surface--expanded { border-radius: 0 0 24px 24px; }
.notch-surface__chevron { opacity: 0; transition: opacity 160ms ease, transform 160ms ease; }
.notch-surface__handle:hover .notch-surface__chevron,
.notch-surface__handle:focus-visible .notch-surface__chevron,
.notch-surface--expanded .notch-surface__chevron { opacity: 1; }
.notch-surface__details {
  display: flex;
  flex: none;
  height: var(--expanded-body-height);
  min-height: 0;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 12px 16px 14px;
}
/* Exit: no spring overshoot (it would dip inside the camera outline); the
   panel eases into the housing's exact size while its contents fade and the
   wings slide in with the edges. */
.notch-surface.notch-surface--exiting {
  border-radius: 0 0 8px 8px;
  box-shadow: 0 2px 6px rgb(0 0 0 / 0);
  transition: width 400ms cubic-bezier(.32, .72, 0, 1), height 340ms cubic-bezier(.32, .72, 0, 1), border-radius 340ms ease, box-shadow 140ms ease;
}
.notch-surface--exiting .notch-surface__wing {
  opacity: 0;
  transition: transform 400ms cubic-bezier(.32, .72, 0, 1), opacity 160ms ease;
}
.notch-surface--exiting .notch-surface__details {
  opacity: 0;
  transition: opacity 120ms ease;
}
.notch-details-enter-active { transition: opacity 200ms ease, transform 300ms cubic-bezier(.22, 1.12, .36, 1); }
.notch-details-leave-active { transition: opacity 120ms ease; }
.notch-details-enter-from { opacity: 0; transform: translateY(-7px); }
.notch-details-leave-to { opacity: 0; }
@keyframes notch-arrive { from { width: var(--arrival-width); } to { width: var(--surface-width); } }
@media (prefers-reduced-motion: reduce) {
  .notch-surface { animation: none; }
  .notch-surface, .notch-surface__wing, .notch-surface__chevron, .notch-details-enter-active, .notch-details-leave-active { transition: none; }
  .notch-surface.notch-surface--exiting, .notch-surface--exiting .notch-surface__wing, .notch-surface--exiting .notch-surface__details { transition: none; }
}
</style>
