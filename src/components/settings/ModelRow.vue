<script setup lang="ts">
import { IconTooltip } from "@/components/ui/tooltip";
import type { ModelEntryDto } from "@/composables/useModelRegistry";
import type { ModelInfo } from "@/composables/useModelDownload";
import { formatBytes } from "@/lib/format";
import { useI18n } from "vue-i18n";

const { t } = useI18n();
import {
  CheckCircle2,
  Download,
  FolderOpen,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from "lucide-vue-next";

defineProps<{
  entry: ModelEntryDto;
  info: ModelInfo;
  isActive: boolean;
  canDelete: boolean;
  control: "radio" | "status";
  downloading?: boolean;
  progressPercent?: number;
  /** True while a manual `verify_model` pass is running for this row. */
  verifying?: boolean;
  /** Persistent hash-mismatch message from the last verify pass (or the
   *  IPC error if the verify call itself failed). Cleared on a
   *  successful download or a passing re-verify. */
  verifyError?: string | null;
  /** Transient "Verified ✓" flash shown briefly after a passing verify.
   *  Auto-clears via a timer in the composable — the row just reacts
   *  to the flag. */
  verifySuccess?: boolean;
  /** Percent hashed during an in-flight verify pass. 0 until the first
   *  `murmur://model-verify` event lands (the backend fires a zeroed
   *  event on start so the determinate bar renders from t=0). */
  verifyPercent?: number;
  /** Bytes hashed so far during the in-flight verify. Lets the row
   *  render "X GB of Y GB" alongside the bar. */
  verifyHashed?: number;
  /** Total bytes the backend will hash (file size). Fixed across the
   *  lifetime of one pass. */
  verifyTotal?: number;
  /** Last download-path error surfaced by `useModelDownload` for this
   *  id. Presence here tells the row to render a failure subtitle with
   *  a retry affordance (the existing Download / Re-download button). */
  downloadError?: string | null;
  /** Marks a missing model as blocking core functionality, not merely
   *  optional. Used for the required speech-to-text model so the row
   *  can show an always-visible install affordance instead of a
   *  misleading inactive/active selector. */
  required?: boolean;
  /** Plain-language one-liner on the accuracy-vs-speed tradeoff of
   *  this entry. Rendered as a muted subtitle next to the "Not
   *  installed" line when present and the row has no louder state
   *  (downloading / verifying / error) to display. */
  caption?: string;
}>();

const emit = defineEmits<{
  (e: "set-active"): void;
  (e: "download"): void;
  (e: "redownload"): void;
  (e: "reveal"): void;
  (e: "delete"): void;
  (e: "verify"): void;
}>();

// Alias the shared helper locally so template bindings below stay as
// `formatApproxBytes(...)` — keeps the diff tight and the name
// self-documenting at the call site ("the approximate size we show
// the user").
const formatApproxBytes = formatBytes;

</script>

<template>
  <div class="group flex items-center gap-3 py-2.5 text-sm">
    <!-- Left: control. The h-5 wrapper matches the text-sm leading-snug
         line-box so the radio centers against the display name's visual
         center instead of the taller row track the hover-only action
         buttons create. -->
    <div class="flex h-5 shrink-0 items-center">
      <!-- Required/non-toggle models: show install/progress/status. Use a
           radio only when there are multiple installed STT choices. -->
      <IconTooltip v-if="!info.exists && !downloading" :label="t('speechModelSettings.downloadModel')">
        <button
          type="button"
          class="inline-flex size-6 cursor-pointer items-center justify-center rounded-full border border-amber-500/60 bg-amber-500/10 text-amber-600 hover:bg-amber-500/20 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-amber-500 dark:text-amber-400"
          :aria-label="t('speechModelSettings.downloadModel')"
          @click="emit('download')"
        >
          <Download class="size-3.5" aria-hidden="true" />
        </button>
      </IconTooltip>

      <span
        v-else-if="downloading"
        class="inline-flex size-6 items-center justify-center rounded-full border border-muted-foreground/40 text-muted-foreground"
        :aria-label="t('speechModelSettings.downloadingModel')"
      >
        <RefreshCw class="size-3.5 animate-spin" aria-hidden="true" />
      </span>

      <IconTooltip v-else-if="control === 'status'" :label="t('speechModelSettings.installed')">
        <span
          class="inline-flex size-6 items-center justify-center rounded-full border border-success-border bg-success-muted text-success"
          :aria-label="t('speechModelSettings.modelInstalled')"
        >
          <CheckCircle2 class="size-3.5" aria-hidden="true" />
        </span>
      </IconTooltip>

      <button
        v-else-if="control === 'radio'"
        type="button"
        role="radio"
        :aria-checked="isActive"
        :aria-label="entry.display_name"
        class="relative size-4 rounded-full border-2 transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        :class="
          isActive
            ? 'border-primary bg-primary'
            : 'border-muted-foreground bg-transparent hover:border-foreground'
        "
        @click="emit('set-active')"
      >
        <span
          v-if="isActive"
          class="absolute inset-1 rounded-full bg-primary-foreground"
          aria-hidden="true"
        />
      </button>

    </div>

    <!-- Middle: metadata -->
    <div class="flex flex-1 flex-col gap-1 leading-snug min-w-0">
      <span class="text-foreground font-medium">
        {{ entry.display_name }}
        <span class="text-muted-foreground font-normal">
          ~{{ formatApproxBytes(entry.approx_bytes) }}
        </span>
      </span>

      <!-- Downloading state: progress bar -->
      <template v-if="downloading">
        <div class="flex flex-col gap-1">
          <div
            class="h-1.5 w-full overflow-hidden rounded-full bg-muted"
            role="progressbar"
            :aria-valuenow="Math.round(progressPercent ?? 0)"
            aria-valuemin="0"
            aria-valuemax="100"
          >
            <div
              class="h-full bg-primary transition-[width] duration-200"
              :style="{ width: `${progressPercent ?? 0}%` }"
            />
          </div>
          <span class="text-xs text-muted-foreground tabular-nums">
            {{ t('speechModelSettings.downloading', { percent: Math.round(progressPercent ?? 0) }) }}
          </span>
        </div>
      </template>

      <!-- Verifying state — determinate progress bar during the hash.
           Backend fires an initial zeroed event on start so the bar
           renders from t=0; intermediate events land every 8 MiB.
           Shape mirrors the download progress above so the two states
           look consistent. -->
      <template v-else-if="verifying">
        <div class="flex flex-col gap-1">
          <div
            class="h-1.5 w-full overflow-hidden rounded-full bg-muted"
            role="progressbar"
            :aria-valuenow="Math.round(verifyPercent ?? 0)"
            aria-valuemin="0"
            aria-valuemax="100"
          >
            <div
              class="h-full bg-primary transition-[width] duration-200"
              :style="{ width: `${verifyPercent ?? 0}%` }"
            />
          </div>
          <span class="text-xs text-muted-foreground tabular-nums">
            {{ t('speechModelSettings.verifying', { percent: Math.round(verifyPercent ?? 0) }) }}<template v-if="verifyTotal && verifyTotal > 0">
              · {{ t('speechModelSettings.bytes', { hashed: formatApproxBytes(verifyHashed ?? 0), total: formatApproxBytes(verifyTotal) }) }}
            </template>
          </span>
        </div>
      </template>

      <!-- Brief "Verified ✓" confirmation after a passing verify.
           Auto-clears from the composable side so it doesn't pile up
           stale green across rows; native notification was unreliable
           (Focus mode, silenced Notification Center, just missed it)
           so the inline flash is the primary success signal. -->
      <template v-else-if="verifySuccess">
        <span class="inline-flex items-center gap-1.5 text-xs text-success">
          <CheckCircle2 class="size-3" aria-hidden="true" />
          {{ t('speechModelSettings.verified') }}
        </span>
      </template>

      <!-- Hash mismatch from the last verify pass persists until the
           user re-downloads or re-verifies. Destructive color signals
           "this file is wrong"; the Re-download action icon to the
           right is the fix. -->
      <template v-else-if="verifyError">
        <span class="text-xs text-destructive">{{ verifyError }}</span>
      </template>

      <!-- Download errors remain inline with the row; retry through the
           existing Download / Re-download action. -->
      <span v-else-if="downloadError" class="text-xs text-destructive">
        {{ downloadError }}
      </span>

      <!-- Not installed — always surface the caption (when present)
           alongside the install state so the user sees the accuracy-
           vs-speed tradeoff before clicking Download rather than
           after. Middot separator keeps it one line. -->
      <template v-else-if="!info.exists">
        <span
          class="text-xs"
          :class="required ? 'text-amber-600 dark:text-amber-400' : 'text-muted-foreground'"
        >
          <template v-if="required">
            {{ t('speechModelSettings.required') }}
          </template>
          <template v-else>
            {{ t('speechModelSettings.notInstalled') }}<template v-if="caption"> · {{ caption }}</template>
          </template>
        </span>
        <span v-if="required && caption" class="text-xs text-muted-foreground">
          {{ caption }}
        </span>
      </template>

      <!-- Installed, no louder state — show the caption on its own so
           users who already picked a model still have a passive
           reminder of what each row optimizes for. -->
      <template v-else-if="caption">
        <span class="text-xs text-muted-foreground">{{ caption }}</span>
      </template>
    </div>

    <!-- Right: actions. Required missing models get an always-visible
         text button; optional/maintenance actions stay hover-revealed so
         installed rows don't look cluttered. -->
    <div
      class="flex shrink-0 items-center gap-1 transition-opacity"
      :class="
        !info.exists && required
          ? 'opacity-100'
          : 'opacity-0 group-hover:opacity-100 focus-within:opacity-100'
      "
    >
      <!-- Required download: persistent and explicit. The old icon-only,
           hover-revealed affordance looked too much like a secondary
           action for the one required STT model. -->
      <button
        v-if="!info.exists && required && !downloading"
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        :aria-label="t('speechModelSettings.downloadRequired')"
        @click="emit('download')"
      >
        <Download class="size-3.5" aria-hidden="true" />
        {{ t('speechModelSettings.download') }}
      </button>

      <!-- Optional download: icon-only, hover-revealed. -->
      <IconTooltip v-else-if="!info.exists && !downloading" :label="t('speechModelSettings.downloadModel')">
        <button
          type="button"
          class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
          :aria-label="t('speechModelSettings.downloadModel')"
          @click="emit('download')"
        >
          <Download class="size-3.5" aria-hidden="true" />
        </button>
      </IconTooltip>

      <!-- Lifecycle maintenance actions disappear as a group while download
           or verification owns the native model lock. This prevents conflicting
           re-download/reveal/delete clicks against changing bytes. -->
      <template v-if="info.exists && !downloading && !verifying">
        <!-- Verify: re-hash the installed bytes against the registry pin. -->
        <IconTooltip :label="t('speechModelSettings.verifyHint')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('speechModelSettings.verifyLabel')"
            @click="emit('verify')"
          >
            <ShieldCheck class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>

        <!-- Re-download -->
        <IconTooltip :label="t('speechModelSettings.redownload')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('speechModelSettings.redownload')"
            @click="emit('redownload')"
          >
            <RefreshCw class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>

        <!-- Show in Finder -->
        <IconTooltip :label="t('speechModelSettings.reveal')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('speechModelSettings.reveal')"
            @click="emit('reveal')"
          >
            <FolderOpen class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>

        <!-- Only an eligible inactive model can show Delete. The native guard stays. -->
        <IconTooltip v-if="canDelete" :label="t('speechModelSettings.deleteModel')">
          <button
            type="button"
            class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-destructive/10 hover:text-destructive focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:opacity-100"
            :aria-label="t('speechModelSettings.deleteModel')"
            @click="emit('delete')"
          >
            <Trash2 class="size-3.5" aria-hidden="true" />
          </button>
        </IconTooltip>
      </template>
    </div>
  </div>
</template>
