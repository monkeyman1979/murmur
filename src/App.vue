<script setup lang="ts">
import AppEntrance from "@/components/AppEntrance.vue";
import AppSidebar from "@/components/AppSidebar.vue";
import AppToaster from "@/components/AppToaster.vue";
import OnboardingFlow from "@/components/onboarding/OnboardingFlow.vue";
import { Button } from "@/components/ui/button";
import { IconTooltip } from "@/components/ui/tooltip";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { useActiveModels } from "@/composables/useActiveModels";
import { useCorrections } from "@/composables/useCorrections";
import { useHistoryRetention } from "@/composables/useHistoryRetention";
import { clearLegacyDictationLanguage } from "@/lib/legacyDictationLanguage";
import { shouldOpenMainWindow } from "@/lib/startupWindow";
import { appLanguage } from "@/i18n/appLanguage";
import { useHotkeyBinding } from "@/composables/useHotkeyBinding";
import { useMenuBarDevices } from "@/composables/useMenuBarDevices";
import { useMeetingAudioRetention } from "@/composables/useMeetingAudioRetention";
import { useMeetingTranscriptRetention } from "@/composables/useMeetingTranscriptRetention";
import {
  buildMeetingAiHandoffPayload,
  readMeetingAiHandoffPrompt,
} from "@/composables/useMeetingAiHandoff";
import { useMeetingCapture } from "@/composables/useMeetingCapture";
import { useOnboarding } from "@/composables/useOnboarding";
import { useMuteWhileDictating } from "@/composables/useMuteWhileDictating";
import { useSoundCues } from "@/composables/useSoundCues";
import { useTheme } from "@/composables/useTheme";
import { useVocabulary } from "@/composables/useVocabulary";
import {
  anchoredScrollTop,
  routeScrollTarget,
  shouldShowScrollToTop,
} from "@/lib/scroll";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ArrowUp, TriangleAlert } from "lucide-vue-next";
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useI18n } from "vue-i18n";

const { t } = useI18n();

// The native status item starts before localStorage is available to Rust.
// Mirror only the interface preference; native dictation and new meetings
// both keep their independent Auto-detect behavior.
watch(appLanguage, (language) => {
  void invoke("set_app_language", { language }).catch((error) => {
    console.error("[murmur] native app language sync failed", error);
  });
}, { immediate: true });

// Hydrate the saved app theme; onboarding scopes its own dark appearance.
const { effectiveMode } = useTheme();

// The native status item exists before the webview mounts. Keep device
// selections synchronized even when the user never visits Settings.
useMenuBarDevices();

// Same rationale for Vocabulary: the list needs to be pushed into
// Rust on app mount so the first hold-to-talk after launch gets the
// bias prompt, even if the user never opens Dictionary this session.
// The composable itself is idempotent — visiting Dictionary later reuses
// the same module-level ref.
useVocabulary();

// Corrections follow the same pattern — the ruleset must be pushed
// to the Rust state before the first hold, or the first hold skips
// user corrections even when they're persisted in localStorage.
useCorrections();

// History retention — the configured window is pushed to Rust on
// mount so the startup purge (which happened in lib.rs setup()
// before the frontend could speak) gets rerun with the user's
// actual preference. Without this, users who set "7 days" would
// see it honored only starting with the second relaunch.
useHistoryRetention();

// Same rationale as vocabulary — the binding must be mirrored to Rust
// before the first hotkey press so the listener thread isn't running on
// stale defaults. Visiting Settings → Hotkey later reuses the same
// module-level ref via the init() guard.
useHotkeyBinding();

// The active STT model id is pushed to Rust on boot so the hotkey pipeline
// knows which model to load before the first hold-to-talk press, even if the
// user never visits the Speech model card in Settings this session.
useActiveModels();

// Dictation and new meetings both use native Whisper auto-detect. Remove any
// old forced dictation hint so hidden legacy preferences cannot be misleading.
clearLegacyDictationLanguage();

// Sound cues — same boot-at-root rationale as the other settings
// composables: the Rust atomic needs to reflect the persisted state
// before the first hotkey press. Otherwise the first dictation after
// launch would always fire with no chime regardless of the user's
// saved preference.
useSoundCues();

// Mute-while-dictating — same pattern. Rust atomic needs the
// persisted flag value before the first hotkey press so
// `mute_if_enabled` honors the user's saved preference on dictation 1
// of the session.
useMuteWhileDictating();
useMeetingAudioRetention();
useMeetingTranscriptRetention();

// Onboarding completion flag — when false, the full-screen
// OnboardingFlow replaces the main app shell. Set to true only
// after the user clicks Open Murmur on the final onboarding step.
const { completed: onboardingCompleted } = useOnboarding();
const enteringFromOnboarding = ref(false);
const route = useRoute();
const router = useRouter();
const mainScrollElement = ref<HTMLElement | null>(null);
const showScrollToTop = ref(false);
const pageHeading = ref<HTMLHeadingElement | null>(null);

type CaptureStoreHealth =
  | { kind: "durable" }
  | { kind: "ephemeral"; reason: string }
  | { kind: "unavailable"; reason: string };
const captureStoreHealth = ref<CaptureStoreHealth | null>(null);
const storageWarning = computed(() => {
  if (captureStoreHealth.value?.kind === "ephemeral") {
    return t("navigation.storageEphemeral");
  }
  if (captureStoreHealth.value?.kind === "unavailable") {
    return t("navigation.storageUnavailable");
  }
  return null;
});

watch(onboardingCompleted, async (complete, wasComplete) => {
  if (!complete || wasComplete) return;
  enteringFromOnboarding.value = true;
  await nextTick();
  pageHeading.value?.focus();
});
let capturesListScrollTop = 0;
let capturesListAnchorId: string | null = null;
let capturesListAnchorOffset = 0;
let pendingRouteScrollTop: number | null = null;
let restoreTimer: number | undefined;
let restoreAttempts = 0;

function updateScrollToTopVisibility(): void {
  const element = mainScrollElement.value;
  showScrollToTop.value = element
    ? shouldShowScrollToTop(element)
    : false;
}

function captureRow(id: string): HTMLElement | undefined {
  const element = mainScrollElement.value;
  return element
    ? [...element.querySelectorAll<HTMLElement>("[data-capture-row]")]
      .find((row) => row.dataset.captureRow === id)
    : undefined;
}

function applyPendingRouteScroll(): void {
  const element = mainScrollElement.value;
  if (!element || pendingRouteScrollTop === null) return;

  window.clearTimeout(restoreTimer);
  if (route.name === "captures" && capturesListAnchorId) {
    const row = captureRow(capturesListAnchorId);
    if (row) {
      element.scrollTop = anchoredScrollTop({
        currentScrollTop: element.scrollTop,
        containerTop: element.getBoundingClientRect().top,
        rowTop: row.getBoundingClientRect().top,
        savedRowOffset: capturesListAnchorOffset,
      });
      capturesListAnchorId = null;
      pendingRouteScrollTop = null;
      restoreAttempts = 0;
      updateScrollToTopVisibility();
      return;
    }

    // The Meetings list refreshes asynchronously after the detail view
    // unmounts. Keep the saved numeric position as a fallback, then retry
    // until the selected row exists so it can return to the same viewport
    // position rather than being clamped to the top of an empty list.
    element.scrollTop = pendingRouteScrollTop;
    if (restoreAttempts < 40) {
      restoreAttempts += 1;
      restoreTimer = window.setTimeout(applyPendingRouteScroll, 50);
      return;
    }
    capturesListAnchorId = null;
  } else {
    element.scrollTop = pendingRouteScrollTop;
  }

  pendingRouteScrollTop = null;
  restoreAttempts = 0;
  updateScrollToTopVisibility();
}

async function restoreRouteScrollAfterInsertion(): Promise<void> {
  await nextTick();
  applyPendingRouteScroll();
}

function scrollMainToTop(): void {
  mainScrollElement.value?.scrollTo({
    top: 0,
    behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches
      ? "auto"
      : "smooth",
  });
}

// The webview scrolls this custom <main>, not window. Reset top-level route
// changes before the new page fades in, except for the list/detail round trip:
// opening a meeting starts its transcript at the top, while returning to the
// Meetings list restores the exact position the user left.
const removeScrollGuard = router.beforeEach((to, from) => {
  window.clearTimeout(restoreTimer);
  restoreAttempts = 0;
  const element = mainScrollElement.value;
  if (from.name === "captures" && to.name === "capture-detail" && element) {
    capturesListScrollTop = element.scrollTop;
    capturesListAnchorId = String(to.params.captureId);
    const row = captureRow(capturesListAnchorId);
    capturesListAnchorOffset = row
      ? row.getBoundingClientRect().top - element.getBoundingClientRect().top
      : 0;
  } else if (!(from.name === "capture-detail" && to.name === "captures")) {
    capturesListAnchorId = null;
  }

  pendingRouteScrollTop = routeScrollTarget({
    fromName: from.name,
    toName: to.name,
    fromPage: from.matched[0]?.path ?? from.path,
    toPage: to.matched[0]?.path ?? to.path,
    capturesListScrollTop,
  });
});

const {
  action,
  activeCapture,
  startFlowRequested,
  consumeStartFlowRequest,
  pause,
  requestStartDialog,
  resolveStartFlow,
  resume,
  stop,
} = useMeetingCapture();

async function handleTrayCopy(kind: "transcript" | "for_ai"): Promise<void> {
  try {
    const context = await invoke<{ title: string; transcript: string } | null>(
      "tray_read_latest_ai_handoff_context",
    );
    if (!context) return;
    const text = kind === "for_ai"
      ? buildMeetingAiHandoffPayload(
          readMeetingAiHandoffPrompt(),
          context.transcript,
          context.title,
        )
      : context.transcript;
    await invoke("tray_copy_text", { text });
  } catch (error) {
    // Never include transcript or prompt content in diagnostics.
    console.error("[murmur] tray transcript copy failed", error);
  }
}

async function handleTrayPauseResume(): Promise<void> {
  if (action.value !== null) return;
  if (activeCapture.value?.status === "paused") await resume();
  else await pause();
}

async function handleTrayStop(): Promise<void> {
  if (action.value !== null) return;
  await stop();
}

const trayUnlisteners: UnlistenFn[] = [];
onMounted(() => {
  // Keep the webview hydrated on quiet login startup, but show it for an
  // explicit Finder/Spotlight cold launch. Rust caches the initial launch
  // source before IPC runs; merely enabling login startup must not hide a
  // manual launch. If source inspection fails, don't trap the user in the tray.
  void invoke<boolean>("startup_launch_at_login")
    .catch((error) => {
      console.error("[murmur] startup launch source could not be checked", error);
      return false;
    })
    .then((launchedAtLogin) => {
      if (shouldOpenMainWindow(onboardingCompleted.value, launchedAtLogin)) {
        return invoke("show_main_window");
      }
    })
    .catch((error) => {
      console.error("[murmur] startup window could not be shown", error);
    });
  void invoke<CaptureStoreHealth>("capture_store_health")
    .then((health) => { captureStoreHealth.value = health; })
    .catch(() => {
      captureStoreHealth.value = {
        kind: "unavailable",
        reason: "storage_health_unavailable",
      };
    });
  void listen<"transcript" | "for_ai">(
    "murmur://tray-copy-requested",
    (event) => void handleTrayCopy(event.payload),
  )
    .then((unlisten) => trayUnlisteners.push(unlisten))
    .catch((error) => console.error("[murmur] tray copy listener failed", error));
  void listen("murmur://tray-open-settings", () => {
    void router.push("/settings/general");
  })
    .then((unlisten) => trayUnlisteners.push(unlisten))
    .catch((error) => console.error("[murmur] tray settings listener failed", error));
  void listen("murmur://tray-meeting-pause-resume-requested", () => {
    void handleTrayPauseResume();
  })
    .then((unlisten) => trayUnlisteners.push(unlisten))
    .catch((error) => console.error("[murmur] tray meeting pause/resume listener failed", error));
  void listen("murmur://tray-meeting-stop-requested", () => {
    void handleTrayStop();
  })
    .then((unlisten) => trayUnlisteners.push(unlisten))
    .catch((error) => console.error("[murmur] tray meeting stop listener failed", error));
});
onUnmounted(() => {
  removeScrollGuard();
  window.clearTimeout(restoreTimer);
  for (const unlisten of trayUnlisteners) unlisten();
});

// Keep the native menu bar synchronized even when Murmur's main window is
// hidden. TrayState retains this value while the status item stays installed,
// so reopening the menu never briefly offers "Record Meeting" during an
// already-active capture.
watch(
  [() => activeCapture.value?.status ?? null, action],
  ([status, pendingAction]) => {
    let trayStatus = status;
    if (status && pendingAction === "pause") trayStatus = "pausing";
    else if (status && pendingAction === "resume") trayStatus = "resuming";
    else if (status && (pendingAction === "stop" || pendingAction === "cancel")) trayStatus = "stopping";

    void invoke("set_tray_meeting_state", { status: trayStatus }).catch((error) => {
      console.error("[murmur] tray meeting state sync failed", error);
    });
  },
  { immediate: true },
);

// Phase B: the single place any start request is resolved. The tray emits a
// request rather than starting capture itself, and the `/captures` button
// raises the same request, so both get identical treatment:
//
//   - everything ready  → start immediately; the floating controller is the UI
//                         and the main window is left exactly as it was
//   - anything missing  → surface the main window and hand off to the dialog
//
// This lives at the app root rather than in `Captures.vue` because the tray
// item works from any route, and because the decision must happen *before*
// anything opens the dialog. `Captures.vue` therefore watches the separate
// `startDialogRequested` flag, which only this resolver sets.
let resolvingStartFlow = false;

async function resolveStartRequest(): Promise<void> {
  if (resolvingStartFlow) return;
  // Mid-onboarding the app shell doesn't render, so `Captures.vue` isn't there
  // to receive a dialog request — and a fast-path start would be a recording
  // the user never consented to. Surface the window and let them finish setup.
  if (!onboardingCompleted.value) {
    await invoke("show_main_window").catch(() => {});
    return;
  }
  resolvingStartFlow = true;
  try {
    const outcome = await resolveStartFlow();
    if (outcome === "started") return;
    // Only now is the main window worth interrupting the user with.
    await invoke("show_main_window").catch((err) => {
      console.error("[murmur] show_main_window failed", err);
    });
    await router.push("/captures");
    requestStartDialog();
  } finally {
    resolvingStartFlow = false;
  }
}

watch(startFlowRequested, (requested) => {
  if (!requested) return;
  consumeStartFlowRequest();
  void resolveStartRequest();
}, { immediate: true });

// Friendly title for the current page. Falls back to "Murmur" if a route
// hasn't declared one.
const pageTitle = computed<string>(() => {
  const meta = route.meta?.title;
  const key = {
    Home: "navigation.home",
    Dictations: "navigation.dictations",
    Meetings: "navigation.meetings",
    Meeting: "navigation.meeting",
    Dictionary: "navigation.dictionary",
    Settings: "navigation.settings",
  }[typeof meta === "string" ? meta : ""];
  return key ? t(key) : t("common.appName");
});
</script>

<template>
  <!-- The main shell stays unmounted during the non-dismissible onboarding dialog.
       Keep the native draggable strip above its full-window composition. -->
  <template v-if="!onboardingCompleted">
    <div
      class="fixed inset-x-0 top-0 z-50 h-8"
      data-tauri-drag-region
      aria-hidden="true"
    />
    <OnboardingFlow />
  </template>
  <AppEntrance v-else :from-onboarding="enteringFromOnboarding">
  <SidebarProvider class="h-svh min-h-0 overflow-hidden bg-transparent! pt-8">
    <div
      class="fixed inset-x-0 top-0 z-50 h-8"
      data-tauri-drag-region
    >
      <SidebarTrigger class="absolute left-[74px] top-0.5" />
    </div>

    <AppSidebar collapsible="icon" class="top-8! h-[calc(100svh-2rem)]!" />
    <SidebarInset>
      <div
        v-if="storageWarning"
        class="mx-4 mt-3 flex shrink-0 items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-foreground"
        role="alert"
        data-testid="storage-warning"
      >
        <TriangleAlert class="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden="true" />
        <span>{{ storageWarning }}</span>
      </div>
      <main
        ref="mainScrollElement"
        class="min-h-0 flex-1 overflow-y-auto overscroll-contain"
        @scroll.passive="updateScrollToTopVisibility"
      >
        <header
          class="page-header-fade sticky top-0 z-20 flex h-14 shrink-0 items-center gap-2 px-4 transition-[height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-12"
        >
          <div class="flex min-w-0 flex-1 items-center gap-3">
            <h1
              ref="pageHeading"
              class="truncate text-sm font-medium"
              :data-captures-heading="route.name === 'captures' ? '' : undefined"
              tabindex="-1"
            >
              {{ pageTitle }}
            </h1>
          </div>
          <div
            id="page-header-actions"
            class="flex min-w-0 flex-1 justify-end empty:hidden"
          />
        </header>

        <RouterView v-slot="{ Component, route: viewRoute }">
          <Transition name="page-fade" mode="out-in">
            <!-- Route pages can have Teleport/fragment roots; animate the wrapper. -->
            <div
              :key="viewRoute.matched[0]?.path ?? viewRoute.path"
              class="min-w-0"
              @vue:mounted="restoreRouteScrollAfterInsertion"
              @vue:updated="restoreRouteScrollAfterInsertion"
            >
              <component :is="Component" />
            </div>
          </Transition>
        </RouterView>
      </main>

      <Transition name="scroll-top-fade">
        <IconTooltip v-if="showScrollToTop" :label="t('navigation.scrollToTop')">
          <Button
            type="button"
            variant="ghost"
            size="icon"
            class="absolute bottom-4 right-4 z-30 size-9 cursor-pointer rounded-full bg-menu text-menu-foreground shadow-lg hover:bg-menu-hover"
            :aria-label="t('navigation.scrollToTop')"
            @click="scrollMainToTop"
          >
            <ArrowUp class="size-4" aria-hidden="true" />
          </Button>
        </IconTooltip>
      </Transition>
    </SidebarInset>
  </SidebarProvider>
  </AppEntrance>
  <AppToaster :theme="effectiveMode" />
</template>
