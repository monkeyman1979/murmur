<script setup lang="ts">
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useInputDevices } from "@/composables/useInputDevices";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { useMicrophoneMonitor } from "@/composables/useMicrophoneMonitor";
import { Button } from "@/components/ui/button";
import { CheckCircle2, Mic2, TriangleAlert } from "lucide-vue-next";
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";

const emit = defineEmits<{
  availability: [hasDevice: boolean];
}>();

const { t } = useI18n();
const { devices, selected, refresh, state } = useInputDevices();
const AUTOMATIC_VALUE = "__automatic__";
const { permissions } = useMurmurEvents();

const { level, monitoring: monitorRunning, monitorError, available, restart, stopForExit } = useMicrophoneMonitor();

const hasMicPermission = computed<boolean>(() => permissions.value?.microphone === "granted");
const hasDevices = computed<boolean>(() => devices.value.length > 0);
const defaultDevice = computed<string | null>(
  () => devices.value.find((device) => device.is_default)?.name ?? null,
);
const selectedDeviceLabel = computed<string>(() => {
  if (selected.value) return simplifyMicrophoneName(selected.value);
  if (defaultDevice.value) return simplifyMicrophoneName(defaultDevice.value);
  return t("onboarding.microphone.automatic");
});

const selectedValue = computed<string>({
  get: () => selected.value ?? AUTOMATIC_VALUE,
  set: (value) => {
    selected.value = value === AUTOMATIC_VALUE ? null : value;
  },
});

const levelPct = computed<number>(() => Math.round(level.value * 100));
const showOptions = ref(false);
const optionsVisible = computed(() => showOptions.value || !available.value);

function simplifyMicrophoneName(name: string): string {
  return name.replace(/\s+Microphone$/i, "").trim();
}

defineExpose({ stopForExit });

watch(available, (value) => emit("availability", value), { immediate: true, flush: "sync" });
</script>

<template>
  <div
    class="rounded-[2rem] border border-border bg-card p-6 "
  >
    <div class="flex items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <div class="flex size-12 items-center justify-center rounded-2xl bg-muted text-foreground">
          <Mic2 class="size-6" aria-hidden="true" />
        </div>
        <div>
          <p class="text-sm text-muted-foreground">{{ t("onboarding.microphone.currentLabel") }}</p>
          <h2 class="text-lg font-semibold tracking-tight text-foreground">
            {{ selectedDeviceLabel }}
          </h2>
        </div>
      </div>
      <CheckCircle2
        v-if="available"
        class="size-7 text-success"
        aria-hidden="true"
      />
      <TriangleAlert v-else class="size-7 text-muted-foreground" aria-hidden="true" />
    </div>

    <p v-if="available" role="status" class="mt-5 text-sm text-success">{{ t("onboarding.microphone.ready") }}</p>
    <Button
      v-if="available"
      type="button"
      variant="outline"
      size="sm"
      class="mt-4"
      :aria-expanded="showOptions"
      aria-controls="microphone-options"
      @click="showOptions = !showOptions"
    >{{ t(showOptions ? "onboarding.microphone.hideInput" : "onboarding.microphone.changeInput") }}</Button>

    <div v-if="optionsVisible" id="microphone-options" class="mt-6">
      <label for="onboarding-microphone" class="text-sm font-medium text-foreground">
        {{ t("onboarding.microphone.pickerLabel") }}
      </label>
      <Select v-model="selectedValue" :disabled="!hasDevices || !hasMicPermission">
        <SelectTrigger
          id="onboarding-microphone"
          class="mt-2 h-9 w-full"
        >
          <SelectValue :placeholder="t('onboarding.microphone.automatic')">{{ selected === null ? t('onboarding.microphone.automatic') : selectedDeviceLabel }}</SelectValue>
        </SelectTrigger>
        <SelectContent class="dark">
          <SelectItem :value="AUTOMATIC_VALUE">
            {{ t("onboarding.microphone.automatic") }}
          </SelectItem>
          <SelectItem
            v-for="device in devices"
            :key="device.name"
            :value="device.name"
          >
            {{ simplifyMicrophoneName(device.name) }}{{ device.is_default ? ` (${t("onboarding.microphone.defaultSuffix")})` : "" }}
          </SelectItem>
        </SelectContent>
      </Select>
    </div>

    <div v-if="optionsVisible" class="mt-7 rounded-3xl bg-muted/50 p-5">
      <div class="mb-3 flex items-center justify-between gap-3 text-sm">
        <p role="status" class="font-medium text-foreground">
          {{ !hasMicPermission ? t("onboarding.microphone.permissionNeeded") : monitorRunning ? t("onboarding.microphone.meterLive") : t("onboarding.microphone.meterStarting") }}
        </p>
        <p class="tabular-nums text-muted-foreground">{{ levelPct }}%</p>
      </div>
      <div class="h-3 overflow-hidden rounded-full bg-muted" aria-hidden="true">
        <div
          class="h-full rounded-full bg-success transition-[width] duration-75"
          :style="{ width: `${levelPct}%` }"
        />
      </div>
      <p class="mt-3 text-sm leading-6 text-muted-foreground">
        {{ t("onboarding.microphone.meterHint") }}
      </p>
    </div>

    <div
      v-if="monitorError"
      role="status"
      class="mt-4 rounded-2xl border border-border bg-muted/50 px-4 py-3 text-sm leading-6 text-foreground"
    >
      {{ t("onboarding.microphone.error") }}
      <Button size="sm" variant="outline" @click="restart">{{ t("onboarding.requiredModel.retryButton") }}</Button>
    </div>

    <div
      v-else-if="state !== 'ready'"
      role="status"
      class="mt-4 rounded-2xl border border-border bg-muted/50 px-4 py-3 text-sm leading-6 text-foreground"
    >
      {{ t(!hasMicPermission ? "onboarding.microphone.permissionHelp" : state === "loading" ? "onboarding.microphone.loading" : state === "error" ? "onboarding.microphone.enumerationError" : "onboarding.microphone.noDevice") }}
      <Button v-if="state === 'error' || state === 'empty'" size="sm" variant="outline" @click="refresh">{{ t("onboarding.requiredModel.retryButton") }}</Button>
    </div>
  </div>
</template>
