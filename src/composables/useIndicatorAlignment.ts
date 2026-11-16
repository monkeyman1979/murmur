import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { onMounted, onUnmounted, ref } from "vue";

/** Keep the content-sized pill against its anchored window edge, rather than
 * centering it inside the transparent window's unused width. Read-only: these
 * windows never import the main window's settings/IPC synchronization. */
export function useIndicatorAlignment() {
  const alignment = ref("justify-center");
  let disposed = false;
  let unlisten: UnlistenFn | undefined;
  onMounted(() => {
    void listen<string>("murmur://indicator-position", ({ payload }) => {
      switch (payload) {
        case "top-left": case "bottom-left": alignment.value = "justify-start"; break;
        case "top-center": case "bottom-center": alignment.value = "justify-center"; break;
        case "top-right": case "bottom-right": alignment.value = "justify-end"; break;
      }
    }, { target: { kind: "WebviewWindow", label: getCurrentWindow().label } }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    }).catch((error) => {
      console.error("[murmur] indicator alignment listener failed", error);
    });
  });
  onUnmounted(() => {
    disposed = true;
    unlisten?.();
  });
  return alignment;
}
