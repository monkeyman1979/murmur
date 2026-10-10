<script setup lang="ts">
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatSupportDiagnostics } from "@/lib/supportDiagnostics";
import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ (event: "update:open", value: boolean): void }>();

const report = ref("");
const loading = ref(false);
const copying = ref(false);
const error = ref(false);
const copied = ref(false);
let lifecycle = 0;
let generationOperation = 0;
let copyOperation = 0;

watch(
  () => props.open,
  (open) => {
    ++lifecycle;
    ++generationOperation;
    ++copyOperation;
    report.value = "";
    loading.value = false;
    copying.value = false;
    error.value = false;
    copied.value = false;
    if (open) void generate();
  },
  { immediate: true },
);

async function generate(): Promise<void> {
  if (!props.open || loading.value) return;
  const expectedLifecycle = lifecycle;
  const operation = ++generationOperation;
  loading.value = true;
  error.value = false;
  copied.value = false;
  try {
    const nextReport = formatSupportDiagnostics(await invoke("support_diagnostics"));
    if (props.open && lifecycle === expectedLifecycle && generationOperation === operation) {
      report.value = nextReport;
    }
  } catch {
    console.error("[murmur] support diagnostics generation failed");
    if (props.open && lifecycle === expectedLifecycle && generationOperation === operation) {
      error.value = true;
    }
  } finally {
    if (lifecycle === expectedLifecycle && generationOperation === operation) {
      loading.value = false;
    }
  }
}

async function copy(): Promise<void> {
  const preview = report.value;
  if (!props.open || !preview || copying.value) return;
  const expectedLifecycle = lifecycle;
  const operation = ++copyOperation;
  copying.value = true;
  copied.value = false;
  error.value = false;
  try {
    await navigator.clipboard.writeText(preview);
    if (
      props.open
      && lifecycle === expectedLifecycle
      && copyOperation === operation
      && report.value === preview
    ) {
      copied.value = true;
    }
  } catch {
    console.error("[murmur] support diagnostics copy failed");
    if (
      props.open
      && lifecycle === expectedLifecycle
      && copyOperation === operation
      && report.value === preview
    ) {
      error.value = true;
    }
  } finally {
    if (lifecycle === expectedLifecycle && copyOperation === operation) {
      copying.value = false;
    }
  }
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <DialogContent class="sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>{{ t('supportDiagnostics.title') }}</DialogTitle>
        <DialogDescription>
          {{ t('supportDiagnostics.description') }}
        </DialogDescription>
      </DialogHeader>

      <p v-if="loading" class="text-sm text-muted-foreground">{{ t('supportDiagnostics.generating') }}</p>
      <p v-else-if="error && !report" class="text-sm text-destructive" role="alert">
        {{ t('supportDiagnostics.generationFailed') }}
      </p>
      <textarea
        v-else
        :value="report"
        readonly
        rows="16"
        class="w-full resize-y rounded-md border border-input bg-muted/30 p-3 font-mono text-xs leading-relaxed text-foreground"
        :aria-label="t('supportDiagnostics.preview')"
      />

      <p v-if="copied" class="text-xs text-success" role="status">
        {{ t('supportDiagnostics.copied') }}
      </p>
      <p v-else-if="error && report" class="text-xs text-destructive" role="alert">
        {{ t('supportDiagnostics.copyFailed') }}
      </p>

      <DialogFooter>
        <Button variant="outline" @click="emit('update:open', false)">{{ t('supportDiagnostics.close') }}</Button>
        <Button v-if="error && !report" variant="outline" :disabled="loading || copying" @click="generate">
          {{ t('supportDiagnostics.retry') }}
        </Button>
        <Button :disabled="loading || copying || !report" @click="copy">
          {{ t(copying ? 'supportDiagnostics.copying' : 'supportDiagnostics.copy') }}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
