<script setup lang="ts">
import { Switch } from "@/components/ui/switch";
import { useAutostart } from "@/composables/useAutostart";
import { Button } from "@/components/ui/button";
import {
  ClipboardPaste,
  Keyboard,
  Mic,
} from "lucide-vue-next";
import { invoke } from "@tauri-apps/api/core";
import { computed, onBeforeUnmount, reactive, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import {
  type PermissionState,
  useMurmurEvents,
} from "@/composables/useMurmurEvents";

type Pane = "microphone" | "input-monitoring" | "accessibility";

const { t } = useI18n();
const { permissions } = useMurmurEvents();
const { enabled, loading, pending, error, setEnabled, refresh } = useAutostart();
const autostartStatus = computed(() => {
  // Writes include the confirmation read: don't flash Saving/Checking for
  // either stage. The disabled, busy switch is enough transient feedback.
  if (pending.value) return "";
  if (loading.value || enabled.value === null) {
    return t(error.value ? "onboarding.permissions.autostart.unknown" : "onboarding.permissions.status.checking");
  }
  return "";
});
const relaunchError = ref(false);
const settingsError = ref<Pane | null>(null);
async function relaunch() {
  relaunchError.value = false;
  try { await invoke("restart_murmur"); } catch { relaunchError.value = true; }
}

const triedPrompt = reactive({
  microphone: false,
  inputMonitoring: false,
  accessibility: false,
});
const requesting = ref<Pane | null>(null);
const settingsOpened = reactive({ inputMonitoring: false, accessibility: false });

const micState = computed<PermissionState | null>(
  () => permissions.value?.microphone ?? null,
);
const inputState = computed<PermissionState | null>(
  () => permissions.value?.input_monitoring ?? null,
);
const accessState = computed<PermissionState | null>(
  () => permissions.value?.accessibility ?? null,
);

const stillNeedsAccess = (state: PermissionState | null) =>
  state === "denied" || state === "not-determined";

// On macOS 27 an Accessibility grant also authorizes the shortcut's
// listen-only event tap, and an Input Monitoring request made while
// Accessibility is off is answered silently (see permissions.rs). So the
// Input Monitoring row is only a fallback: shown if listening is still
// missing a few seconds after Accessibility is granted, which is long
// enough for the hotkey tap to start and the 2 s watcher to report it.
const INPUT_FALLBACK_DELAY_MS = 6000;
const inputFallbackDue = ref(false);
let inputFallbackTimer: ReturnType<typeof setTimeout> | undefined;
watch(accessState, (state) => {
  clearTimeout(inputFallbackTimer);
  inputFallbackDue.value = false;
  if (state === "granted") {
    inputFallbackTimer = setTimeout(() => { inputFallbackDue.value = true; }, INPUT_FALLBACK_DELAY_MS);
  }
}, { immediate: true });
onBeforeUnmount(() => clearTimeout(inputFallbackTimer));
const showInputFallback = computed(() =>
  inputFallbackDue.value && isGranted(accessState.value) && stillNeedsAccess(inputState.value),
);

const needsRelaunchHelp = computed(() =>
  showInputFallback.value
  || (settingsOpened.inputMonitoring && stillNeedsAccess(inputState.value))
  || (settingsOpened.accessibility && stillNeedsAccess(accessState.value)),
);

function isGranted(state: PermissionState | null): boolean {
  return state === "granted";
}

function hasTriedPrompt(pane: Pane): boolean {
  if (pane === "input-monitoring") return triedPrompt.inputMonitoring;
  return triedPrompt[pane];
}

function buttonLabel(pane: Pane): string {
  const opensSettings = hasTriedPrompt(pane)
    || (pane === "microphone" && micState.value === "denied");
  return t(opensSettings ? "onboarding.permissions.action.openSettings" : "onboarding.permissions.action.allow");
}

async function openSettings(pane: Pane): Promise<void> {
  settingsError.value = null;
  try {
    await invoke("open_privacy_pane", { pane });
    if (pane === "input-monitoring") settingsOpened.inputMonitoring = true;
    if (pane === "accessibility") settingsOpened.accessibility = true;
  } catch (err) {
    settingsError.value = pane;
    console.error(`[murmur] open_privacy_pane(${pane}) failed`, err);
  }
}

async function requestPermission(pane: Pane): Promise<void> {
  if (requesting.value) return;
  const nativeCommand = pane === "microphone"
    ? "request_microphone_prompt"
    : pane === "input-monitoring"
      ? "request_input_monitoring_prompt"
      : "request_accessibility_prompt";
  if (hasTriedPrompt(pane) || (pane === "microphone" && micState.value === "denied")) {
    await openSettings(pane);
    return;
  }

  // Let macOS own the first prompt. Opening Settings immediately can cover a
  // native dialog; offer the exact pane only if the request did not grant.
  requesting.value = pane;
  let result: unknown;
  try {
    result = await invoke(nativeCommand);
  } catch (err) {
    console.error(`[murmur] ${nativeCommand} failed`, err);
  } finally {
    requesting.value = null;
  }
  if (result !== true && result !== "granted") {
    triedPrompt[pane === "input-monitoring" ? "inputMonitoring" : pane] = true;
  }
}

const rows = computed(() => [
  { pane: "microphone" as const, key: "microphone", state: micState.value, icon: Mic },
  { pane: "accessibility" as const, key: "accessibility", state: accessState.value, icon: ClipboardPaste },
  ...(showInputFallback.value
    ? [{ pane: "input-monitoring" as const, key: "inputMonitoring", state: inputState.value, icon: Keyboard }]
    : []),
]);
</script>

<template>
  <div class="flex flex-col gap-5" aria-live="polite">
    <div class="divide-y divide-border rounded-3xl border border-input bg-muted px-5 shadow-lg">
      <div class="flex items-center justify-between gap-4 py-5" data-testid="autostart-row">
        <div class="flex min-w-0 flex-1 flex-col gap-1">
          <label for="onboarding-autostart" class="text-sm font-medium">{{ t("onboarding.permissions.autostart.title") }}</label>
          <p class="text-balance text-xs leading-5 text-muted-foreground">{{ t("onboarding.permissions.autostart.body") }}</p>
          <div v-if="error" class="text-xs text-destructive">
            {{ t("onboarding.permissions.autostart.error") }}
            <Button size="sm" variant="ghost" :disabled="pending || loading" @click="refresh">{{ t("onboarding.requiredModel.retryButton") }}</Button>
          </div>
        </div>
        <div class="grid min-h-11 w-28 shrink-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
          <span class="text-xs text-muted-foreground" data-testid="autostart-status">{{ autostartStatus }}</span>
          <Switch id="onboarding-autostart" :model-value="enabled === true" :aria-busy="loading || pending" :disabled="enabled === null || loading || pending" @update:model-value="setEnabled" />
        </div>
      </div>
      <div v-for="row in rows" :key="row.key" class="grid grid-cols-[1.25rem_minmax(0,1fr)_auto] items-center gap-4 py-5" data-testid="permission-row">
        <component :is="row.icon" class="mt-1 size-5 self-start text-muted-foreground" aria-hidden="true" />
        <div class="flex min-w-0 flex-1 flex-col gap-1">
          <h2 class="text-sm font-semibold">{{ t(`onboarding.permissions.${row.key}.title`) }}</h2>
          <p class="text-balance text-xs leading-5 text-muted-foreground">{{ t(`onboarding.permissions.${row.key}.body`) }}</p>
        </div>
        <span v-if="row.state === null || row.state === 'unknown'" class="text-center text-xs text-muted-foreground">{{ t("onboarding.permissions.status.checking") }}</span>
        <span v-else-if="isGranted(row.state)" class="text-center text-xs font-medium text-success">{{ t("onboarding.permissions.status.granted") }}</span>
        <Button v-else size="sm" variant="outline" class="h-auto min-h-9 w-fit max-w-full justify-self-end whitespace-normal break-words px-3 py-2 text-center leading-5 border-input bg-background" :disabled="requesting !== null" :aria-label="`${buttonLabel(row.pane)}: ${t(`onboarding.permissions.${row.key}.title`)}`" @click="requestPermission(row.pane)">{{ buttonLabel(row.pane) }}</Button>
        <p v-if="settingsError === row.pane && !isGranted(row.state)" class="col-span-2 col-start-2 text-xs leading-5 text-destructive" role="alert">{{ t('onboarding.permissions.settingsError', { permission: t(`onboarding.permissions.${row.key}.title`) }) }}</p>
        <details v-else-if="hasTriedPrompt(row.pane) && !isGranted(row.state) && requesting !== row.pane" class="col-span-2 col-start-2 min-w-0 text-xs leading-5 text-muted-foreground">
          <summary class="w-fit cursor-pointer underline underline-offset-4">{{ t('onboarding.permissions.helpLabel') }}</summary>
          <p class="mt-2 text-balance">{{ t('onboarding.permissions.settingsFallback') }}</p>
        </details>
      </div>
    </div>
    <div v-if="needsRelaunchHelp" class="flex flex-wrap items-center gap-5 text-xs leading-5 text-muted-foreground">
      <p class="flex-1 text-balance">{{ t("onboarding.permissions.relaunchHelp") }}</p>
      <Button size="sm" variant="outline" @click="relaunch">{{ t("onboarding.permissionStep.relaunchButton") }}</Button>
      <p v-if="relaunchError" class="text-destructive">{{ t("onboarding.permissions.relaunchError") }}</p>
    </div>
  </div>
</template>
