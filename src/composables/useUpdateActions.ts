import { computed } from "vue";
import { useCaptures } from "@/composables/useCaptures";
import { useMurmurEvents } from "@/composables/useMurmurEvents";
import { useUpdater } from "@/composables/useUpdater";
import { i18n } from "@/i18n";

/**
 * Labels and actions shared by the sidebar update card and Settings → About,
 * so both surfaces gate restart on the same in-progress work.
 */
export function useUpdateActions() {
  const t = i18n.global.t;
  const updater = useUpdater();
  const { isRecording, isProcessingDictation } = useMurmurEvents();
  const { inProgress, hasActiveMutation } = useCaptures();

  const restartBlocked = computed(() =>
    isRecording.value || isProcessingDictation.value || inProgress.value.length > 0 || hasActiveMutation.value,
  );

  // Sidebar card: shown only once an update is found.
  const statusLabel = computed(() => {
    if (updater.phase.value === "available") return t('updatesCard.availableVersion', { version: updater.version.value ?? '' });
    if (updater.phase.value === "downloading") return t('updatesCard.downloading');
    if (updater.phase.value === "ready") {
      if (restartBlocked.value) return t('updatesCard.readyBlocked');
      return updater.errorMessage.value ?? t('updatesCard.ready');
    }
    if (updater.phase.value === "installing") return t('updatesCard.installing');
    return updater.errorMessage.value ?? t('updatesCard.unavailable');
  });
  const actionLabel = computed(() => {
    if (updater.phase.value === "available") return t('updatesCard.update');
    if (updater.phase.value === "ready") return t(restartBlocked.value ? 'updatesCard.finishFirst' : 'updatesCard.restartUpdate');
    if (updater.phase.value === "error" && updater.errorSource.value === "restart") return t('updatesCard.restart');
    if (updater.phase.value === "error") return t('updatesCard.retry');
    return null;
  });
  function runAction(): void {
    if (updater.phase.value === "available") void updater.downloadUpdate();
    else if (updater.phase.value === "ready") void updater.installAndRestart(restartBlocked.value);
    else if (updater.phase.value === "error" && updater.errorSource.value === "restart") void updater.retryRelaunch();
    else if (updater.phase.value === "error") void updater.checkForUpdates({ manual: true });
  }

  // Settings → About: the whole flow, from checking through restart.
  const settingsStatus = computed(() => {
    const phase = updater.phase.value;
    if (phase === "checking") return t('generalSettings.checking');
    if (phase === "up-to-date") return t('generalSettings.upToDate');
    if (phase === "available" || phase === "dismissed") {
      return updater.version.value ? t('generalSettings.versionAvailable', { version: updater.version.value }) : t('generalSettings.updateAvailable');
    }
    if (phase === "downloading" || phase === "ready" || phase === "installing") return statusLabel.value;
    if (phase === "error") return updater.errorMessage.value ?? t('generalSettings.updateFailed');
    return t('generalSettings.updateHelp');
  });
  const settingsActionLabel = computed(() => {
    const phase = updater.phase.value;
    if (phase === "downloading" || phase === "installing") return null;
    if (phase === "dismissed") return t('updatesCard.update');
    return actionLabel.value ?? t('generalSettings.checkUpdates');
  });
  const settingsActionDisabled = computed(() =>
    updater.phase.value === "checking" || (updater.phase.value === "ready" && restartBlocked.value),
  );
  function runSettingsAction(): void {
    if (updater.phase.value === "dismissed") {
      updater.resumeDismissedUpdate();
      void updater.downloadUpdate();
    } else if (actionLabel.value) {
      runAction();
    } else {
      void updater.checkForUpdates({ manual: true });
    }
  }

  return {
    updater,
    restartBlocked,
    statusLabel,
    actionLabel,
    runAction,
    settingsStatus,
    settingsActionLabel,
    settingsActionDisabled,
    runSettingsAction,
  };
}
