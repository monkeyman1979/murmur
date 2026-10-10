<script setup lang="ts">
import {
  MULTILINGUAL_MODEL_ID,
  useModelDownload,
} from "@/composables/useModelDownload";
import { computed, onMounted } from "vue";
import { useI18n } from "vue-i18n";
import { useRouter } from "vue-router";

const { t } = useI18n();
const router = useRouter();
const model = useModelDownload(MULTILINGUAL_MODEL_ID);

onMounted(() => {
  void model.refresh();
});

const visible = computed<boolean>(() =>
  ["missing", "downloading", "error"].includes(model.status.value.kind),
);

const isDownloading = computed<boolean>(
  () => model.status.value.kind === "downloading",
);
const isError = computed<boolean>(() => model.status.value.kind === "error");
const progress = computed<number>(() => Math.round(model.progressPercent.value));

function openSpeechModelSettings(): void {
  void router.push({
    name: "settings-general",
    query: { section: "speech-model" },
  });
}
</script>

<template>
  <div
    v-if="visible"
    role="status"
    class="flex flex-col gap-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm text-amber-900 dark:text-amber-200 sm:flex-row sm:items-center sm:justify-between"
  >
    <div class="flex flex-1 flex-col gap-1">
      <p class="font-medium leading-tight">
        {{ isError ? t("speechModelBanner.errorTitle") : t("speechModelBanner.title") }}
      </p>
      <p class="text-xs text-amber-800/80 dark:text-amber-200/80 leading-snug">
        <template v-if="isDownloading">
          {{ t("speechModelBanner.downloading", { percent: progress }) }}
        </template>
        <template v-else-if="isError">
          {{ t("speechModelBanner.errorBody") }}
        </template>
        <template v-else>
          {{ t("speechModelBanner.body") }}
        </template>
      </p>
      <div
        v-if="isDownloading"
        class="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-amber-950/10 dark:bg-amber-50/15"
        role="progressbar"
        :aria-valuenow="progress"
        aria-valuemin="0"
        aria-valuemax="100"
      >
        <div
          class="h-full bg-amber-500 transition-[width] duration-200"
          :style="{ width: `${progress}%` }"
        />
      </div>
    </div>
    <button
      v-if="!isDownloading"
      type="button"
      class="shrink-0 rounded-md bg-amber-500 px-3 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-amber-600 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-amber-500"
      @click="openSpeechModelSettings"
    >
      {{ isError ? t("speechModelBanner.retryButton") : t("speechModelBanner.button") }}
    </button>
  </div>
</template>
