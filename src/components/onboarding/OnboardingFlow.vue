<script setup lang="ts">
import OnboardingFrame from "@/components/onboarding/OnboardingFrame.vue";
import OnboardingHomePreview from "@/components/onboarding/OnboardingHomePreview.vue";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import StepProductOverview from "@/components/onboarding/steps/StepProductOverview.vue";
import StepMicrophone from "@/components/onboarding/steps/StepMicrophone.vue";
import StepPermissions from "@/components/onboarding/steps/StepPermissions.vue";
import StepRequiredModel from "@/components/onboarding/steps/StepRequiredModel.vue";
import StepTryIt from "@/components/onboarding/steps/StepTryIt.vue";
import { useOnboardingModel } from "@/composables/useOnboardingModel";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { ONBOARDING_STEPS, useOnboarding } from "@/composables/useOnboarding";
import { appLanguage, setAppLanguage, supportedAppLanguages } from "@/i18n/appLanguage";
import { MURMUR_URLS, openMurmurLink } from "@/lib/externalLinks";
import { formatBytes } from "@/lib/format";
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();
const selectedAppLanguage = computed({
  get: () => appLanguage.value,
  set: (value: string) => setAppLanguage(value),
});
const selectedAppLanguageLabel = computed(() =>
  supportedAppLanguages.find(({ code }) => code === appLanguage.value)?.label ?? "English",
);
const { stepKey: currentStep, stepIndex, markComplete } = useOnboarding();
const { permissions } = useMurmurEvents();
const stt = useOnboardingModel();

const tryItPhase = ref<"hold" | "toggle">("hold");
const holdDemoComplete = ref(false);
const toggleDemoComplete = ref(false);
const demoComplete = computed<boolean>(() => holdDemoComplete.value && toggleDemoComplete.value);
const currentTryItPhaseDone = computed<boolean>(() =>
  tryItPhase.value === "hold" ? holdDemoComplete.value : toggleDemoComplete.value,
);
const hasInputDevice = ref(false);
const microphoneStep = ref<{ stopForExit: () => Promise<boolean> } | null>(null);
const navigating = ref(false);
const privacyLinkError = ref(false);
async function openPrivacy(event: MouseEvent): Promise<void> {
  event.preventDefault();
  privacyLinkError.value = false;
  try { await openMurmurLink("privacy"); }
  catch { privacyLinkError.value = true; }
}
const prerequisitesPending = computed(() =>
  (stepIndex.value > ONBOARDING_STEPS.indexOf("permissions") && !allPermissionsGranted.value) ||
  (stepIndex.value > ONBOARDING_STEPS.indexOf("requiredModel") && !stt.operational.value),
);

const stepNumber = computed<number>(() => stepIndex.value + 1);
const totalSteps = ONBOARDING_STEPS.length;
const stepTitleKey = computed<string>(() => `onboarding.steps.${currentStep.value}.title`);
const stepSubtitleKey = computed<string>(() => `onboarding.steps.${currentStep.value}.subtitle`);

const allPermissionsGranted = computed<boolean>(() => {
  const p = permissions.value;
  return !!p && p.microphone === "granted" && p.input_monitoring === "granted" && p.accessibility === "granted";
});
const sttStatusKind = stt.state;

const modelApproxSize = computed<string>(() => {
  const status = stt.status.value;
  if (status.kind === "missing" || status.kind === "ready") {
    return formatBytes(status.info.approx_download_bytes);
  }
  if (status.kind === "downloading" && status.total) return formatBytes(status.total);
  return "488 MB";
});

const primaryCtaLabel = computed<string>(() => {
  switch (currentStep.value) {
    case "welcome":
      return t("onboarding.cta.getStarted");
    case "productOverview":
      return t("onboarding.cta.continue");
    case "permissions":
      return allPermissionsGranted.value
        ? t("onboarding.cta.continue")
        : t("onboarding.cta.grantRequired");
    case "microphone":
      return hasInputDevice.value
        ? t("onboarding.cta.continue")
        : t("onboarding.cta.connectMicrophone");
    case "requiredModel": {
      const kind = sttStatusKind.value;
      if (kind === "missing") {
        return t("onboarding.requiredModel.downloadButton", { size: modelApproxSize.value });
      }
      if (kind === "downloading") return t("onboarding.requiredModel.downloadingButton");
      if (kind === "error") return t("onboarding.requiredModel.retryButton");
      if (kind === "operational") return t("onboarding.cta.continue");
      if (kind === "mismatch") return t("onboarding.requiredModel.redownloadButton");
      if (kind === "verifying") return t("onboarding.requiredModel.verifying");
      if (kind === "activating") return t("onboarding.requiredModel.activating");
      return t("onboarding.requiredModel.checkingButton");
    }
    case "tryIt":
      if (!currentTryItPhaseDone.value) return t("onboarding.cta.completeDemos");
      return t(tryItPhase.value === "hold" ? "onboarding.tryIt.nextGestureButton" : "onboarding.cta.continue");
    case "ready":
      return t("onboarding.cta.openMurmur");
  }
});

const primaryDisabled = computed<boolean>(() => {
  if (navigating.value || prerequisitesPending.value) return true;
  switch (currentStep.value) {
    case "permissions":
      return !allPermissionsGranted.value;
    case "microphone":
      return !hasInputDevice.value;
    case "requiredModel":
      return stt.busy.value;
    case "tryIt":
      return !currentTryItPhaseDone.value;
    default:
      return false;
  }
});

onMounted(() => { void stt.prepare(); });

// If a persisted mid-flow step points beyond a now-missing prerequisite,
// pull the user back to the first hard-blocked setup step.
watch(
  [currentStep, permissions, sttStatusKind],
  () => {
    const permissionIdx = ONBOARDING_STEPS.indexOf("permissions");
    const modelIdx = ONBOARDING_STEPS.indexOf("requiredModel");
    const permissionSnapshot = permissions.value;
    const permissionsKnown = permissionSnapshot && Object.values(permissionSnapshot).some((state) => state === "denied" || state === "not-determined");
    if (stepIndex.value > permissionIdx && permissionsKnown && !allPermissionsGranted.value) {
      currentStep.value = "permissions";
      return;
    }
    if (
      stepIndex.value > modelIdx &&
      ["missing", "mismatch", "error", "downloading"].includes(sttStatusKind.value)
    ) {
      currentStep.value = "requiredModel";
    }
  },
  { immediate: true },
);

async function back(): Promise<void> {
  if (navigating.value) return;
  const leaving = currentStep.value;
  if (leaving === "microphone" && (!await stopMicrophone() || currentStep.value !== leaving)) return;
  if (currentStep.value === "tryIt" && tryItPhase.value === "toggle") {
    tryItPhase.value = "hold";
    return;
  }
  if (stepIndex.value > 0) currentStep.value = ONBOARDING_STEPS[stepIndex.value - 1];
}

async function stopMicrophone(): Promise<boolean> {
  navigating.value = true;
  try { return await microphoneStep.value?.stopForExit() ?? true; }
  finally { navigating.value = false; }
}

async function next(): Promise<void> {
  if (primaryDisabled.value) return;
  if (currentStep.value === "permissions" && !allPermissionsGranted.value) return;
  if (currentStep.value === "microphone" && !hasInputDevice.value) return;

  if (currentStep.value === "requiredModel") {
    if (stt.busy.value) return;
    if (!stt.operational.value) {
      void stt.retry();
      return;
    }
  }

  if (currentStep.value === "tryIt") {
    if (!currentTryItPhaseDone.value) return;
    if (tryItPhase.value === "hold") {
      tryItPhase.value = "toggle";
      return;
    }
    if (!demoComplete.value) return;
  }

  if (currentStep.value === "ready") {
    markComplete();
    return;
  }

  const leaving = currentStep.value;
  if (leaving === "microphone") {
    if (!await stopMicrophone() || currentStep.value !== leaving || prerequisitesPending.value) return;
  }
  currentStep.value = ONBOARDING_STEPS[Math.min(stepIndex.value + 1, totalSteps - 1)];
}
</script>

<template>
  <OnboardingFrame
    :title="t(stepTitleKey)"
    :subtitle="currentStep === 'productOverview' ? '' : t(stepSubtitleKey)"
    :step-label="t('onboarding.stepLabel', { current: stepNumber, total: totalSteps })"
    :current-step="stepNumber"
    :total-steps="totalSteps"
    :primary-label="primaryCtaLabel"
    :back-label="t('onboarding.cta.back')"
    :primary-disabled="primaryDisabled"
    :show-back="stepIndex > 0"
    :centered="currentStep === 'welcome' || currentStep === 'ready'"
    :center-welcome="currentStep === 'welcome'"
    :compact="currentStep === 'productOverview'"
    :bleed-right="currentStep === 'productOverview'"
    :focus-demo="currentStep === 'tryIt'"
    :motion-key="currentStep === 'tryIt' ? tryItPhase : ''"
    @back="back"
    @primary="next"
  >
    <template #secondary>
      <p v-if="currentStep === 'welcome'" class="min-w-0 text-right text-xs leading-5 text-muted-foreground">
        <a :href="MURMUR_URLS.privacy" target="_blank" rel="noopener" class="underline underline-offset-4" @click="openPrivacy">{{ t("onboarding.welcome.privacyLink") }}</a>
        <span v-if="privacyLinkError" role="alert" class="block text-destructive">{{ t('onboarding.welcome.privacyError') }}</span>
      </p>
    </template>

    <template #left>
      <div v-if="currentStep === 'welcome'" class="w-[min(100%,14rem)]">
        <Select v-model="selectedAppLanguage">
          <SelectTrigger id="onboarding-app-language" class="w-full" :aria-label="t('settingsAppLanguage.label')">
            <SelectValue>{{ selectedAppLanguageLabel }}</SelectValue>
          </SelectTrigger>
          <SelectContent class="dark">
            <SelectItem v-for="option in supportedAppLanguages" :key="option.code" :value="option.code">
              {{ option.label }}
            </SelectItem>
          </SelectContent>
        </Select>
      </div>
      <StepProductOverview v-else-if="currentStep === 'productOverview'" />
    </template>

    <template #right>
      <OnboardingHomePreview v-if="currentStep === 'productOverview'" />
      <StepPermissions v-else-if="currentStep === 'permissions'" />
      <StepMicrophone
        ref="microphoneStep"
        v-else-if="currentStep === 'microphone'"
        @availability="hasInputDevice = $event"
      />
      <StepRequiredModel v-else-if="currentStep === 'requiredModel'" />
      <StepTryIt
        v-else-if="currentStep === 'tryIt'"
        :active-phase="tryItPhase"
        :hold-complete="holdDemoComplete"
        :toggle-complete="toggleDemoComplete"
        @model-missing="stt.invalidate()"
        @hold-complete="holdDemoComplete = true"
        @toggle-complete="toggleDemoComplete = true"
      />
    </template>
  </OnboardingFrame>
</template>
