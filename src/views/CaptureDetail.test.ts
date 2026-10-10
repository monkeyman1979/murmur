import "@/test/installEnglishI18n";
import { flushPromises, shallowMount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

const captureMocks = vi.hoisted(() => ({
  route: null as unknown as { params: { captureId: string }; fullPath: string },
  getDetail: vi.fn(),
  saveAudioCopy: vi.fn(),
  keepAudio: vi.fn(),
  deleteAudio: vi.fn(),
  refreshOne: vi.fn(),
}));

vi.mock("vue-router", async () => {
  const { reactive } = await import("vue");
  captureMocks.route = reactive({ params: { captureId: "42" }, fullPath: "/captures/42" });
  return {
    RouterLink: { props: ["to"], template: '<a :href="to"><slot /></a>' },
    useRoute: () => captureMocks.route,
    useRouter: () => ({ back: vi.fn(), push: vi.fn() }),
  };
});

vi.mock("@/composables/useCaptures", async () => {
  const { reactive } = await import("vue");
  return {
    useCaptures: () => ({
      details: reactive(new Map()),
      getDetail: captureMocks.getDetail,
      stateFor: () => ({ loading: false, error: null }),
      saveAudioCopy: captureMocks.saveAudioCopy,
      keepAudio: captureMocks.keepAudio,
      deleteAudio: captureMocks.deleteAudio,
      refreshOne: captureMocks.refreshOne,
    }),
  };
});

vi.mock("@/composables/useMeetingCapture", async () => {
  const { ref } = await import("vue");
  return {
    useMeetingCapture: () => ({
      activeCapture: ref(null),
      operation: ref(null),
      pause: vi.fn(),
      resume: vi.fn(),
      stop: vi.fn(),
    }),
  };
});

import CaptureDetail from "./CaptureDetail.vue";

const stubs = {
  MeetingTitleEditor: {
    props: ["title"],
    template: '<h1 data-test="title">{{ title }}</h1>',
  },
  CaptureStatusBadge: { template: "<span>Ready</span>" },
  CaptureActions: {
    props: ["actions", "primary", "primaryLast"],
    template: '<div data-test="capture-actions" :data-actions="actions.join(\',\')" :data-primary="primary" :data-primary-last="primaryLast" />',
  },
  ProcessingProgress: { template: "<div />" },
  Collapsible: { template: "<div><slot /></div>" },
  CollapsibleTrigger: { template: "<div><slot /></div>" },
  CollapsibleContent: { template: "<div><slot /></div>" },
  Button: { template: '<button v-bind="$attrs"><slot /></button>' },
  AlertDialog: { template: "<div />" },
  RouterLink: { props: ["to"], template: '<a :href="to"><slot /></a>' },
};

describe("CaptureDetail meeting actions and retained audio", () => {
  it("puts Copy for AI after overflow and exposes retained-audio status and controls", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-07T12:00:00Z"));
    captureMocks.getDetail.mockResolvedValue({
      summary: {
        id: 42,
        status: "completed",
        title: "Planning review",
        created_at: Date.now() - 10_000,
        started_at: Date.now() - 10_000,
        ended_at: Date.now(),
        active_duration_ms: 10_000,
        paused_duration_ms: 0,
        source_name: null,
        language: null,
        word_count: 10,
        error_code: null,
        error_message: null,
        markdown_status: "available",
        processing: null,
        has_recovery_audio: true,
        can_retry_transcription: false,
        audio_retention: "temporary",
        audio_delete_at: Date.now() + 18 * 3_600_000,
        audio_bytes: 1_073_741_824,
        audio_kept: false,
      },
      source_kind: null,
      source_bundle: null,
      transcript_text: "[00:00] Speaker: Hello",
      markdown_path: "/tmp/Planning review.md",
    });

    const wrapper = shallowMount(CaptureDetail, { global: { stubs } });
    await flushPromises();

    const actions = wrapper.get('[data-test="capture-actions"]');
    const detailsButton = wrapper.findAll("button").find((button) => button.text().includes("Details"))!;
    expect(detailsButton.classes()).toContain("active:not-aria-[haspopup]:translate-y-0");
    expect(actions.attributes("data-primary")).toBe("copy_for_ai");
    expect(actions.attributes("data-primary-last")).toBe("true");
    expect(actions.attributes("data-actions")!.split(",")).toContain("copy_path");
    expect(wrapper.text()).toContain("Audio retained for recovery · deletes in 18 hours · 1.0 GB");
    expect(wrapper.text()).toContain("Save audio copy");
    expect(wrapper.text()).toContain("Keep until deleted");
    expect(wrapper.text()).toContain("Delete audio");

    const buttons = wrapper.findAll("button");
    const saveAudioButton = buttons.find((button) => button.text() === "Save audio copy")!;
    const keepAudioButton = buttons.find((button) => button.text() === "Keep until deleted")!;
    const deleteAudioButton = buttons.find((button) => button.text() === "Delete audio")!;
    expect(saveAudioButton.classes()).toContain("cursor-pointer");
    expect(keepAudioButton.classes()).toContain("cursor-pointer");
    expect(deleteAudioButton.classes()).toContain("cursor-pointer");
    await saveAudioButton.trigger("click");
    await keepAudioButton.trigger("click");
    await deleteAudioButton.trigger("click");
    await flushPromises();
    expect(captureMocks.saveAudioCopy).toHaveBeenCalledWith(expect.objectContaining({ id: 42 }));
    expect(captureMocks.keepAudio).toHaveBeenCalledWith(42);
    expect(captureMocks.refreshOne).toHaveBeenCalledWith(42);
    expect(captureMocks.deleteAudio).not.toHaveBeenCalled();

    vi.useRealTimers();
  });

  it("updates real transcript content when the shared detail route changes IDs", async () => {
    captureMocks.getDetail.mockImplementation(async (id: number) => ({
      summary: { id, title: `Fixture ${id}`, status: "completed", created_at: 1, active_duration_ms: 0,
        paused_duration_ms: 0, word_count: 2, markdown_status: "available", audio_bytes: 0 },
      transcript_text: `Private-free fixture transcript ${id}`, markdown_path: null,
    }));
    const wrapper = shallowMount(CaptureDetail, { global: { stubs } });
    await flushPromises();
    expect(wrapper.text()).toContain("Private-free fixture transcript 42");
    captureMocks.route.params.captureId = "43";
    captureMocks.route.fullPath = "/captures/43";
    await flushPromises();
    expect(wrapper.text()).toContain("Private-free fixture transcript 43");
    expect(wrapper.text()).not.toContain("Private-free fixture transcript 42");
    wrapper.unmount();
    captureMocks.route.params.captureId = "42";
    captureMocks.route.fullPath = "/captures/42";
  });

  it("hides unusable audio controls when recovery validation failed", async () => {
    captureMocks.getDetail.mockResolvedValue({
      summary: {
        id: 42,
        status: "transcription_failed",
        title: "Failed meeting",
        created_at: 1,
        started_at: 1,
        ended_at: 2,
        active_duration_ms: 1_000,
        paused_duration_ms: 0,
        source_name: null,
        language: null,
        word_count: 0,
        error_code: "audio_unavailable",
        error_message: null,
        markdown_status: "not_applicable",
        processing: null,
        has_recovery_audio: true,
        can_retry_transcription: false,
        retry_unavailable_reason: "audio_unavailable",
        audio_retention: "failure_preserved",
        audio_delete_at: null,
        audio_bytes: 128,
        audio_kept: false,
      },
      source_kind: null,
      source_bundle: null,
      transcript_text: null,
      markdown_path: null,
    });

    const wrapper = shallowMount(CaptureDetail, { global: { stubs } });
    await flushPromises();

    expect(wrapper.get('[data-test="capture-actions"]').attributes("data-actions")).toBe("delete");
    expect(wrapper.find('[data-test="retained-audio-details"]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain("Save audio copy");
  });

  it("omits the retained-audio detail row when no audio remains", async () => {
    captureMocks.getDetail.mockResolvedValue({
      summary: {
        id: 42,
        status: "completed",
        title: "Planning review",
        created_at: Date.now() - 10_000,
        started_at: Date.now() - 10_000,
        ended_at: Date.now(),
        active_duration_ms: 10_000,
        paused_duration_ms: 0,
        source_name: null,
        language: null,
        word_count: 10,
        error_code: null,
        error_message: null,
        markdown_status: "available",
        processing: null,
        has_recovery_audio: false,
        can_retry_transcription: false,
        audio_retention: null,
        audio_delete_at: null,
        audio_bytes: 0,
        audio_kept: false,
      },
      source_kind: null,
      source_bundle: null,
      transcript_text: "[00:00] Speaker: Hello",
      markdown_path: "/tmp/Planning review.md",
    });

    const wrapper = shallowMount(CaptureDetail, { global: { stubs } });
    await flushPromises();

    expect(wrapper.find('[data-test="retained-audio-details"]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain("No retained audio");
  });
});
