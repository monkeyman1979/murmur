<script setup lang="ts">
import {
  bindingSegments,
  CG_FLAG_ALTERNATE,
  CG_FLAG_COMMAND,
  CG_FLAG_CONTROL,
  CG_FLAG_SHIFT,
  formatKeycodeName,
  KEYCODE_FROM_EVENT_CODE,
  MODIFIER_CODE_TO_DEVICE_MASK,
  type HotkeyBinding,
} from "@/lib/hotkey";
import { computed, onUnmounted, ref } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const props = defineProps<{ modelValue: HotkeyBinding }>();
const emit = defineEmits<{ (e: "update:modelValue", value: HotkeyBinding): void }>();

const recording = ref(false);

interface TriggerSnapshot {
  keycode: number;
  required_flags: number;
}

// Ordered list of modifier codes currently held. Index 0 is the first-pressed
// modifier, which we surface as the primary intent when committing a
// ModifierOnly binding.
const heldModifierCodes = ref<string[]>([]);
// Survives modifier keyups so release order doesn't change the committed
// ModifierOnly device_mask.
const firstModifierCode = ref<string | null>(null);
// Non-modifier key captured during the session. Overwritten if the user
// presses another non-modifier (so mispresses can be corrected without
// restarting the recorder).
const triggerSnapshot = ref<TriggerSnapshot | null>(null);

function buildRequiredFlags(event: KeyboardEvent): number {
  let flags = 0;
  if (event.shiftKey) flags |= CG_FLAG_SHIFT;
  if (event.ctrlKey) flags |= CG_FLAG_CONTROL;
  if (event.altKey) flags |= CG_FLAG_ALTERNATE;
  if (event.metaKey) flags |= CG_FLAG_COMMAND;
  return flags;
}

// Derive which CG modifier flags are active from the currently-held codes.
function activeFlagsFromHeld(): number {
  let flags = 0;
  for (const code of heldModifierCodes.value) {
    if (code === "ControlLeft" || code === "ControlRight") flags |= CG_FLAG_CONTROL;
    if (code === "AltLeft" || code === "AltRight") flags |= CG_FLAG_ALTERNATE;
    if (code === "ShiftLeft" || code === "ShiftRight") flags |= CG_FLAG_SHIFT;
    if (code === "MetaLeft" || code === "MetaRight") flags |= CG_FLAG_COMMAND;
  }
  return flags;
}

// Live segments while recording, in canonical Mac order. Stays empty until
// the user presses something so the placeholder copy can render.
const recordingSegments = computed<string[]>(() => {
  const flags = activeFlagsFromHeld();
  const out: string[] = [];
  if (flags & CG_FLAG_CONTROL) out.push("⌃");
  if (flags & CG_FLAG_ALTERNATE) out.push("⌥");
  if (flags & CG_FLAG_SHIFT) out.push("⇧");
  if (flags & CG_FLAG_COMMAND) out.push("⌘");
  if (triggerSnapshot.value !== null) {
    out.push(
      formatKeycodeName(triggerSnapshot.value.keycode, t) ??
        `#${triggerSnapshot.value.keycode}`,
    );
  }
  return out;
});

const committedSegments = computed<string[]>(() => bindingSegments(props.modelValue, t));

function finalize(binding: HotkeyBinding): void {
  emit("update:modelValue", binding);
  stopRecording();
}

function stopRecording(): void {
  recording.value = false;
  heldModifierCodes.value = [];
  firstModifierCode.value = null;
  triggerSnapshot.value = null;
  window.removeEventListener("keydown", onKeyDown, { capture: true });
  window.removeEventListener("keyup", onKeyUp, { capture: true });
}

function onKeyDown(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();

  if (event.code === "Escape") {
    // Cancel without committing anything.
    stopRecording();
    return;
  }

  if (event.code in MODIFIER_CODE_TO_DEVICE_MASK) {
    if (!heldModifierCodes.value.includes(event.code)) {
      heldModifierCodes.value = [...heldModifierCodes.value, event.code];
      if (firstModifierCode.value === null) firstModifierCode.value = event.code;
    }
    return;
  }

  if (event.code in KEYCODE_FROM_EVENT_CODE) {
    triggerSnapshot.value = {
      keycode: KEYCODE_FROM_EVENT_CODE[event.code],
      required_flags: buildRequiredFlags(event),
    };
    return;
  }

  // Unsupported key (NumLock, PrintScreen, etc.) — ignore and keep recording.
}

function onKeyUp(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();

  const wasModifier = event.code in MODIFIER_CODE_TO_DEVICE_MASK;
  if (wasModifier) {
    heldModifierCodes.value = heldModifierCodes.value.filter((c) => c !== event.code);
  }

  // Only commit once ALL keys are released.
  if (heldModifierCodes.value.length > 0) return;

  if (triggerSnapshot.value !== null) {
    const snap = triggerSnapshot.value;
    finalize({
      kind: "key_combo",
      keycode: snap.keycode,
      required_flags: snap.required_flags,
    });
    return;
  }

  // No non-modifier pressed — commit as ModifierOnly using the first
  // modifier pressed (primary intent), regardless of release order.
  if (wasModifier && firstModifierCode.value !== null) {
    finalize({
      kind: "modifier_only",
      device_mask: MODIFIER_CODE_TO_DEVICE_MASK[firstModifierCode.value],
    });
  }
}

function startRecording(): void {
  if (recording.value) return;
  heldModifierCodes.value = [];
  firstModifierCode.value = null;
  triggerSnapshot.value = null;
  recording.value = true;
  // capture: true so we intercept before Vue's event delegation and the
  // browser's own shortcut handling (e.g. ⌘W closing the window).
  window.addEventListener("keydown", onKeyDown, { capture: true });
  window.addEventListener("keyup", onKeyUp, { capture: true });
}

onUnmounted(() => {
  if (recording.value) stopRecording();
});
</script>

<template>
  <div
    role="button"
    tabindex="0"
    :aria-label="t('hotkeyRecorder.change')"
    :aria-pressed="recording"
    class="group inline-flex cursor-pointer select-none items-center gap-1.5 rounded-md px-2 py-1.5 transition-colors focus:outline-none focus:ring-1 focus:ring-ring"
    :class="recording ? 'bg-muted/50' : 'bg-background hover:bg-muted/50'"
    @click="startRecording"
    @keydown.enter.prevent="startRecording"
    @keydown.space.prevent="startRecording"
    @blur="recording ? stopRecording() : undefined"
  >
    <template v-if="recording">
      <span
        v-if="recordingSegments.length === 0"
        class="px-2 text-sm text-muted-foreground"
        aria-live="polite"
        aria-atomic="true"
      >
        {{ t('hotkeyRecorder.press') }}
      </span>
      <template v-else>
        <template v-for="(seg, i) in recordingSegments" :key="`rec-${i}`">
          <span v-if="i > 0" class="px-0.5 text-sm text-muted-foreground">+</span>
          <kbd
            class="inline-flex h-8 min-w-[2rem] items-center justify-center rounded border border-border bg-muted px-2 font-mono text-base font-medium"
          >
            {{ seg }}
          </kbd>
        </template>
        <template v-if="triggerSnapshot === null">
          <span class="px-0.5 text-sm text-muted-foreground">+</span>
          <kbd
            class="inline-flex h-8 min-w-[2rem] items-center justify-center rounded border border-dashed border-border bg-muted/40 px-2 font-mono text-base font-medium text-muted-foreground"
          >
            &hellip;
          </kbd>
        </template>
      </template>
    </template>

    <template v-else>
      <template v-for="(seg, i) in committedSegments" :key="`cmt-${i}`">
        <span v-if="i > 0" class="px-0.5 text-sm text-muted-foreground">+</span>
        <kbd
          class="inline-flex h-8 min-w-[2rem] items-center justify-center rounded border border-border bg-muted px-2 font-mono text-base font-medium text-foreground"
        >
          {{ seg }}
        </kbd>
      </template>
    </template>
  </div>
</template>
