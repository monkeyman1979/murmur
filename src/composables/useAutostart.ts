import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { computed, ref } from "vue";
import { i18n } from "@/i18n";

// One queue across all consumers: a settings toggle must not race onboarding.
const enabled = ref<boolean | null>(null);
const loading = ref(false);
const writes = ref(0);
const pending = computed(() => writes.value > 0);
const error = ref<string | null>(null);
let queue: Promise<void> = Promise.resolve();
let initialized = false;

function serialize(work: () => Promise<void>): Promise<void> {
  queue = queue.then(work, work);
  return queue;
}

async function readActual(): Promise<void> {
  loading.value = true;
  try {
    enabled.value = await isEnabled();
  } catch {
    enabled.value = null;
    error.value = i18n.global.t('runtimeErrors.autostartStatus');
  } finally {
    loading.value = false;
  }
}

function refresh(): Promise<void> {
  return serialize(async () => {
    error.value = null;
    await readActual();
  });
}

function setEnabled(value: boolean): Promise<void> {
  writes.value++;
  return serialize(async () => {
    error.value = null;
    try {
      await (value ? enable() : disable());
    } catch {
      error.value = i18n.global.t('runtimeErrors.autostartUpdate');
    }
    // Never claim success optimistically, including a partial plugin failure.
    await readActual();
    writes.value--;
  });
}

export function useAutostart() {
  if (!initialized) {
    initialized = true;
    void refresh();
  }
  return { enabled, loading, pending, error, setEnabled, refresh };
}
