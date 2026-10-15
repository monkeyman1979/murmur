<script setup lang="ts">
import HotkeyRecorder from "@/components/settings/HotkeyRecorder.vue";
import SettingRow from "@/components/settings/SettingRow.vue";
import SpeechModelSettings from "@/components/settings/SpeechModelSettings.vue";
import SupportedDictationLanguagesDialog from "@/components/settings/SupportedDictationLanguagesDialog.vue";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { IconTooltip } from "@/components/ui/tooltip";
import { useAutostart } from "@/composables/useAutostart";
import { useCaptures } from "@/composables/useCaptures";
import { appLanguage, setAppLanguage, supportedAppLanguages } from "@/i18n/appLanguage";
import { useI18n } from "vue-i18n";
import { useHistory } from "@/composables/useHistory";
import {
  RETENTION_OPTIONS,
  useHistoryRetention,
  type RetentionDays,
} from "@/composables/useHistoryRetention";
import { useHotkeyBinding } from "@/composables/useHotkeyBinding";
import { useInputDevices } from "@/composables/useInputDevices";
import {
  useMeetingAiHandoff,
  useMeetingAiHandoffDraft,
  validateMeetingAiHandoffPrompt,
  MEETING_AI_HANDOFF_PLACEHOLDER,
} from "@/composables/useMeetingAiHandoff";
import {
  MEETING_AUDIO_RETENTION_OPTIONS,
  useMeetingAudioRetention,
} from "@/composables/useMeetingAudioRetention";
import {
  MEETING_TRANSCRIPT_RETENTION_OPTIONS,
  useMeetingTranscriptRetention,
  type MeetingTranscriptRetentionDays,
} from "@/composables/useMeetingTranscriptRetention";
import { useMuteWhileDictating } from "@/composables/useMuteWhileDictating";
import { useSoundCues } from "@/composables/useSoundCues";
import { useUpdater } from "@/composables/useUpdater";
import {
  retentionChangeRequiresConfirmation,
} from "@/lib/retention";
import {
  useTheme,
  type ShellGradient,
  type ThemeMode,
} from "@/composables/useTheme";
import { Check, RefreshCw } from "lucide-vue-next";
import { computed, nextTick, ref, watch } from "vue";
import { useRoute } from "vue-router";
import { toast } from "vue-sonner";

const route = useRoute();
const { t } = useI18n();
const appLanguageSelectValue = computed({
  get: () => appLanguage.value,
  set: (value: string) => setAppLanguage(value),
});
const { binding: hotkeyBinding } = useHotkeyBinding();
const { mode, setMode, shellGradient, setShellGradient } = useTheme();
const { enabled: autostartEnabled, setEnabled: setAutostartEnabled } =
  useAutostart();
const {
  devices: inputDevices,
  selected: selectedInputDevice,
  refresh: refreshInputDevices,
} = useInputDevices();
const { enabled: soundCuesEnabled } = useSoundCues();
const { enabled: muteWhileDictatingEnabled } = useMuteWhileDictating();
const updater = useUpdater();
const updateStatus = computed<string>(() => {
  if (updater.phase.value === "checking") return t('generalSettings.checking');
  if (updater.phase.value === "up-to-date") return t('generalSettings.upToDate');
  if (["available", "downloading", "ready", "installing"].includes(updater.phase.value)) {
    return updater.version.value ? t('generalSettings.versionAvailable', { version: updater.version.value }) : t('generalSettings.updateAvailable');
  }
  if (updater.phase.value === "error") return updater.errorMessage.value ?? t('generalSettings.updateFailed');
  return t('generalSettings.updateHelp');
});
const { days: retentionDays } = useHistoryRetention();
const { retention: meetingAudioRetention } = useMeetingAudioRetention();
const { days: meetingTranscriptRetentionDays } = useMeetingTranscriptRetention();
const {
  deletableCount: meetingHistoryTotal,
  clearingAll: clearingAllMeetings,
  clearAllMeetings,
} = useCaptures();
const {
  prompt: meetingAiHandoffPrompt,
  isCustom: meetingAiHandoffCustomized,
  storageError: meetingAiHandoffStorageError,
  save: saveMeetingAiHandoffPrompt,
  restoreDefault: restoreMeetingAiHandoffDefault,
} = useMeetingAiHandoff();
const meetingAiHandoffDraft = useMeetingAiHandoffDraft(meetingAiHandoffPrompt);
const meetingAiHandoffValidationError = computed(() =>
  validateMeetingAiHandoffPrompt(meetingAiHandoffDraft.value),
);
const meetingAiHandoffError = computed(() =>
  meetingAiHandoffValidationError.value
    ? t(meetingAiHandoffDraft.value.includes(MEETING_AI_HANDOFF_PLACEHOLDER) ? 'generalSettings.aiPromptRepeated' : 'generalSettings.aiPromptMissing', { placeholder: MEETING_AI_HANDOFF_PLACEHOLDER })
    : meetingAiHandoffStorageError.value ? t('generalSettings.aiPromptStorageError') : null,
);
function retentionLabel(days: number): string {
  return days === 0 ? t('generalSettings.retentionForever')
    : days === 365 ? t('generalSettings.retentionYear')
    : t('generalSettings.retentionDays', { count: days });
}
function audioRetentionLabel(value: string): string {
  return t(`generalSettings.${({ delete_immediately: 'audioImmediately', after_24_hours: 'audioAfterHours', after_7_days: 'audioAfterDays' } as Record<string, string>)[value]}`);
}
const meetingAiHandoffDirty = computed(() =>
  meetingAiHandoffDraft.value !== meetingAiHandoffPrompt.value,
);
// `total` stays reactive — booting `useHistory()` here means the
// Settings page hydrates the same shared list `/history` uses. We
// only read `total` + `clearAll` from the composable; the list
// itself isn't rendered on this page.
const { total: historyTotal, clearAll: clearAllHistory } = useHistory();
type PendingRetentionChange =
  | { kind: "dictations"; days: RetentionDays; label: string }
  | { kind: "meetings"; days: MeetingTranscriptRetentionDays; label: string };
const pendingRetentionChange = ref<PendingRetentionChange | null>(null);
const retentionChangeDialogOpen = ref(false);

function applyRetentionChange(change: PendingRetentionChange): void {
  if (change.kind === "dictations") retentionDays.value = change.days;
  else meetingTranscriptRetentionDays.value = change.days;

  toast.message(t(`generalSettings.${change.kind === 'dictations' ? 'dictations' : 'meetings'}${change.days === 0 ? 'ForeverToast' : 'AfterToast'}`, { period: retentionLabel(change.days) }));
}

function requestRetentionChange(change: PendingRetentionChange): void {
  const currentDays = change.kind === "dictations"
    ? retentionDays.value
    : meetingTranscriptRetentionDays.value;
  if (currentDays === change.days) return;
  if (retentionChangeRequiresConfirmation(currentDays, change.days)) {
    pendingRetentionChange.value = change;
    retentionChangeDialogOpen.value = true;
    return;
  }
  applyRetentionChange(change);
}

function setRetentionChangeDialogOpen(open: boolean): void {
  retentionChangeDialogOpen.value = open;
  if (!open) pendingRetentionChange.value = null;
}

function confirmRetentionChange(): void {
  const change = pendingRetentionChange.value;
  if (!change) return;
  applyRetentionChange(change);
  retentionChangeDialogOpen.value = false;
  pendingRetentionChange.value = null;
}

const historyRetentionSelectValue = computed<string>({
  get: () => String(retentionDays.value),
  set: (value) => {
    const option = RETENTION_OPTIONS.find(
      (candidate) => String(candidate.value) === value,
    );
    if (option) requestRetentionChange({
      kind: "dictations",
      days: option.value,
      label: retentionLabel(option.value),
    });
  },
});
const meetingTranscriptRetentionSelectValue = computed<string>({
  get: () => String(meetingTranscriptRetentionDays.value),
  set: (value) => {
    const option = MEETING_TRANSCRIPT_RETENTION_OPTIONS.find(
      (candidate) => String(candidate.value) === value,
    );
    if (option) requestRetentionChange({
      kind: "meetings",
      days: option.value,
      label: retentionLabel(option.value),
    });
  },
});

// Reka Select reserves the empty string for clearing, so explicit sentinel
// values represent each picker's nullable automatic mode.
const AUTOMATIC_DEVICE_VALUE = "__murmur_automatic_device__";
const inputDeviceSelectValue = computed<string>({
  get: () => selectedInputDevice.value ?? AUTOMATIC_DEVICE_VALUE,
  set: (value) => {
    selectedInputDevice.value = value === AUTOMATIC_DEVICE_VALUE ? null : value;
  },
});
const simplifyMicrophoneName = (name: string): string =>
  name.replace(/\s+Microphone$/i, "").trim();
const isPreferredAutomaticInput = (name: string): boolean =>
  name.toLowerCase().includes("usb");
const currentDefaultInputDevice = computed<string | null>(
  () => inputDevices.value.find((device) => device.is_default)?.name ?? null,
);
const automaticInputDevice = computed<string | null>(
  () =>
    inputDevices.value.find((device) => isPreferredAutomaticInput(device.name))
      ?.name ?? currentDefaultInputDevice.value,
);
const automaticInputDeviceLabel = computed<string>(() =>
  automaticInputDevice.value
    ? simplifyMicrophoneName(automaticInputDevice.value)
    : t('generalSettings.automatic'),
);
const hasMultipleInputDevices = computed<boolean>(() => inputDevices.value.length > 1);
const selectedInputDeviceLabel = computed<string>(() => {
  if (selectedInputDevice.value === null) return automaticInputDeviceLabel.value;
  const device = inputDevices.value.find((item) => item.name === selectedInputDevice.value);
  const suffix = device?.is_default && hasMultipleInputDevices.value
    ? ` (${t('generalSettings.default')})` : '';
  return `${simplifyMicrophoneName(selectedInputDevice.value)}${suffix}`;
});
const appLanguageLabel = computed(() =>
  supportedAppLanguages.find(({ code }) => code === appLanguage.value)?.label ?? 'English',
);
const themeOptions: { value: ThemeMode }[] = [{ value: "system" }, { value: "light" }, { value: "dark" }];
const shellGradientOptions: {
  value: ShellGradient;
  preview: string;
}[] = [
  {
    value: "sunset",
    preview:
      "radial-gradient(circle at 18% 8%, #aebed0 0%, transparent 52%), linear-gradient(145deg, #8fa4ba 0%, #d0aaa2 52%, #e3a36e 100%)",
  },
  {
    value: "forest",
    preview:
      "radial-gradient(circle at 18% 8%, #8c8b45 0%, transparent 52%), linear-gradient(145deg, #4a4c1f 0%, #29382f 52%, #0c2138 100%)",
  },
  {
    value: "desert",
    preview:
      "radial-gradient(circle at 18% 8%, #f1c985 0%, transparent 52%), linear-gradient(145deg, #d9b67a 0%, #c98962 52%, #c47d60 100%)",
  },
  {
    value: "mocha",
    preview:
      "radial-gradient(circle at 18% 8%, rgba(230, 189, 141, 0.9) 0%, transparent 54%), linear-gradient(145deg, #795449 0%, #4b302f 52%, #241c22 100%)",
  },
];

// `useAutostart` starts with `null` until the plugin responds. Coerce
// to boolean for the Switch's v-model; null reads as "off" which is a
// reasonable default on first paint.
const autostartSwitch = computed<boolean>({
  get: () => autostartEnabled.value === true,
  set: (value) => {
    void setAutostartEnabled(value);
  },
});

watch(
  () => route.query.section,
  async (section) => {
    if (section !== "speech-model" && section !== "ai-handoff") return;
    await nextTick();
    // App.vue resets the shared <main> scroll position after route insertion.
    // Wait until the next frame so the deep link wins over that reset.
    window.requestAnimationFrame(() => {
      const target = document.getElementById(section === "ai-handoff" ? "meeting-ai-handoff" : "speech-model");
      target?.scrollIntoView({
        behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
        block: "start",
      });
      if (section === "ai-handoff") target?.focus({ preventScroll: true });
    });
  },
  { immediate: true },
);

function saveMeetingAiPrompt(): void {
  if (meetingAiHandoffValidationError.value) return;
  try {
    saveMeetingAiHandoffPrompt(meetingAiHandoffDraft.value);
  } catch {
    // The composable keeps the saved value unchanged and exposes actionable copy.
  }
}

function cancelMeetingAiPrompt(): void {
  meetingAiHandoffDraft.value = meetingAiHandoffPrompt.value;
}

function confirmMeetingAiPromptRestore(): void {
  try {
    restoreMeetingAiHandoffDefault();
    meetingAiHandoffDraft.value = meetingAiHandoffPrompt.value;
  } catch {
    // Leave the custom prompt and draft intact when local storage is unavailable.
  }
}

</script>

<template>
  <div class="flex min-w-0 flex-col gap-4 p-4">

    <!-- Hotkey -->
    <Card>
      <CardContent class="flex flex-col gap-0 px-4 py-2">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-2">
          {{ t('generalSettings.hotkey') }}
        </h2>
        <SettingRow
          :title="t('generalSettings.hotkeyTitle')"
          :description="t('generalSettings.hotkeyHelp')"
        >
          <HotkeyRecorder v-model="hotkeyBinding" />
        </SettingRow>
      </CardContent>
    </Card>

    <!-- Appearance -->
    <Card>
      <CardContent class="flex flex-col gap-0 px-4 py-2">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-2">
          {{ t('generalSettings.appearance') }}
        </h2>
        <SettingRow
          :title="t('generalSettings.theme')"
          :description="t('generalSettings.themeHelp')"
        >
          <div
            class="inline-flex items-center gap-0.5 rounded-md bg-muted p-0.5"
            role="radiogroup"
            :aria-label="t('generalSettings.theme')"
          >
            <button
              v-for="opt in themeOptions"
              :key="opt.value"
              type="button"
              role="radio"
              :aria-checked="mode === opt.value"
              :class="[
                'rounded-sm px-3 py-1 text-xs font-medium transition-colors',
                mode === opt.value
                  ? 'bg-background text-foreground shadow-sm'
                  : 'text-muted-foreground hover:text-foreground',
              ]"
              @click="setMode(opt.value)"
            >
              {{ t(`generalSettings.${opt.value}`) }}
            </button>
          </div>
        </SettingRow>

        <Separator />

        <SettingRow
          :title="t('generalSettings.shell')"
          :description="t('generalSettings.shellHelp')"
        >
          <div class="flex items-center gap-3" role="radiogroup" :aria-label="t('generalSettings.shell')">
            <IconTooltip
              v-for="opt in shellGradientOptions"
              :key="opt.value"
              :label="t(`generalSettings.${opt.value}`)"
            >
              <button
                type="button"
                role="radio"
                :aria-checked="shellGradient === opt.value"
                :aria-label="t(`generalSettings.${opt.value}`)"
                :style="{ background: opt.preview }"
                :class="[
                  'inline-flex size-8 cursor-pointer items-center justify-center rounded-full ring-1 ring-border/60 transition-shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card',
                  shellGradient === opt.value
                    ? 'ring-2 ring-ring ring-offset-2 ring-offset-card'
                    : 'hover:ring-foreground/40',
                ]"
                @click="setShellGradient(opt.value)"
              >
                <Check
                  v-if="shellGradient === opt.value"
                  class="size-3.5 text-white drop-shadow-[0_1px_2px_rgba(0,0,0,0.7)]"
                  aria-hidden="true"
                />
              </button>
            </IconTooltip>
          </div>
        </SettingRow>
      </CardContent>
    </Card>

    <!-- Fundamentals -->
    <Card>
      <CardContent class="flex flex-col gap-0 px-4 py-2">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-2">
          {{ t('generalSettings.fundamentals') }}
        </h2>
        <SettingRow
          :title="t('generalSettings.login')"
          :description="t('generalSettings.loginHelp')"
        >
          <Switch v-model="autostartSwitch" :aria-label="t('generalSettings.login')" />
        </SettingRow>

        <Separator />

        <SettingRow
          v-if="updater.enabled"
          :title="t('generalSettings.updates')"
          :description="updateStatus"
        >
          <Button
            size="sm"
            variant="outline"
            :disabled="updater.phase.value === 'checking' || updater.phase.value === 'downloading' || updater.phase.value === 'installing'"
            @click="updater.checkForUpdates({ manual: true })"
          >
            <RefreshCw :class="['size-3.5', updater.phase.value === 'checking' ? 'animate-spin' : '']" aria-hidden="true" />
            {{ t('generalSettings.checkUpdates') }}
          </Button>
        </SettingRow>

        <Separator v-if="updater.enabled" />

        <SettingRow
          :title="t('generalSettings.sound')"
          :description="t('generalSettings.soundHelp')"
        >
          <Switch v-model="soundCuesEnabled" :aria-label="t('generalSettings.sound')" />
        </SettingRow>

        <Separator />

        <SettingRow
          :title="t('generalSettings.mute')"
          :description="t('generalSettings.muteHelp')"
        >
          <Switch
            v-model="muteWhileDictatingEnabled"
            :aria-label="t('generalSettings.mute')"
          />
        </SettingRow>

        <Separator />

        <SettingRow
          :title="t('generalSettings.microphone')"
          :description="t('generalSettings.microphoneHelp')"
        >
          <div class="flex items-center gap-2">
            <Select
              v-model="inputDeviceSelectValue"
              :disabled="!hasMultipleInputDevices"
            >
              <SelectTrigger class="w-56 max-w-full" :aria-label="t('generalSettings.inputDevice')">
                <SelectValue>{{ selectedInputDeviceLabel }}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem :value="AUTOMATIC_DEVICE_VALUE">
                  {{ selectedInputDevice === null ? automaticInputDeviceLabel : t('generalSettings.automatic') }}
                </SelectItem>
                <SelectItem
                  v-for="device in inputDevices"
                  :key="device.name"
                  :value="device.name"
                >
                  {{ simplifyMicrophoneName(device.name) }}{{ device.is_default && hasMultipleInputDevices ? ` (${t('generalSettings.default')})` : "" }}
                </SelectItem>
              </SelectContent>
            </Select>
            <IconTooltip :label="t('generalSettings.refreshMicrophones')">
              <button
                type="button"
                class="inline-flex size-9 cursor-pointer items-center justify-center rounded-md border border-input bg-transparent text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                :aria-label="t('generalSettings.refreshMicrophones')"
                @click="refreshInputDevices"
              >
                <RefreshCw class="size-4" aria-hidden="true" />
              </button>
            </IconTooltip>
          </div>
        </SettingRow>

        <Separator />
        <SettingRow
          :title="t('settingsAppLanguage.title')"
          :description="t('settingsAppLanguage.description')"
        >
          <Select v-model="appLanguageSelectValue">
            <SelectTrigger class="w-44 max-w-full" :aria-label="t('settingsAppLanguage.label')">
              <SelectValue>{{ appLanguageLabel }}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem v-for="option in supportedAppLanguages" :key="option.code" :value="option.code">
                {{ option.label }}
              </SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>

      </CardContent>
    </Card>

    <!-- Dictation transcript retention is independent from meeting storage. -->
    <Card>
      <CardContent class="flex flex-col gap-0 px-4 py-2">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-2">
          {{ t('generalSettings.dictation') }}
        </h2>
        <SettingRow
          :title="t('settingsDictationLanguage.title')"
          :description="t('settingsDictationLanguage.description')"
        >
          <SupportedDictationLanguagesDialog />
        </SettingRow>
        <Separator />
        <SettingRow
          :title="t('generalSettings.keepDictations')"
          :description="t('generalSettings.dictationRetentionHelp')"
        >
          <Select v-model="historyRetentionSelectValue">
            <SelectTrigger class="w-44 max-w-full" :aria-label="t('generalSettings.dictationRetentionLabel')">
              <SelectValue>{{ retentionLabel(retentionDays) }}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="opt in RETENTION_OPTIONS"
                :key="opt.value"
                :value="String(opt.value)"
              >
                {{ retentionLabel(opt.value) }}
              </SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>

        <Separator />

        <SettingRow
          :title="t('generalSettings.deleteDictations')"
          :description="t('generalSettings.deleteDictationsHelp')"
        >
          <AlertDialog>
            <AlertDialogTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="text-destructive hover:text-destructive hover:bg-destructive/10"
                :disabled="historyTotal === 0"
              >
                {{ t('generalSettings.deleteNow') }}
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>{{ t('generalSettings.deleteAllDictations') }}</AlertDialogTitle>
                <AlertDialogDescription as="div" class="flex flex-col gap-2 text-pretty">
                  <p>{{ t('generalSettings.dictationsDeletedHelp') }}</p>
                  <p>{{ t('generalSettings.otherDataSafe') }}</p>
                  <p class="font-medium text-foreground">{{ t('generalSettings.irreversible') }}</p>
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel>
                <AlertDialogAction
                  variant="destructive"
                  @click="clearAllHistory"
                >
                  {{ t('generalSettings.confirmDeleteDictations') }}
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </SettingRow>
      </CardContent>
    </Card>

    <Card>
      <CardContent class="flex flex-col gap-0 px-4 py-2">
        <h2 class="mb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          {{ t('generalSettings.meetingCaptures') }}
        </h2>
        <SettingRow
          :title="t('generalSettings.keepMeetings')"
          :description="t('generalSettings.meetingRetentionHelp')"
        >
          <Select v-model="meetingTranscriptRetentionSelectValue">
            <SelectTrigger
              class="w-44 max-w-full"
              :aria-label="t('generalSettings.meetingRetentionLabel')"
            >
              <SelectValue>{{ retentionLabel(meetingTranscriptRetentionDays) }}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="option in MEETING_TRANSCRIPT_RETENTION_OPTIONS"
                :key="option.value"
                :value="String(option.value)"
              >
                {{ retentionLabel(option.value) }}
              </SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>

        <Separator />

        <SettingRow
          :title="t('generalSettings.keepAudio')"
          :description="t('generalSettings.audioRetentionHelp')"
        >
          <Select v-model="meetingAudioRetention">
            <SelectTrigger
              class="w-44 max-w-full"
              :aria-label="t('generalSettings.audioRetentionLabel')"
            >
              <SelectValue>{{ audioRetentionLabel(meetingAudioRetention) }}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="option in MEETING_AUDIO_RETENTION_OPTIONS"
                :key="option.value"
                :value="option.value"
              >
                {{ audioRetentionLabel(option.value) }}
              </SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>

        <Separator />

        <SettingRow
          :title="t('generalSettings.deleteMeetings')"
          :description="t('generalSettings.deleteMeetingsHelp')"
        >
          <AlertDialog>
            <AlertDialogTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="text-destructive hover:text-destructive hover:bg-destructive/10"
                :disabled="meetingHistoryTotal === 0 || clearingAllMeetings"
              >
                {{ t(clearingAllMeetings ? 'generalSettings.deleting' : 'generalSettings.deleteNow') }}
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>{{ t('generalSettings.deleteAllMeetings') }}</AlertDialogTitle>
                <AlertDialogDescription as="div" class="flex flex-col gap-2 text-pretty">
                  <p>{{ t('generalSettings.meetingsDeletedHelp') }}</p>
                  <p>{{ t('generalSettings.savedCopiesSafe') }}</p>
                  <p class="font-medium text-foreground">{{ t('generalSettings.irreversible') }}</p>
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>{{ t('meetingDialogs.cancel') }}</AlertDialogCancel>
                <AlertDialogAction variant="destructive" @click="clearAllMeetings">
                  {{ t('generalSettings.confirmDeleteMeetings') }}
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </SettingRow>

        <Separator />

        <div id="meeting-ai-handoff" tabindex="-1" :aria-label="t('generalSettings.aiPrompt')" class="flex scroll-mt-6 flex-col gap-3 py-3 outline-none">
          <div>
            <label for="meeting-ai-handoff-prompt" class="text-sm font-medium">
              {{ t('generalSettings.aiPrompt') }}
            </label>
            <p class="mt-1 text-xs leading-snug text-muted-foreground">
              {{ t('generalSettings.aiPromptHelp') }}
            </p>
            <p class="mt-1 text-xs leading-snug text-muted-foreground">
              {{ t('generalSettings.aiPromptLanguageHelp') }}
            </p>
          </div>
          <textarea
            id="meeting-ai-handoff-prompt"
            v-model="meetingAiHandoffDraft"
            class="min-h-72 w-full resize-y rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs leading-relaxed shadow-sm transition-colors focus:outline-none focus:ring-1 focus:ring-ring"
            :aria-invalid="meetingAiHandoffError ? 'true' : undefined"
            :aria-describedby="meetingAiHandoffError ? 'meeting-ai-handoff-error' : undefined"
            spellcheck="true"
          />
          <p
            v-if="meetingAiHandoffError"
            id="meeting-ai-handoff-error"
            class="text-xs text-destructive"
            role="alert"
          >
            {{ meetingAiHandoffError }}
          </p>
          <div class="flex flex-wrap items-center justify-between gap-2">
            <AlertDialog>
              <AlertDialogTrigger as-child>
                <Button variant="ghost" size="sm" :disabled="!meetingAiHandoffCustomized">
                  {{ t('generalSettings.restoreDefault') }}
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>{{ t('generalSettings.restoreTitle') }}</AlertDialogTitle>
                  <AlertDialogDescription>
                    {{ t('generalSettings.restoreHelp') }}
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>{{ t('generalSettings.keepCustom') }}</AlertDialogCancel>
                  <AlertDialogAction @click="confirmMeetingAiPromptRestore">
                    {{ t('generalSettings.restoreDefault') }}
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
            <div class="ml-auto flex items-center gap-2">
              <Button variant="outline" size="sm" :disabled="!meetingAiHandoffDirty" @click="cancelMeetingAiPrompt">
                {{ t('generalSettings.cancel') }}
              </Button>
              <Button size="sm" :disabled="!meetingAiHandoffDirty || !!meetingAiHandoffValidationError" @click="saveMeetingAiPrompt">
                {{ t('generalSettings.save') }}
              </Button>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>

    <AlertDialog
      :open="retentionChangeDialogOpen"
      @update:open="setRetentionChangeDialogOpen"
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {{ t('generalSettings.retentionTitle', { kind: t(pendingRetentionChange?.kind === 'meetings' ? 'generalSettings.meetingKind' : 'generalSettings.dictationKind'), period: pendingRetentionChange?.label ?? '' }) }}
          </AlertDialogTitle>
          <AlertDialogDescription as="div" class="flex flex-col gap-2 text-pretty">
            <template v-if="pendingRetentionChange?.kind === 'meetings'">
              <p>{{ t('generalSettings.meetingChangeHelp', { period: pendingRetentionChange.label }) }}</p>
              <p>{{ t('generalSettings.meetingChangeSafe') }}</p>
            </template>
            <template v-else>
              <p>{{ t('generalSettings.dictationChangeHelp', { period: pendingRetentionChange?.label ?? '' }) }}</p>
              <p>{{ t('generalSettings.otherDataSafe') }}</p>
            </template>
            <p class="font-medium text-foreground">{{ t('generalSettings.irreversible') }}</p>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{{ t('generalSettings.keepCurrent') }}</AlertDialogCancel>
          <Button variant="destructive" @click="confirmRetentionChange">
            {{ t('generalSettings.changeTo', { period: pendingRetentionChange?.label ?? '' }) }}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>

    <!-- Infrequent model maintenance stays available without occupying a
         primary position among everyday preferences. -->
    <SpeechModelSettings />
  </div>
</template>
