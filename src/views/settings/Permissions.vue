<script setup lang="ts">
import PermissionRow from "@/components/settings/PermissionRow.vue";
import { Card, CardContent } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { computed } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const { permissions } = useMurmurEvents();

// Each row reads directly from the live snapshot. While `permissions`
// is still `null` (very brief — until the first `murmur://permissions`
// event lands), rows render "Checking…" with a neutral dot.
const mic = computed(() => permissions.value?.microphone ?? null);
const inputMonitoring = computed(
  () => permissions.value?.input_monitoring ?? null,
);
const accessibility = computed(
  () => permissions.value?.accessibility ?? null,
);
</script>

<template>
  <div class="flex flex-col gap-4 p-4">

    <Card>
      <CardContent class="flex flex-col gap-0 px-4 py-2">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-2">
          {{ t('permissionSettings.heading') }}
        </h2>

        <PermissionRow
          :title="t('permissionSettings.microphone')"
          :description="t('permissionSettings.microphoneHelp')"
          :state="mic"
          pane="microphone"
        />
        <Separator />
        <PermissionRow
          :title="t('permissionSettings.inputMonitoring')"
          :description="t('permissionSettings.inputHelp')"
          :state="inputMonitoring"
          pane="input-monitoring"
        />
        <Separator />
        <PermissionRow
          :title="t('permissionSettings.accessibility')"
          :description="t('permissionSettings.accessibilityHelp')"
          :state="accessibility"
          pane="accessibility"
        />
      </CardContent>
    </Card>

    <p class="text-xs text-muted-foreground leading-snug text-balance px-1">
      {{ t('permissionSettings.refreshHelp') }}
    </p>
  </div>
</template>
