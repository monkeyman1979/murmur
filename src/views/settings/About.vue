<script setup lang="ts">
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import SupportDiagnosticsDialog from "@/components/settings/SupportDiagnosticsDialog.vue";
import { MURMUR_URLS, openMurmurLink } from "@/lib/externalLinks";
import { getVersion } from "@tauri-apps/api/app";
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

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
