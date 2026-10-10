<script setup lang="ts">
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { useRouter } from "vue-router";

const { permissions } = useMurmurEvents();
const { t } = useI18n();
const router = useRouter();

// `not-determined` (mic on first launch before any prompt) is treated
// as missing too — the user still needs to act. `unknown` is the
// transient API-failure state and we don't nag for that.
const hasMissing = computed<boolean>(() => {
  const p = permissions.value;
  if (!p) return false;
  return [p.microphone, p.input_monitoring, p.accessibility].some(
    (s) => s === "denied" || s === "not-determined",
  );
});

function openPermissions(): void {
  void router.push("/settings/permissions");
}
</script>

<template>
  <div
    v-if="hasMissing"
    role="status"
    class="flex flex-col gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm text-amber-900 dark:text-amber-200 sm:flex-row sm:items-center sm:justify-between"
  >
    <div class="flex flex-col gap-0.5">
      <p class="font-medium leading-tight">{{ t("permissionsBanner.title") }}</p>
      <p class="text-xs text-amber-800/80 dark:text-amber-200/80 leading-snug">
        {{ t("permissionsBanner.body") }}
      </p>
    </div>
    <button
      type="button"
      class="shrink-0 rounded-md bg-amber-500 px-3 py-1.5 text-xs font-semibold text-white transition-colors hover:bg-amber-600 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-amber-500"
      @click="openPermissions"
    >
      {{ t("permissionsBanner.button") }}
    </button>
  </div>
</template>
