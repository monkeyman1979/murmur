import { onMounted, onUnmounted, ref, type Ref } from "vue";

const hoveredTranscriptId = ref<number | null>(null);
let users = 0;

function trackHoveredTranscript(event: PointerEvent): void {
  const element = event.target instanceof Element
    ? event.target.closest<HTMLElement>("[data-transcript-row-id]")
    : null;
  const id = element?.dataset.transcriptRowId;
  hoveredTranscriptId.value = id === undefined ? null : Number(id);
}

function clearHoveredTranscript(): void {
  hoveredTranscriptId.value = null;
}

function install(): void {
  if (users++ > 0) return;
  document.addEventListener("pointermove", trackHoveredTranscript, {
    capture: true,
    passive: true,
  });
  document.documentElement.addEventListener("pointerleave", clearHoveredTranscript);
  window.addEventListener("blur", clearHoveredTranscript);
}

function uninstall(): void {
  users = Math.max(0, users - 1);
  if (users > 0) return;
  document.removeEventListener("pointermove", trackHoveredTranscript, { capture: true });
  document.documentElement.removeEventListener("pointerleave", clearHoveredTranscript);
  window.removeEventListener("blur", clearHoveredTranscript);
  clearHoveredTranscript();
}

/**
 * Shared pointer tracker for transcript rows. WebKit can retain stale
 * per-element hover state in the app's composited scrolling shell, so the
 * current event target is the authoritative hovered row.
 */
export function useTranscriptRowHover(): Ref<number | null> {
  onMounted(install);
  onUnmounted(uninstall);
  return hoveredTranscriptId;
}
