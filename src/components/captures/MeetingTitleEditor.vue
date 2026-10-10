<script setup lang="ts">
import { Button } from "@/components/ui/button";
import { IconTooltip } from "@/components/ui/tooltip";
import { useCaptures } from "@/composables/useCaptures";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { captureErrorCopy, type CaptureSummary } from "@/lib/captures";
import { Check, LoaderCircle, Pencil } from "lucide-vue-next";
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from "vue";
import { RouterLink } from "vue-router";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const props = withDefaults(defineProps<{
  captureId: number;
  title: string;
  to?: string;
  variant?: "row" | "active" | "heading";
  as?: "h1" | "span";
}>(), {
  to: undefined,
  variant: "row",
  as: "span",
});

const emit = defineEmits<{
  renamed: [summary: CaptureSummary];
}>();

const captures = useCaptures();
const meeting = useMeetingCapture();
const root = ref<HTMLElement | null>(null);
const input = ref<HTMLInputElement | null>(null);
const editing = ref(false);
const draft = ref(props.title);
const localError = ref<string | null>(null);
const submitted = ref(false);
const errorId = `meeting-title-error-${useId()}`;
const renameState = computed(() => captures.stateFor(props.captureId, "rename"));
const saving = computed(() => renameState.value.loading);
const errorMessage = computed(() => {
  if (localError.value) return localError.value;
  return submitted.value && renameState.value.error
    ? captureErrorCopy(renameState.value.error, t)
    : null;
});

const titleClass = computed(() => {
  if (props.variant === "heading") {
    return "min-w-0 break-words text-xl font-semibold text-balance";
  }
  if (props.variant === "active") {
    return "min-w-0 truncate text-sm font-medium";
  }
  return "min-w-0 truncate font-medium";
});

const inputClass = computed(() => {
  const typography = props.variant === "heading"
    ? "h-8 text-xl font-semibold"
    : props.variant === "active"
      ? "h-7 text-sm font-medium"
      : "h-7 font-medium";
  return `${typography} min-w-0 flex-1 cursor-text rounded-sm border-0 bg-muted/45 px-1 py-0 text-foreground shadow-none outline-none ring-1 ring-ring/50 transition-[background-color,box-shadow] duration-150 focus:bg-background focus:ring-2 focus:ring-ring`;
});

const editorHeightClass = computed(() => props.variant === "heading" ? "min-h-8" : "min-h-7");
const iconButtonClass = computed(() => props.variant === "heading" ? "size-8" : "size-7");

let restoreFocus = false;

function handleOutsidePointerDown(event: PointerEvent): void {
  if (!editing.value || !(event.target instanceof Node) || root.value?.contains(event.target)) return;
  restoreFocus = false;
  void save();
}

function restoreInputFocus(): void {
  if (!restoreFocus || !editing.value || saving.value || !document.hasFocus()) return;
  restoreFocus = false;
  void focusInput();
}

function handleInputBlur(event: FocusEvent): void {
  const nextTarget = event.relatedTarget;
  if (nextTarget instanceof Node && root.value?.contains(nextTarget)) return;
  if (nextTarget === null) {
    restoreFocus = true;
    window.setTimeout(restoreInputFocus, 0);
    return;
  }
  void save();
}

watch(editing, (active) => {
  if (active) {
    document.addEventListener("pointerdown", handleOutsidePointerDown, true);
    window.addEventListener("focus", restoreInputFocus);
    return;
  }
  document.removeEventListener("pointerdown", handleOutsidePointerDown, true);
  window.removeEventListener("focus", restoreInputFocus);
  restoreFocus = false;
}, { flush: "sync" });

watch(() => props.title, (title) => {
  if (!editing.value) draft.value = title;
});

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", handleOutsidePointerDown, true);
  window.removeEventListener("focus", restoreInputFocus);
});

async function focusInput(select = false): Promise<void> {
  await nextTick();
  input.value?.focus();
  if (select) input.value?.select();
}

function beginEditing(): void {
  draft.value = props.title;
  localError.value = null;
  submitted.value = false;
  editing.value = true;
  void focusInput(true);
}

function cancelEditing(): void {
  if (saving.value) return;
  draft.value = props.title;
  localError.value = null;
  submitted.value = false;
  editing.value = false;
}

async function save(): Promise<void> {
  if (!editing.value || saving.value) return;
  const title = draft.value.trim();
  if (!title) {
    localError.value = t('meetingTitle.required');
    await focusInput();
    return;
  }
  if (title === props.title) {
    cancelEditing();
    return;
  }

  localError.value = null;
  submitted.value = true;
  const summary = await captures.rename(props.captureId, title);
  if (!summary) {
    await focusInput();
    return;
  }

  meeting.syncActiveCapture(summary);
  editing.value = false;
  submitted.value = false;
  emit("renamed", summary);
}
</script>

<template>
  <div
    ref="root"
    :data-row-interactive="editing ? 'true' : undefined"
    :class="[editing ? 'w-full' : 'w-fit max-w-full', 'group/title flex min-w-0 flex-col gap-1']"
  >
    <div :class="[editorHeightClass, 'grid min-w-0']">
      <Transition name="meeting-title-swap">
        <div
          v-if="editing"
          key="editor"
          class="col-start-1 row-start-1 flex min-w-0 items-center gap-1"
        >
          <label :for="`meeting-title-${captureId}`" class="sr-only">{{ t('meetingTitle.label') }}</label>
          <input
            :id="`meeting-title-${captureId}`"
            ref="input"
            v-model="draft"
            type="text"
            maxlength="160"
            autocomplete="off"
            :class="inputClass"
            :aria-invalid="errorMessage ? 'true' : undefined"
            :aria-describedby="errorMessage ? errorId : undefined"
            @keydown.enter.prevent="save"
            @keydown.esc.prevent="cancelEditing"
            @blur="handleInputBlur"
          />
          <IconTooltip :label="t('meetingTitle.saveHint')">
            <Button
              type="button"
              variant="secondary"
              size="icon"
              :class="[iconButtonClass, 'shrink-0 cursor-pointer shadow-sm ring-1 ring-border/80']"
              :disabled="saving"
              :aria-label="t('meetingTitle.save')"
              aria-keyshortcuts="Enter"
              @mousedown.prevent
              @click="save"
            >
              <LoaderCircle v-if="saving" class="size-3.5 animate-spin" aria-hidden="true" />
              <Check v-else class="size-3.5" aria-hidden="true" />
            </Button>
          </IconTooltip>
        </div>

        <div
          v-else
          key="title"
          class="col-start-1 row-start-1 flex min-w-0 items-center gap-1"
        >
          <RouterLink
            v-if="to"
            :to="to"
            :class="[titleClass, 'rounded-sm px-1 py-0.5 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring']"
            :aria-label="t('meetingTitle.open', { title })"
          >
            {{ title }}
          </RouterLink>
          <component :is="as" v-else :class="titleClass">
            {{ title }}
          </component>
          <IconTooltip :label="t('meetingTitle.rename')">
            <Button
              type="button"
              variant="secondary"
              size="icon"
              :class="[iconButtonClass, 'shrink-0 cursor-pointer opacity-0 shadow-sm ring-1 ring-border/80 transition-[opacity,background-color,color,box-shadow] group-hover/title:opacity-100 group-focus-within/title:opacity-100']"
              :aria-label="t('meetingTitle.rename')"
              @click="beginEditing"
            >
              <Pencil class="size-3.5" aria-hidden="true" />
            </Button>
          </IconTooltip>
        </div>
      </Transition>
    </div>

    <p v-if="errorMessage" :id="errorId" class="px-1 text-xs text-destructive" role="alert">
      {{ errorMessage }}
    </p>
  </div>
</template>

<style scoped>
.meeting-title-swap-enter-active,
.meeting-title-swap-leave-active {
  transform-origin: left center;
  transition:
    opacity 130ms ease,
    transform 170ms cubic-bezier(0.22, 1, 0.36, 1);
}

.meeting-title-swap-enter-from {
  opacity: 0;
  transform: scaleX(0.92);
}

.meeting-title-swap-leave-to {
  opacity: 0;
  transform: scaleX(0.98);
}

@media (prefers-reduced-motion: reduce) {
  .meeting-title-swap-enter-active,
  .meeting-title-swap-leave-active {
    transition-duration: 0.01ms;
  }
}
</style>
