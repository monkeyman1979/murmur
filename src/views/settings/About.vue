<script setup lang="ts">
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import SupportDiagnosticsDialog from "@/components/settings/SupportDiagnosticsDialog.vue";
import { useUpdateActions } from "@/composables/useUpdateActions";
import { MURMUR_URLS, openMurmurLink } from "@/lib/externalLinks";
import { getVersion } from "@tauri-apps/api/app";
import { RefreshCw } from "lucide-vue-next";
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();
// The full update flow lives here too, so it stays reachable when the
// sidebar is collapsed or its card was dismissed.
const {
  updater,
  settingsStatus,
  settingsActionLabel,
  settingsActionDisabled,
  runSettingsAction,
} = useUpdateActions();

const version = ref<string>("—");
const diagnosticsOpen = ref(false);
const linkError = ref(false);
async function openExternal(event: MouseEvent): Promise<void> {
  event.preventDefault();
  linkError.value = false;
  try { await openMurmurLink("report"); }
  catch { linkError.value = true; }
}
onMounted(async () => {
  try {
    version.value = await getVersion();
  } catch (err) {
    console.error("version lookup failed", err);
  }
});

</script>

<template>
  <div class="flex flex-col gap-4 p-4">

    <Card>
      <CardHeader class="pb-2">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <CardTitle class="text-base">{{ t('aboutPage.support') }}</CardTitle>
          <div class="flex flex-wrap items-center gap-2">
            <Button as-child size="sm" variant="ghost">
              <a :href="MURMUR_URLS.report" target="_blank" rel="noopener" @click="openExternal($event)">{{ t('aboutPage.report') }}</a>
            </Button>
            <Button size="sm" variant="outline" @click="diagnosticsOpen = true">
              {{ t('aboutPage.preview') }}
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        <p v-if="linkError" role="alert" class="mb-2 text-xs text-destructive">{{ t('aboutPage.reportError') }}</p>
        <p class="text-xs text-muted-foreground leading-snug">
          {{ t('aboutPage.supportBody') }}
        </p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader class="pb-2">
        <CardTitle class="text-base">{{ t('aboutPage.title') }}</CardTitle>
      </CardHeader>
      <CardContent>
        <dl class="grid gap-2 text-sm">
          <div class="flex justify-between">
            <dt class="text-muted-foreground">{{ t('aboutPage.version') }}</dt>
            <dd class="font-mono tabular-nums">{{ version }}</dd>
          </div>
          <div class="flex justify-between">
            <dt class="text-muted-foreground">{{ t('aboutPage.bundle') }}</dt>
            <dd class="font-mono">com.monkeyman.murmur</dd>
          </div>
          <div class="flex justify-between">
            <dt class="text-muted-foreground">{{ t('aboutPage.madeBy') }}</dt>
            <dd>monkeyman1979 (GitHub)</dd>
          </div>
          <div class="flex justify-between">
            <dt class="text-muted-foreground">{{ t('aboutPage.sourceLicense') }}</dt>
            <dd>MIT</dd>
          </div>
        </dl>
        <div
          v-if="updater.enabled"
          data-test="about-updates"
          class="mt-3 flex flex-wrap items-center justify-between gap-3 border-t border-border/60 pt-3"
        >
          <div class="flex min-w-0 flex-1 flex-col gap-1">
            <p class="text-sm font-medium">{{ t('generalSettings.updates') }}</p>
            <p class="text-xs text-muted-foreground leading-snug" aria-live="polite">{{ settingsStatus }}</p>
            <div
              v-if="updater.phase.value === 'downloading'"
              class="h-1 max-w-64 overflow-hidden rounded-full bg-muted"
              role="progressbar"
              :aria-label="t('updatesCard.downloading')"
              :aria-valuenow="updater.progress.value === null ? undefined : Math.round(updater.progress.value * 100)"
            >
              <div
                class="h-full rounded-full bg-primary transition-[width]"
                :class="updater.progress.value === null ? 'w-1/3 animate-pulse' : ''"
                :style="updater.progress.value === null ? undefined : { width: `${updater.progress.value * 100}%` }"
              />
            </div>
          </div>
          <Button
            v-if="settingsActionLabel"
            size="sm"
            variant="outline"
            :disabled="settingsActionDisabled"
            @click="runSettingsAction"
          >
            <RefreshCw v-if="updater.phase.value === 'checking'" class="size-3.5 animate-spin" aria-hidden="true" />
            {{ settingsActionLabel }}
          </Button>
        </div>
        <p class="text-xs text-muted-foreground leading-snug text-balance pt-4">
          {{ t('aboutPage.body') }}
        </p>
        <p class="text-xs text-muted-foreground leading-snug text-balance pt-2">
          {{ t('aboutPage.copyright') }}
        </p>
        <div class="pt-4">
          <Button as-child size="sm" variant="outline">
            <RouterLink to="/settings/acknowledgements">
              {{ t('aboutPage.acknowledgements') }}
            </RouterLink>
          </Button>
        </div>
      </CardContent>
    </Card>

    <SupportDiagnosticsDialog v-model:open="diagnosticsOpen" />
  </div>
</template>
