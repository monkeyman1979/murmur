<script setup lang="ts">
import { useOnboardingModel } from "@/composables/useOnboardingModel";
import { formatBytes } from "@/lib/format";
import { CheckCircle2, Globe2, TriangleAlert } from "lucide-vue-next";
import { computed, onMounted } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();
const model = useOnboardingModel();

const statusKind = model.state;
const progressPct = computed<number>(() => Math.round(statusKind.value === "verifying" ? model.verifyPercent.value : model.progressPercent.value));

const approxSize = computed<string>(() => {
  const status = model.status.value;
  if (status.kind === "missing" || status.kind === "ready") {
    return formatBytes(status.info.approx_download_bytes);
  }
  if (status.kind === "downloading" && status.total) return formatBytes(status.total);
  return "488 MB";
});

const downloadedLabel = computed<string>(() => {
  const status = model.status.value;
  const verification = model.verifyProgress.value;
  if (statusKind.value === "verifying" && verification) return `${formatBytes(verification.hashed)} / ${formatBytes(verification.total)}`;
  if (status.kind !== "downloading") return "";
  const downloaded = formatBytes(status.downloaded);
  const total = status.total ? formatBytes(status.total) : approxSize.value;
  return `${downloaded} / ${total}`;
});

onMounted(() => { void model.prepare(); });
</script>

<template>
  <div
    aria-live="polite"
    class="rounded-[2rem] border border-border bg-card p-6 "
  >
    <div class="rounded-[1.5rem] bg-muted/50 px-6 py-8 text-center">
      <Globe2 class="mx-auto size-10 text-primary" aria-hidden="true" />
      <p class="mt-3 text-3xl font-semibold tracking-[-0.04em] text-primary">
        Whisper
      </p>
    </div>

    <div class="mt-6">
      <p class="text-sm font-medium uppercase tracking-[0.16em] text-muted-foreground">
        {{ t("onboarding.requiredModel.requiredLabel") }}
      </p>
      <h2 class="mt-2 text-2xl font-semibold tracking-tight text-foreground">
        {{ t("onboarding.requiredModel.cardTitle") }}
      </h2>
      <p class="mt-3 text-sm leading-6 text-muted-foreground">
        {{ t("onboarding.requiredModel.cardBody", { size: approxSize }) }}
      </p>

      <div
        v-if="(statusKind === 'downloading' || statusKind === 'verifying')"
        class="mt-4 rounded-2xl border border-input bg-muted p-4"
        data-testid="model-progress"
      >
        <div class="mb-2 flex items-center justify-between gap-3 text-sm tabular-nums">
          <span class="font-medium text-foreground">{{ t(statusKind === "verifying" ? "onboarding.requiredModel.verifying" : "onboarding.requiredModel.downloading") }}</span>
          <span class="text-muted-foreground">{{ progressPct }}%</span>
        </div>
        <div class="h-2.5 overflow-hidden rounded-full progress-track">
          <div
            class="h-full rounded-full bg-foreground transition-[width] duration-300"
            :style="{ width: `${progressPct}%` }"
          />
        </div>
        <div class="mt-2 text-right text-xs tabular-nums text-muted-foreground">
          {{ downloadedLabel }}
        </div>
      </div>

      <div
        v-if="statusKind === 'operational'"
        class="mt-4 flex items-center gap-3 rounded-2xl border border-success-border bg-success-muted/60 p-4 text-foreground"
        data-testid="model-ready"
      >
        <CheckCircle2 class="size-5 shrink-0 text-success" aria-hidden="true" />
        <p class="font-semibold text-success">{{ t("onboarding.requiredModel.completeTitle") }}</p>
      </div>

      <div
        v-if="statusKind === 'error' || statusKind === 'mismatch'"
        class="mt-4 flex gap-3 rounded-2xl border border-border bg-muted/50 p-4 text-foreground"
      >
        <TriangleAlert class="mt-0.5 size-5 shrink-0" aria-hidden="true" />
        <div>
          <p class="font-semibold">{{ t("onboarding.requiredModel.errorTitle") }}</p>
          <p class="mt-1 text-sm leading-6">{{ t(statusKind === "mismatch" ? "onboarding.requiredModel.mismatch" : `onboarding.requiredModel.errors.${model.failedStage.value}`) }}</p>
        </div>
      </div>

      <p v-if="statusKind === 'checking' || statusKind === 'activating' || statusKind === 'missing'" class="mt-3 text-sm leading-6 text-muted-foreground">
        {{ t(`onboarding.requiredModel.${statusKind === 'missing' ? 'readyToDownload' : statusKind}`, { size: approxSize }) }}
      </p>
    </div>
  </div>
</template>
