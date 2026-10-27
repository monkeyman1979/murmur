<script setup lang="ts">
import ModelRow from "@/components/settings/ModelRow.vue";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Card, CardContent } from "@/components/ui/card";
import { useActiveModels } from "@/composables/useActiveModels";
import { useModelDownload } from "@/composables/useModelDownload";
import {
  useModelRegistry,
  type ModelEntryDto,
  type ModelListItem,
} from "@/composables/useModelRegistry";
import { formatBytes } from "@/lib/format";
import { invoke } from "@tauri-apps/api/core";
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const { activeSttId } = useActiveModels();
const { models, refresh } = useModelRegistry();

const pendingDelete = ref<ModelEntryDto | null>(null);
const deleteDialogOpen = ref(false);

function downloadedCount(): number {
  return models.value.filter((m) => m.info.exists).length;
}

function canDelete(item: ModelListItem): boolean {
  if (item.entry.id === activeSttId.value) return false;
  if (!item.info.exists) return false;
  return downloadedCount() > 1;
}

function handleSetActive(item: ModelListItem): void {
  if (!item.info.exists) return;
  activeSttId.value = item.entry.id;
}

function handleDownload(item: ModelListItem): void {
  const dl = useModelDownload(item.entry.id);
  void dl.download().then(async () => {
    // Boot activation fails when this model is absent. Downloading the
    // already-selected id does not trigger the selection watcher again.
    if (activeSttId.value === item.entry.id) {
      await invoke("set_active_stt", { modelId: item.entry.id });
    }
  }).catch(() => {
    console.error("[murmur] model download failed");
  }).finally(() => void refresh());
}

function handleRedownload(item: ModelListItem): void {
  const dl = useModelDownload(item.entry.id);
  void dl.download({ force: true }).then(async () => {
    if (activeSttId.value === item.entry.id) {
      await invoke("set_active_stt", { modelId: item.entry.id });
    }
  }).catch(() => {
    console.error("[murmur] model re-download failed");
  }).finally(() => void refresh());
}

function handleVerify(item: ModelListItem): void {
  void useModelDownload(item.entry.id).verify().catch(() => {
    console.error("[murmur] model verification failed");
  });
}

async function handleReveal(id: string): Promise<void> {
  try {
    await invoke("reveal_model_in_finder", { id });
  } catch {
    console.error("[murmur] reveal model failed");
  }
}

function openDeleteDialog(entry: ModelEntryDto): void {
  pendingDelete.value = entry;
  deleteDialogOpen.value = true;
}

async function confirmDelete(): Promise<void> {
  const target = pendingDelete.value;
  if (!target) return;
  try {
    await invoke("delete_model", { modelId: target.id });
    if (activeSttId.value === target.id) {
      const remaining = models.value.filter(
        (m) => m.entry.id !== target.id && m.info.exists,
      );
      activeSttId.value = remaining[0]?.entry.id ?? activeSttId.value;
    }
    await refresh();
  } catch {
    console.error("[murmur] delete model failed");
  } finally {
    pendingDelete.value = null;
    deleteDialogOpen.value = false;
  }
}

function cancelDelete(): void {
  pendingDelete.value = null;
  deleteDialogOpen.value = false;
}

interface RowDownloadState {
  downloading: boolean;
  progressPercent: number;
  verifying: boolean;
  verifyError: string | null;
  verifySuccess: boolean;
  verifyPercent: number;
  verifyHashed: number;
  verifyTotal: number;
  downloadError: string | null;
}

function rowStateFor(id: string): RowDownloadState {
  const dl = useModelDownload(id);
  const status = dl.status.value;
  const verifyProgress = dl.verifyProgress.value;
  return {
    downloading: status.kind === "downloading",
    progressPercent:
      status.kind === "downloading" ? dl.progressPercent.value : 0,
    verifying: dl.verifying.value,
    verifyError: dl.verifyFailure.value,
    verifySuccess: dl.verifySuccess.value,
    verifyPercent: dl.verifyPercent.value,
    verifyHashed: verifyProgress?.hashed ?? 0,
    verifyTotal: verifyProgress?.total ?? 0,
    downloadError: status.kind === "error" ? status.message : null,
  };
}

const downloadStates = computed(() =>
  Object.fromEntries(
    models.value.map((item) => [item.entry.id, rowStateFor(item.entry.id)]),
  ),
);

const installedBytes = computed(() =>
  models.value
    .filter((m) => m.info.exists)
    .reduce((total, m) => total + m.info.size, 0),
);
</script>

<template>
  <div id="speech-model" class="flex min-w-0 scroll-mt-16 flex-col gap-4">
    <Card class="min-w-0">
      <CardContent class="flex flex-col gap-4 px-4 min-w-0">
        <div class="flex flex-col gap-1">
          <h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {{ t('speechModelSettings.heading') }}
          </h2>
          <p class="text-xs text-muted-foreground leading-snug">
            {{ t('speechModelSettings.help') }}
          </p>
        </div>

        <div class="flex flex-col divide-y divide-border min-w-0">
          <ModelRow
            v-for="item in models"
            :key="item.entry.id"
            :entry="item.entry"
            :info="item.info"
            :is-active="item.info.exists && item.entry.id === activeSttId"
            :control="models.length > 1 ? 'radio' : 'status'"
            :required="!item.info.exists && item.entry.id === activeSttId"
            :can-delete="canDelete(item)"
            :caption="item.entry.id === 'whisper-small-multilingual' ? t('speechModelSettings.captionSmall') : undefined"
            :downloading="downloadStates[item.entry.id]?.downloading"
            :progress-percent="downloadStates[item.entry.id]?.progressPercent"
            :verifying="downloadStates[item.entry.id]?.verifying"
            :verify-error="downloadStates[item.entry.id]?.verifyError"
            :verify-success="downloadStates[item.entry.id]?.verifySuccess"
            :verify-percent="downloadStates[item.entry.id]?.verifyPercent"
            :verify-hashed="downloadStates[item.entry.id]?.verifyHashed"
            :verify-total="downloadStates[item.entry.id]?.verifyTotal"
            :download-error="downloadStates[item.entry.id]?.downloadError"
            @set-active="handleSetActive(item)"
            @download="handleDownload(item)"
            @redownload="handleRedownload(item)"
            @reveal="handleReveal(item.entry.id)"
            @verify="handleVerify(item)"
            @delete="openDeleteDialog(item.entry)"
          />
        </div>

        <p v-if="installedBytes > 0" class="text-xs text-muted-foreground tabular-nums">
          {{ t('speechModelSettings.storage', { size: formatBytes(installedBytes) }) }}
        </p>
      </CardContent>
    </Card>

    <AlertDialog :open="deleteDialogOpen">
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('speechModelSettings.deleteTitle') }}</AlertDialogTitle>
          <AlertDialogDescription>
            <i18n-t keypath="speechModelSettings.deleteDescription" scope="global" tag="span">
              <template #name><strong>{{ pendingDelete?.display_name }}</strong></template>
            </i18n-t>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel @click="cancelDelete">{{ t('meetingDialogs.cancel') }}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" @click="confirmDelete">
            {{ t('speechModelSettings.delete') }}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>
</template>
