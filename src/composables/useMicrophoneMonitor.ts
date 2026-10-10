import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useInputDevices } from "@/composables/useInputDevices";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { i18n } from "@/i18n";

// Shared serialization also covers a fast unmount/remount of the step.
let monitorQueue: Promise<void> = Promise.resolve();
export function useMicrophoneMonitor() {
  const input = useInputDevices();
  const { permissions } = useMurmurEvents();
  const level = ref(0);
  const monitoring = ref(false);
  const monitorError = ref<string | null>(null);
  const permissionGranted = computed(() => permissions.value?.microphone === "granted");
  const available = computed(() => permissionGranted.value && input.state.value === "ready" && input.devices.value.length > 0 && monitoring.value && !monitorError.value);
  let mounted = false;
  let suspended = false;
  let generation = 0;
  let unlisten: UnlistenFn | null = null;

  function restart(): Promise<void> {
    suspended = false;
    const request = ++generation;
    monitoring.value = false;
    level.value = 0;
    monitorError.value = null;
    const action = async () => {
      try {
        await invoke("stop_mic_monitor");
        if (request !== generation || !mounted || suspended || !permissionGranted.value || input.state.value !== "ready") return;
        if (!unlisten) {
          const stop = await listen<number>("murmur://mic-monitor-level", (event) => {
            if (mounted && monitoring.value) level.value = Math.max(0, Math.min(1, event.payload));
          });
          if (!mounted || request !== generation) { stop(); return; }
          unlisten = stop;
        }
        await input.syncSelection();
        if (request !== generation || !mounted || suspended) return;
        await invoke("start_mic_monitor");
        if (request !== generation || !mounted || suspended) {
          await invoke("stop_mic_monitor");
          return;
        }
        monitoring.value = true;
      } catch {
        if (request === generation && mounted) {
          monitorError.value = i18n.global.t('runtimeErrors.microphonePreviewStart');
        }
      }
    };
    monitorQueue = monitorQueue.then(action, action);
    return monitorQueue;
  }

  async function stopForExit(): Promise<boolean> {
    suspended = true;
    ++generation;
    monitoring.value = false;
    level.value = 0;
    const stop = async () => { await invoke("stop_mic_monitor"); };
    const result = monitorQueue.then(stop, stop);
    monitorQueue = result.catch(() => {});
    try { await result; return true; }
    catch { if (mounted) monitorError.value = i18n.global.t('runtimeErrors.microphonePreviewStop'); return false; }
  }
  onMounted(() => { mounted = true; void restart(); });
  watch([input.selected, input.devices, input.state, permissionGranted], () => {
    if (mounted && !suspended) void restart();
  }, { flush: "sync" });
  onUnmounted(() => {
    mounted = false;
    unlisten?.();
    unlisten = null;
    void stopForExit();
  });
  return { level, monitoring, monitorError, available, restart, stopForExit };
}
