<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { CalendarClock } from "lucide-vue-next";
import { useI18n } from "vue-i18n";
import { useTheme } from "@/composables/useTheme";
import { Button } from "@/components/ui/button";

interface ReminderView { token: string; title: string; start_ms: number; provider: string }
useTheme();
const { t, locale } = useI18n();
const reminder = ref<ReminderView | null>(null);
const pending = ref(false);
const failed = ref(false);
let disposed = false;
let version = 0;
let unlisten: UnlistenFn | null = null;

onMounted(async () => {
  const stop = await listen<ReminderView | null>("murmur://calendar-reminder", ({ payload }) => {
    version++;
    reminder.value = payload;
    failed.value = false;
  }).catch(() => { failed.value = true; return null; });
  if (disposed) { stop?.(); return; }
  unlisten = stop;
  const current = version;
  const initial = await invoke<ReminderView | null>("calendar_reminder_current").catch(() => null);
  if (!disposed && version === current) reminder.value = initial;
});
onUnmounted(() => { disposed = true; version++; unlisten?.(); });

async function respond(action: "start_recording" | "dismiss"): Promise<void> {
  if (!reminder.value || pending.value) return;
  const token = reminder.value.token;
  pending.value = true;
  failed.value = false;
  try {
    await invoke("calendar_reminder_action", { token, action });
    if (reminder.value?.token === token) reminder.value = null;
  } catch { failed.value = true; }
  finally { pending.value = false; }
}
function time(ms: number): string {
  return new Intl.DateTimeFormat(locale.value, { hour: "numeric", minute: "2-digit" }).format(ms);
}
</script>

<template>
  <div class="p-2">
    <section v-if="reminder" :key="reminder.token" role="region" aria-live="polite" :aria-label="t('meetingReminder.heading')" class="meeting-reminder-enter flex flex-col gap-3 rounded-xl border bg-background p-4 text-foreground shadow-lg">
      <div class="flex items-start gap-3">
        <CalendarClock class="mt-0.5 size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
        <div class="min-w-0">
          <p class="text-xs text-muted-foreground">{{ t('meetingReminder.heading') }} · {{ time(reminder.start_ms) }}</p>
          <h1 class="line-clamp-2 break-words text-sm font-semibold">{{ reminder.title || t('calendarConnection.untitledMeeting') }}</h1>
        </div>
      </div>
      <p v-if="failed" role="alert" class="text-xs text-destructive">{{ t('meetingReminder.failed') }}</p>
      <div class="flex justify-end gap-2">
        <Button variant="ghost" size="sm" :disabled="pending" @click="respond('dismiss')">{{ t('meetingReminder.dismiss') }}</Button>
        <Button size="sm" :disabled="pending" @click="respond('start_recording')">{{ t('meetingReminder.start') }}</Button>
      </div>
    </section>
  </div>
</template>

<style>
@keyframes meeting-reminder-enter {
  from { opacity: 0; transform: translateY(-10px); }
  to { opacity: 1; transform: translateY(0); }
}
.meeting-reminder-enter {
  animation: meeting-reminder-enter 220ms ease-out both;
}
@media (prefers-reduced-motion: reduce) {
  .meeting-reminder-enter { animation: none; }
}
html, body, #app {
  background: transparent !important;
  margin: 0 !important;
  padding: 0 !important;
  overflow: hidden !important;
}
</style>
