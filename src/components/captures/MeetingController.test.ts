import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const cancelMock = vi.hoisted(() => vi.fn());
const surfaceMocks = vi.hoisted(() => ({
  notch: false, paused: false, revision: 0, pause: vi.fn(), resume: vi.fn(), stop: vi.fn(),
  callbacks: new Map<string, (event: { payload: unknown }) => void>(),
}));
const recordingIssueMock = vi.hoisted(() => ({
  value: null as null | { severity: "warning" | "error"; label: string; detail: string },
}));

vi.mock("@/composables/useMeetingCapture", async () => {
  const { ref } = await import("vue");
  return {
    useMeetingCapture: () => ({
      activeCapture: ref({ id: 42, status: surfaceMocks.paused ? "paused" : "recording" }),
      // Under 10 seconds, native cancel(false) would discard immediately.
      activeDurationMs: ref(2_000),
      action: ref(null),
      controls: ref([surfaceMocks.paused ? "resume" : "pause", "stop", "cancel"]),
      error: ref(null),
      recordingIssue: recordingIssueMock,
      hydrate: vi.fn(),
      pause: surfaceMocks.pause,
      resume: surfaceMocks.resume,
      stop: surfaceMocks.stop,
      cancel: cancelMock,
    }),
  };
});

vi.mock("@/composables/useTheme", () => ({ useTheme: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async (command, args) => {
  const presentation = { style: surfaceMocks.notch ? "notch" : "floating", expanded: false, compact_width: 329, notch_width: 185, notch_height: 32, expanded_width: 400, expanded_height: 176, revision: surfaceMocks.revision, fresh: true };
  if (command === "get_indicator_presentation") return presentation;
  if (command === "set_indicator_expanded") surfaceMocks.callbacks.get("murmur://indicator-presentation")?.({ payload: { ...presentation, expanded: args.expanded, revision: ++surfaceMocks.revision, fresh: false } });
  return undefined;
}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ label: "meeting-controller" }) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (name, callback) => {
  surfaceMocks.callbacks.set(name, callback);
  return vi.fn();
}) }));

import MeetingController from "@/components/captures/MeetingController.vue";
import { IconTooltip } from "@/components/ui/tooltip";
import { AudioLines } from "lucide-vue-next";

function finishSurfaceTransition(element: Element) {
  const ended = new Event("transitionend");
  Object.defineProperty(ended, "propertyName", { value: "width" });
  element.dispatchEvent(ended);
}

function mountController(locale = "en") {
  return mount(MeetingController, {
    global: { plugins: [createI18n({
      legacy: false,
      locale,
      fallbackLocale: "en",
      messages: { en, es, fr, ru },
    })] },
  });
}

describe("MeetingController discard confirmation", () => {
  beforeEach(() => {
    window.history.replaceState({}, "", "/");
    cancelMock.mockReset();
    surfaceMocks.notch = false;
    surfaceMocks.revision = 0;
    surfaceMocks.paused = false;
    surfaceMocks.resume.mockReset();
    surfaceMocks.pause.mockReset();
    surfaceMocks.stop.mockReset();
    surfaceMocks.callbacks.clear();
    recordingIssueMock.value = null;
    cancelMock.mockResolvedValue({
      status: "confirmation_required",
      capture: { id: 42, status: "recording" },
    });
  });

  it("keeps existing pause, stop and confirmed discard actions inside the expanded notch", async () => {
    surfaceMocks.notch = true;
    const controller = mountController();
    await flushPromises();
    expect(controller.find('[aria-label="Pause meeting recording"]').exists()).toBe(false);
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(controller.find(".meeting-notch-controls").exists()).toBe(true);
    expect(controller.find(".meeting-controller-pill").exists()).toBe(false);
    expect(controller.findAll("time")).toHaveLength(1);
    expect(controller.get('[aria-label="Pause meeting recording"]').text()).toBe("Pause");
    expect(controller.get('[aria-label="Stop meeting recording and transcribe"]').text()).toBe("Finish");
    await controller.get('[aria-label="Pause meeting recording"]').trigger("click");
    expect(surfaceMocks.pause).toHaveBeenCalledOnce();
    surfaceMocks.stop.mockResolvedValue(null);
    await controller.get('[aria-label="Stop meeting recording and transcribe"]').trigger("click");
    // Finish first folds the notch into the camera housing, then stops.
    const panel = controller.get(".notch-surface");
    expect(surfaceMocks.stop).not.toHaveBeenCalled();
    expect(panel.classes()).toContain("notch-surface--exiting");
    expect(panel.attributes("style")).toContain("--surface-width: 185px");
    expect(panel.attributes("style")).toContain("--surface-height: 32px");
    finishSurfaceTransition(panel.element);
    await flushPromises();
    expect(surfaceMocks.stop).toHaveBeenCalledOnce();
    // stop() did not end the capture, so the panel comes back.
    expect(panel.classes()).not.toContain("notch-surface--exiting");
    expect(controller.get(".notch-surface__handle").attributes("aria-expanded")).toBe("true");
    await controller.get('[aria-label="Discard meeting recording"]').trigger("click");
    expect(cancelMock).not.toHaveBeenCalled();
    await controller.get('[aria-label="Confirm discard meeting recording"]').trigger("click");
    expect(cancelMock).not.toHaveBeenCalled();
    expect(panel.classes()).toContain("notch-surface--exiting");
    finishSurfaceTransition(panel.element);
    await flushPromises();
    expect(cancelMock).toHaveBeenCalledWith(true);
    controller.unmount();
  });

  it("stays folded into the camera once a discard actually ends the capture", async () => {
    surfaceMocks.notch = true;
    cancelMock.mockResolvedValue({ status: "cancelled", capture_id: 42 });
    const controller = mountController();
    await flushPromises();
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    await controller.get('[aria-label="Discard meeting recording"]').trigger("click");
    await controller.get('[aria-label="Confirm discard meeting recording"]').trigger("click");
    const panel = controller.get(".notch-surface");
    finishSurfaceTransition(panel.element);
    await flushPromises();
    expect(cancelMock).toHaveBeenCalledOnce();
    expect(panel.classes()).toContain("notch-surface--exiting");
    controller.unmount();
  });

  it("keeps one identical timer and waveform through expansion and collapse", async () => {
    surfaceMocks.notch = true;
    const controller = mountController();
    await flushPromises();
    const timer = controller.get(".meeting-notch-timer").element;
    expect(controller.findAll("time")).toHaveLength(1);
    expect(controller.findAllComponents(AudioLines)).toHaveLength(1);
    expect(controller.findAll(".bg-red-500")).toHaveLength(1);
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(controller.get(".meeting-notch-timer").element).toBe(timer);
    expect(controller.get(".meeting-notch-timer").classes()).toContain("text-[11px]");
    expect(controller.find(".meeting-notch-controls time").exists()).toBe(false);
    expect(controller.findAllComponents(AudioLines)).toHaveLength(1);
    await controller.get('.meeting-notch-controls [aria-label="Collapse recording controls"]').trigger("click");
    await flushPromises();
    expect(controller.get(".notch-surface__handle").attributes("aria-expanded")).toBe("false");
    expect(controller.get(".meeting-notch-timer").element).toBe(timer);
    expect(controller.findAll("time")).toHaveLength(1);
    expect(controller.findAllComponents(AudioLines)).toHaveLength(1);
    controller.unmount();
  });

  it("places a subtle discard text action above Pause/Finish and still confirms", async () => {
    surfaceMocks.notch = true;
    const controller = mountController();
    await flushPromises();
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    const discard = controller.get('[aria-label="Discard meeting recording"]');
    expect(discard.text()).toBe("Discard recording");
    expect(discard.find("svg").exists()).toBe(false);
    expect(discard.classes()).toContain("hover:underline");
    const actions = controller.findAll(".meeting-notch-controls button");
    const discardIndex = actions.findIndex(button => button.attributes("aria-label") === "Discard meeting recording");
    const pauseIndex = actions.findIndex(button => button.attributes("aria-label") === "Pause meeting recording");
    const finishIndex = actions.findIndex(button => button.attributes("aria-label") === "Stop meeting recording and transcribe");
    expect(discardIndex).toBeLessThan(pauseIndex);
    expect(discardIndex).toBeLessThan(finishIndex);
    await discard.trigger("click");
    expect(cancelMock).not.toHaveBeenCalled();
    expect(controller.find('[aria-label="Confirm discard meeting recording"]').exists()).toBe(true);
    controller.unmount();
  });

  it("resets an unconfirmed discard after outside dismissal without changing recording", async () => {
    surfaceMocks.notch = true;
    const controller = mountController();
    await flushPromises();
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    await controller.get('[aria-label="Discard meeting recording"]').trigger("click");
    expect(controller.find('[aria-label="Confirm discard meeting recording"]').exists()).toBe(true);
    surfaceMocks.callbacks.get("murmur://indicator-presentation")?.({ payload: { style: "notch", expanded: false, compact_width: 329, notch_width: 185, notch_height: 32 } });
    await flushPromises();
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(controller.find('[aria-label="Confirm discard meeting recording"]').exists()).toBe(false);
    expect(cancelMock).not.toHaveBeenCalled();
    expect(surfaceMocks.pause).not.toHaveBeenCalled();
    expect(surfaceMocks.stop).not.toHaveBeenCalled();
    controller.unmount();
  });

  it("shows a clear paused state and a labeled Resume action", async () => {
    surfaceMocks.notch = true;
    surfaceMocks.paused = true;
    const controller = mountController();
    await flushPromises();
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(controller.get(".meeting-notch-controls").text()).toContain("Paused");
    const resume = controller.get('[aria-label="Resume meeting recording"]');
    expect(resume.text()).toBe("Resume");
    await resume.trigger("click");
    expect(surfaceMocks.resume).toHaveBeenCalledOnce();
    controller.unmount();
  });

  it("opens and controls the meeting from the laptop notch, even with a legacy mirror URL", async () => {
    // With Notch selected, the laptop notch is a full controller when the
    // meeting runs on a notchless external display (meeting-notch window).
    surfaceMocks.notch = true;
    window.history.replaceState({}, "", "/?window=meeting-controller&mirror=1");
    const controller = mountController();
    await flushPromises();
    expect(controller.find(".notch-surface--passive").exists()).toBe(false);
    expect(controller.text()).toContain("00:02");
    await controller.get(".notch-surface__handle").trigger("click");
    await flushPromises();
    expect(controller.find(".meeting-notch-controls").exists()).toBe(true);
    await controller.get('[aria-label="Pause meeting recording"]').trigger("click");
    expect(surfaceMocks.pause).toHaveBeenCalledOnce();
    controller.unmount();
  });

  it("keeps recording issues visible when the notch is collapsed", async () => {
    surfaceMocks.notch = true;
    recordingIssueMock.value = { severity: "error", label: "Status unavailable", detail: "Could not confirm recording." };
    const controller = mountController();
    await flushPromises();
    expect(controller.get('[role="alert"]').text()).toBe("Status unavailable");
    expect(controller.find(".meeting-controller-pill").exists()).toBe(false);
    controller.unmount();
  });

  it("shows an explicit issue instead of an ambiguous icon and keeps safe exit controls", () => {
    recordingIssueMock.value = {
      severity: "error",
      label: "Status unavailable",
      detail: "Murmur could not confirm the recording status.",
    };
    const controller = mountController();

    expect(controller.get('[role="alert"]').text()).toBe("Status unavailable");
    expect(controller.get('[role="alert"]').attributes("title")).toBeUndefined();
    expect(controller.findAllComponents(IconTooltip).map((tooltip) => tooltip.props("label"))).toContain(
      "Murmur could not confirm the recording status.",
    );
    expect(controller.find("time").exists()).toBe(false);
    expect(controller.find('[aria-label="Pause meeting recording"]').exists()).toBe(false);
    const stop = controller.get('[aria-label="Stop meeting recording and transcribe"]');
    expect(stop.text()).toBe("Stop");
    expect(controller.find('[aria-label="Discard meeting recording"]').exists()).toBe(true);
  });

  it.each([
    ["es", "Descartar", "Descartar grabación de la reunión", "Detener grabación de la reunión y transcribir"],
    ["fr", "Supprimer", "Supprimer l’enregistrement de la réunion", "Arrêter l’enregistrement de la réunion et transcrire"],
    ["ru", "Удалить", "Удалить запись встречи", "Остановить запись встречи и расшифровать"],
  ])("renders %s visible and accessible controller copy", async (locale, label, accessibleName, stopName) => {
    const controller = mountController(locale);
    expect(controller.find(`[aria-label="${stopName}"]`).exists()).toBe(true);
    expect(controller.get("time").attributes("aria-label")).toContain("02");
    await controller.get(`[aria-label="${accessibleName}"]`).trigger("click");
    expect(controller.find(`[aria-label="${accessibleName}"]`).exists()).toBe(false);
    expect(controller.get(".meeting-controller-pill").text()).toContain(label);
    expect(controller.find("time").exists()).toBe(false);
  });

  it("separates discard from Stop and confirms with explicit Keep / Discard actions", async () => {
    const controller = mountController();
    expect(controller.get(".meeting-controller-pill").classes()).toContain("px-1.5");
    const buttons = controller.findAll("button");
    expect(buttons[0].attributes("aria-label")).toBe("Discard meeting recording");
    expect(buttons[buttons.length - 1].attributes("aria-label")).toBe("Stop meeting recording and transcribe");
    expect(controller.findAllComponents(IconTooltip).map((tooltip) => tooltip.props("label"))).toContain(
      "Discard recording (confirmation required)",
    );

    await controller.get('[aria-label="Discard meeting recording"]').trigger("click");
    await flushPromises();

    expect(cancelMock).not.toHaveBeenCalled();
    expect(controller.text()).toContain("Discard meeting recording?");
    expect(controller.get(".meeting-controller-pill").classes()).toContain("px-3");
    expect(controller.find("time").exists()).toBe(false);
    expect(controller.find(".bg-red-500").exists()).toBe(false);
    expect(controller.find('[aria-label="Discard meeting recording"]').exists()).toBe(false);
    expect(controller.get('[aria-label="Keep meeting recording"]').text()).toBe("Keep");
    expect(controller.get('[aria-label="Confirm discard meeting recording"]').text()).toBe("Discard");

    await controller.get('[aria-label="Keep meeting recording"]').trigger("click");
    expect(controller.text()).not.toContain("Discard meeting recording?");
    expect(controller.get(".meeting-controller-pill").classes()).toContain("px-1.5");
    expect(controller.find("time").exists()).toBe(true);
    expect(controller.find(".bg-red-500").exists()).toBe(true);
    expect(cancelMock).not.toHaveBeenCalled();

    await controller.get('[aria-label="Discard meeting recording"]').trigger("click");
    await flushPromises();
    await controller.get('[aria-label="Confirm discard meeting recording"]').trigger("click");
    await flushPromises();

    expect(cancelMock).toHaveBeenCalledTimes(1);
    expect(cancelMock).toHaveBeenCalledWith(true);
  });
});
