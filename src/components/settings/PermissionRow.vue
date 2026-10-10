<script setup lang="ts">
import { Button } from "@/components/ui/button";
import type { PermissionState } from "@/composables/useMurmurEvents";
import { invoke } from "@tauri-apps/api/core";
import { ExternalLink } from "lucide-vue-next";
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const props = defineProps<{
  title: string;
  description: string;
  /** `null` until the first `murmur://permissions` event lands. */
  state: PermissionState | null;
  /** Identifier passed to the Rust `open_privacy_pane` command — one of
   *  `"microphone"`, `"input-monitoring"`, `"accessibility"`. */
  pane: "microphone" | "input-monitoring" | "accessibility";
}>();

// Tracks whether we've already fired the native TCC prompt for this
// row in the current session. macOS suppresses the prompt after the
// first user response, so if the state is still not `granted` after a
// prompt attempt we swap the button to "Open Settings" — the only
// recovery path once TCC has cached a decision.
const hasTriedPrompt = ref(false);

const stateLabel = computed<string>(() => {
  switch (props.state) {
    case "granted":
      return t('permissionSettings.granted');
    case "denied":
      return t('permissionSettings.denied');
    case "not-determined":
      return t('permissionSettings.notDetermined');
    case "unknown":
      return t('permissionSettings.unknown');
    default:
      return t('permissionSettings.checking');
  }
});

const dotClass = computed<string>(() => {
  switch (props.state) {
    case "granted":
      return "bg-green-500";
    case "denied":
      return "bg-red-500";
    case "not-determined":
      return "bg-amber-500";
    default:
      return "bg-muted-foreground/50";
  }
});

// Input Monitoring has no native prompt — macOS only surfaces a
// Settings-level toggle, no dialog. So IM rows always route through
// Open Settings regardless of state. Mic and Accessibility have real
// TCC prompts.
const supportsPrompt = computed<boolean>(() => props.pane !== "input-monitoring");

/** Button mode:
 *  - `prompt`: shows "Grant" → fires the native TCC prompt for this pane
 *  - `settings`: shows "Open Settings" → deep-links to the Privacy pane
 *
 *  `prompt` only applies when the pane supports a native prompt, the
 *  state is not yet granted, AND we haven't already tried a prompt in
 *  this session. Every other state — granted, already-prompted, denied
 *  for a non-promptable pane — lands on `settings`, which keeps the
 *  "Open Settings" deep-link available even after grant so users can
 *  revoke or re-verify from the permissions pane without having to
 *  remember the macOS System Settings path. Left-side dot carries the
 *  granted/denied visual state; the button is strictly an action
 *  affordance, not a status indicator.
 */
const buttonMode = computed<"prompt" | "settings">(() => {
  if (!supportsPrompt.value) return "settings";
  if (props.state === "granted") return "settings";
  if (props.state === "not-determined" && !hasTriedPrompt.value) return "prompt";
  // Accessibility + Microphone: offer the Grant button once even on
  // `denied`. Two separate reasons for each, same effect:
  //
  // - **Accessibility** has no formal `not-determined` state on
  //   macOS; `AXIsProcessTrustedWithOptions({prompt:YES})` opens
  //   the Accessibility-pane dialog even when the cached status
  //   reads `denied`. The Grant button is the way to trigger that.
  //
  // - **Microphone** reaches `denied` via the `AVAuthorizationStatus`
  //   mapping when there's any cached TCC state for the bundle
  //   (including stale entries where macOS returns `denied` even
  //   though System Settings shows nothing). On a true `denied`,
  //   `AVCaptureDevice.requestAccess` is a no-op — safe to call,
  //   just doesn't prompt. On the edge case where the cache is
  //   actually `notDetermined` masquerading as `denied` (races after
  //   `tccutil reset` while the app is alive), the prompt fires. So
  //   trying once is always worth it; the button swaps to "Open
  //   Settings" on the second press via `hasTriedPrompt` if the
  //   prompt didn't actually land.
  if (
    (props.pane === "accessibility" || props.pane === "microphone") &&
    props.state === "denied" &&
    !hasTriedPrompt.value
  ) {
    return "prompt";
  }
  return "settings";
});

const promptCommand = computed<string | null>(() => {
  switch (props.pane) {
    case "microphone":
      return "request_microphone_prompt";
    case "accessibility":
      return "request_accessibility_prompt";
    case "input-monitoring":
      return null; // no prompt available
  }
});

async function handleGrant(): Promise<void> {
  const cmd = promptCommand.value;
  if (!cmd) return;
  hasTriedPrompt.value = true;
  try {
    await invoke(cmd);
  } catch (err) {
    console.error(`[murmur] ${cmd} failed`, err);
  }
}

async function openSettings(): Promise<void> {
  try {
    await invoke("open_privacy_pane", { pane: props.pane });
  } catch (err) {
    console.error(`[murmur] open_privacy_pane(${props.pane}) failed`, err);
  }
}
</script>

<template>
  <div class="flex items-start justify-between gap-4 py-3">
    <div class="flex flex-1 items-start gap-3 min-w-0">
      <span
        :class="['mt-1.5 size-2 shrink-0 rounded-full', dotClass]"
        aria-hidden="true"
      />
      <div class="flex flex-col gap-0.5 min-w-0">
        <div class="flex flex-wrap items-baseline gap-x-2">
          <p class="text-sm font-medium leading-tight">{{ title }}</p>
          <span class="text-xs text-muted-foreground">{{ stateLabel }}</span>
        </div>
        <p class="text-xs text-muted-foreground leading-snug text-balance">
          {{ description }}
        </p>
      </div>
    </div>

    <!-- Prompt: fire the native TCC dialog. Only shown when the pane
         supports a prompt and we haven't tried one this session. -->
    <Button
      v-if="buttonMode === 'prompt'"
      size="sm"
      class="shrink-0"
      @click="handleGrant"
    >
      {{ t('permissionSettings.grant') }}
    </Button>

    <!-- Settings: deep-link to System Settings → Privacy pane. Shown
         when granted too — the left-side dot + label already carry the
         status, and keeping the button available means users can revoke
         or re-verify without memorizing the macOS pane path. -->
    <Button
      v-else
      variant="secondary"
      size="sm"
      class="shrink-0"
      @click="openSettings"
    >
      {{ t('permissionSettings.openSettings') }}
      <ExternalLink class="size-3.5 ml-1" aria-hidden="true" />
    </Button>
  </div>
</template>
