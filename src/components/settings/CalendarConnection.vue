<script setup lang="ts">
import { computed, ref, useId } from "vue";
import { Check, ChevronRight } from "lucide-vue-next";
import { CheckboxIndicator, CheckboxRoot } from "reka-ui";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { useI18n } from "vue-i18n";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import SettingRow from "@/components/settings/SettingRow.vue";
import { REMINDER_TIMINGS, useCalendarReminders } from "@/composables/useCalendarReminders";
import { useCalendarConnection } from "@/composables/useCalendarConnection";

const { t } = useI18n();
const id = useId();
const privacyOpen = ref(false);
const accountsOpen = ref(false);
const {
  connected, permission, snapshot, pending, failed, selectedIds, selectedCount,
  selectionStorageFailed, connect, disconnect, setAccount,
} = useCalendarConnection();
const {
  enabled: remindersEnabled, leadSeconds, pending: reminderPending, failed: reminderFailed,
  setEnabled: setRemindersEnabled, setLeadSeconds,
} = useCalendarReminders();
const timingValue = computed({
  get: () => String(leadSeconds.value),
  set: (value: string) => { void setLeadSeconds(Number(value)); },
});
function timingLabel(seconds: number): string {
  return t(seconds === 0 ? "calendarConnection.timingAtStart"
    : seconds === 60 ? "calendarConnection.timingMinute" : "calendarConnection.timingSeconds", { seconds });
}
const selected = computed(() => new Set(selectedIds.value));
const accounts = computed(() => snapshot.value?.accounts ?? []);
const blocked = computed(() => permission.value === "denied" || permission.value === "write-only");
const checkboxClass = "inline-flex size-3.5 shrink-0 appearance-none items-center justify-center rounded-[3px] border border-input bg-background text-primary-foreground outline-none data-[state=checked]:border-primary data-[state=checked]:bg-primary focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:cursor-wait";
async function disconnectCalendars(): Promise<void> {
  await setRemindersEnabled(false);
  accountsOpen.value = false;
  disconnect();
}

async function manageAccounts(): Promise<void> {
  if (accountsOpen.value && snapshot.value) {
    accountsOpen.value = false;
    return;
  }
  accountsOpen.value = true;
  if (!snapshot.value) await connect();
}

async function openSettings(): Promise<void> {
  try { await invoke("open_privacy_pane", { pane: "calendars" }); }
  catch { /* Retrying the connection remains available without exposing native errors. */ }
}
</script>

<template>
  <section :aria-labelledby="`${id}-title`" class="flex min-w-0 flex-col gap-3 py-3">
    <header class="flex flex-wrap items-center justify-between gap-x-8 gap-y-3">
      <!-- Match SettingRow's title/description spacing. The minimum flex basis
           wraps controls before they can crowd the bounded description. -->
      <div class="flex min-w-0 basis-64 grow flex-col gap-0.5">
        <h3 :id="`${id}-title`" class="text-sm font-medium leading-tight">{{ t('calendarConnection.title') }}</h3>
        <p class="w-[min(100%,65ch)] text-balance text-xs leading-snug text-muted-foreground">{{ t('calendarConnection.description') }}</p>
      </div>
      <Button
        variant="outline" size="sm" class="shrink-0" :class="{ 'disabled:opacity-100': connected }"
        :disabled="pending || permission === 'unsupported'" :aria-expanded="accountsOpen && !!snapshot"
        :aria-controls="snapshot ? `${id}-accounts` : undefined" @click="manageAccounts"
      >
        {{ t(connected ? 'calendarConnection.manageAccounts' : pending ? 'calendarConnection.connecting' : 'calendarConnection.connect') }}
      </Button>
    </header>
    <SettingRow :title="t('calendarConnection.remindersTitle')" :description="t('calendarConnection.remindersHelp')">
      <div class="flex items-center gap-3">
        <Select v-if="remindersEnabled" v-model="timingValue" :disabled="reminderPending">
          <SelectTrigger class="w-44 max-w-full" :aria-label="t('calendarConnection.timingTitle')">
            <SelectValue>{{ timingLabel(leadSeconds) }}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem v-for="seconds in REMINDER_TIMINGS" :key="seconds" :value="String(seconds)">{{ timingLabel(seconds) }}</SelectItem>
          </SelectContent>
        </Select>
        <Switch :model-value="remindersEnabled" :aria-label="t('calendarConnection.remindersTitle')" :disabled="reminderPending || (!remindersEnabled && (!connected || !selectedIds.length))" @update:model-value="setRemindersEnabled" />
      </div>
    </SettingRow>
    <Collapsible v-model:open="privacyOpen">
      <CollapsibleTrigger class="group inline-flex items-center gap-1 text-xs text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring">
        <ChevronRight aria-hidden="true" class="size-3 transition-transform duration-200 group-data-[state=open]:rotate-90 motion-reduce:transition-none" />
        {{ t('calendarConnection.privacy') }}
      </CollapsibleTrigger>
      <CollapsibleContent class="calendar-disclosure">
        <p class="w-[min(100%,65ch)] pt-2 text-xs leading-snug text-muted-foreground">{{ t('calendarConnection.permissionHelp') }}</p>
      </CollapsibleContent>
    </Collapsible>
    <p v-if="reminderFailed" role="alert" class="text-xs text-destructive">{{ t('calendarConnection.remindersFailed') }}</p>
    <p v-if="failed" role="alert" class="text-xs text-destructive">{{ t('calendarConnection.failed') }}</p>
    <p v-if="selectionStorageFailed" role="alert" class="text-xs text-destructive">{{ t('calendarConnection.selectionSaveFailed') }}</p>
    <div v-if="blocked" class="flex flex-wrap items-center justify-between gap-3">
      <p role="status" class="text-xs text-muted-foreground">{{ t('calendarConnection.denied') }}</p>
      <Button variant="outline" size="sm" @click="openSettings">{{ t('calendarConnection.openSettings') }}</Button>
    </div>
    <p v-if="permission === 'restricted'" role="status" class="text-xs text-muted-foreground">{{ t('calendarConnection.restricted') }}</p>
    <p v-if="permission === 'unsupported'" role="status" class="text-xs text-muted-foreground">{{ t('calendarConnection.unsupported') }}</p>
    <!-- Keep spacing inside the animated height, so the flex gap cannot snap on close. -->
    <Collapsible :id="`${id}-accounts`" :open="accountsOpen && !!snapshot" class="-mt-3">
      <CollapsibleContent class="calendar-disclosure">
        <div class="pt-3">
          <div v-if="snapshot" class="flex min-w-0 flex-col gap-4 rounded-md border p-3">
            <p role="status" class="text-xs font-medium" aria-live="polite">{{ t('calendarConnection.connected', { selected: selectedCount, accounts: accounts.length }) }}</p>
            <p v-if="!accounts.length" class="text-xs text-muted-foreground">{{ t('calendarConnection.noCalendars') }}</p>
            <ul v-else class="flex max-h-64 flex-col gap-3 overflow-y-auto">
              <li v-for="account in accounts" :key="account.id">
                <label class="flex cursor-pointer items-center gap-2 text-xs">
                  <CheckboxRoot
                    :class="checkboxClass" :model-value="selected.has(account.id)"
                    :aria-label="t('calendarConnection.useAccount', { account: account.email })"
                    :disabled="pending" @update:model-value="setAccount(account.id, $event === true)"
                  >
                    <CheckboxIndicator force-mount class="inline-flex items-center justify-center data-[state=unchecked]:invisible">
                      <Check aria-hidden="true" class="size-3" :stroke-width="3" />
                    </CheckboxIndicator>
                  </CheckboxRoot>
                  <span class="min-w-0 break-words">{{ account.email }}</span>
                </label>
              </li>
            </ul>
            <div class="flex items-center justify-between gap-3 border-t pt-3">
              <Button variant="ghost" size="sm" class="text-muted-foreground" :disabled="pending || reminderPending" @click="disconnectCalendars">
                {{ t('calendarConnection.disconnect') }}
              </Button>
              <Button variant="outline" size="sm" class="disabled:opacity-100" :disabled="pending" @click="connect">
                {{ t('calendarConnection.refresh') }}
              </Button>
            </div>
          </div>
        </div>
      </CollapsibleContent>
    </Collapsible>
  </section>
</template>

<style scoped>
.calendar-disclosure {
  overflow: hidden;
}

.calendar-disclosure[data-state="open"] {
  animation: calendar-disclosure-open 180ms ease-out;
}

.calendar-disclosure[data-state="closed"] {
  animation: calendar-disclosure-close 150ms ease-in;
}

@keyframes calendar-disclosure-open {
  from { height: 0; opacity: 0; }
  to { height: var(--reka-collapsible-content-height); opacity: 1; }
}

@keyframes calendar-disclosure-close {
  from { height: var(--reka-collapsible-content-height); opacity: 1; }
  to { height: 0; opacity: 0; }
}

@media (prefers-reduced-motion: reduce) {
  .calendar-disclosure[data-state] { animation: none; }
}
</style>
