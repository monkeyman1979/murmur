import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createI18n } from "vue-i18n";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import fr from "@/locales/fr.json";
import ru from "@/locales/ru.json";

const cancelMock = vi.hoisted(() => vi.fn());
const recordingIssueMock = vi.hoisted(() => ({
  value: null as null | { severity: "warning" | "error"; label: string; detail: string },
}));

vi.mock("@/composables/useMeetingCapture", async () => {
  const { ref } = await import("vue");
  return {
    useMeetingCapture: () => ({
      activeCapture: ref({ id: 42, status: "recording" }),
      // Under 10 seconds, native cancel(false) would discard immediately.
      activeDurationMs: ref(2_000),
      action: ref(null),
      controls: ref(["pause", "stop", "cancel"]),
      error: ref(null),
      recordingIssue: recordingIssueMock,
      hydrate: vi.fn(),
      pause: vi.fn(),
      resume: vi.fn(),
      stop: vi.fn(),
      cancel: cancelMock,
    }),
  };
});

vi.mock("@/composables/useTheme", () => ({ useTheme: vi.fn() }));

import MeetingController from "@/components/captures/MeetingController.vue";
import { IconTooltip } from "@/components/ui/tooltip";

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
    cancelMock.mockReset();
    recordingIssueMock.value = null;
    cancelMock.mockResolvedValue({
      status: "confirmation_required",
      capture: { id: 42, status: "recording" },
    });
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
