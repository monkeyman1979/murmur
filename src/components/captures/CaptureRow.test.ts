import "@/test/installEnglishI18n";
import { shallowMount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { CaptureState, CaptureSummary } from "@/lib/captures";

const pushMock = vi.hoisted(() => vi.fn());

vi.mock("vue-router", () => ({
  useRouter: () => ({ push: pushMock }),
}));

import CaptureRow from "./CaptureRow.vue";

const summary = {
  id: 42,
  status: "completed",
  title: "Planning review",
  created_at: 1,
  started_at: 2,
  ended_at: 3,
  active_duration_ms: 1_000,
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
  retry_unavailable_reason: null,
  audio_retention: null,
  audio_delete_at: null,
  audio_bytes: 0,
  audio_kept: false,
} satisfies CaptureSummary;

const stubs = {
  MeetingTitleEditor: {
    props: ["title", "to"],
    template: '<div><a data-test="title" :href="to" @click.prevent>{{ title }}</a><button data-test="rename">Rename</button></div>',
  },
  CaptureStatusBadge: { template: '<span data-test="status">Completed</span>' },
  ProcessingProgress: { template: "<div />" },
  CaptureActions: {
    props: ["actions", "primary", "primaryLast", "busyActions", "detailTo"],
    template: '<div :data-actions="actions.join(\',\')" :data-primary="primary" :data-primary-last="primaryLast" :data-detail-to="detailTo"><slot /><button data-test="action" :disabled="busyActions?.includes(primary)">Copy for AI</button></div>',
  },
  Tooltip: { template: "<div><slot /></div>" },
  TooltipTrigger: { template: "<div><slot /></div>" },
  TooltipContent: { template: "<div><slot /></div>" },
};

function mountRow() {
  return shallowMount(CaptureRow, {
    props: { summary, allowedActions: ["copy_for_ai"] },
    global: { stubs },
  });
}

describe("CaptureRow saved duration", () => {
  it.each([
    "draft", "selecting_source", "starting", "recording",
    "pausing", "paused", "resuming", "stopping",
  ] satisfies CaptureState[])("hides checkpoint duration during %s without leaving a separator", async (status) => {
    const wrapper = shallowMount(CaptureRow, {
      props: { summary: { ...summary, status, ended_at: null, active_duration_ms: 0 } },
      global: { stubs },
    });

    expect(wrapper.text()).not.toContain("00:00");
    expect(wrapper.find('[data-testid="capture-duration"]').exists()).toBe(false);
    expect(wrapper.get("time").attributes("datetime")).toBe(new Date(2).toISOString());
    expect(wrapper.get("time").element.previousElementSibling).toBeNull();
    expect(wrapper.find('[data-test="status"]').exists()).toBe(true);

    // A persisted checkpoint is still not a live timer, even when nonzero.
    await wrapper.setProps({ summary: { ...summary, status, ended_at: null, active_duration_ms: 120_000 } });
    expect(wrapper.text()).not.toContain("02:00");
    expect(wrapper.find('[data-testid="capture-duration"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it.each([
    "transcribing", "finalizing", "completed", "interrupted", "capture_failed",
    "transcription_failed", "finalization_failed", "cancelled", "cleanup_failed",
  ] satisfies CaptureState[])("shows saved duration after recording in %s", (status) => {
    const wrapper = shallowMount(CaptureRow, {
      props: { summary: { ...summary, status, active_duration_ms: 32_000 } },
      global: { stubs },
    });

    expect(wrapper.get('[data-testid="capture-duration"]').text()).toBe("00:32");
    expect(wrapper.get("time").element.previousElementSibling?.textContent).toBe("·");
    if (status === "completed") expect(wrapper.text()).toContain("10 words");
    wrapper.unmount();
  });

  it("reveals duration only after the stop transition, without remounting", async () => {
    const wrapper = shallowMount(CaptureRow, {
      props: { summary: { ...summary, status: "recording", ended_at: null, active_duration_ms: 0 } },
      global: { stubs },
    });

    expect(wrapper.find('[data-testid="capture-duration"]').exists()).toBe(false);
    await wrapper.setProps({ summary: { ...summary, status: "stopping", ended_at: null, active_duration_ms: 19_000 } });
    expect(wrapper.find('[data-testid="capture-duration"]').exists()).toBe(false);
    await wrapper.setProps({ summary: { ...summary, status: "transcribing", active_duration_ms: 32_000 } });
    expect(wrapper.get('[data-testid="capture-duration"]').text()).toBe("00:32");
    await wrapper.setProps({ summary: { ...summary, status: "completed", active_duration_ms: 32_000 } });
    expect(wrapper.get('[data-testid="capture-duration"]').text()).toBe("00:32");
    expect(wrapper.text()).toContain("10 words");
    wrapper.unmount();
  });

  it("keeps historical durations visible while a different meeting is recording", () => {
    const wrapper = shallowMount(CaptureRow, {
      props: { summary: { ...summary, active_duration_ms: 32_000 }, recordingActive: true },
      global: { stubs },
    });

    expect(wrapper.get('[data-testid="capture-duration"]').text()).toBe("00:32");
    expect(wrapper.text()).toContain("10 words");
    wrapper.unmount();
  });
});

describe("CaptureRow navigation", () => {
  beforeEach(() => {
    pushMock.mockReset();
  });

  it("presents the row as clickable and opens detail from non-action content", async () => {
    const wrapper = mountRow();
    const row = wrapper.get("article");

    expect(row.classes()).toContain("cursor-pointer");
    expect(row.classes()).toContain("lg:grid-cols-[minmax(0,1fr)_auto_max-content]");
    expect(row.classes()).not.toContain("lg:grid-cols-[minmax(0,1fr)_auto_minmax(0,25rem)]");
    expect(wrapper.get("time").attributes("datetime")).toBe(new Date(2).toISOString());
    expect(wrapper.get('[data-primary="copy_for_ai"]').attributes("data-primary-last")).toBe("true");
    await wrapper.get('[data-test="status"]').trigger("click");

    expect(pushMock).toHaveBeenCalledWith("/captures/42");
  });

  it("leaves title editing and meeting actions independent", async () => {
    const wrapper = mountRow();

    await wrapper.get('[data-test="title"]').trigger("click");
    await wrapper.get('[data-test="rename"]').trigger("click");
    await wrapper.get('[data-test="action"]').trigger("click");

    expect(pushMock).not.toHaveBeenCalled();
  });

  it("offers only meeting deletion when neither transcript nor usable audio exists", () => {
    const wrapper = shallowMount(CaptureRow, {
      props: {
        summary: {
          ...summary,
          status: "transcription_failed",
          markdown_status: "not_applicable",
          has_recovery_audio: true,
          can_retry_transcription: false,
          retry_unavailable_reason: "audio_unavailable",
        },
      },
      global: { stubs },
    });

    const actions = wrapper.get('[data-actions]').attributes("data-actions");
    expect(actions).toBe("delete");
    expect(wrapper.get('[data-actions]').attributes("data-detail-to")).toBeUndefined();
    expect(wrapper.text()).not.toContain("Audio retained");
  });

  it("keeps queued Pause in More, but promotes it only once processing starts", async () => {
    const processing = { ...summary, status: "transcribing" as const, markdown_status: "not_applicable" as const, transcription_paused: false };
    const wrapper = shallowMount(CaptureRow, {
      props: {
        summary: processing,
        allowedActions: ["pause_transcription", "resume_transcription", "cancel_transcription"],
        recordingActive: true,
      },
      global: { stubs },
    });
    expect(wrapper.get('[data-actions]').attributes("data-primary")).toBeUndefined();
    expect(wrapper.get('[data-actions]').attributes("data-actions")).toContain("pause_transcription");
    expect(wrapper.text()).not.toContain("Transcription will resume after recording ends.");
    await wrapper.setProps({ recordingActive: false, summary: { ...processing, transcription_queued: true } });
    expect(wrapper.get('[data-actions]').attributes("data-primary")).toBeUndefined();
    await wrapper.setProps({ summary: { ...processing, transcription_queued: false } });
    expect(wrapper.get('[data-primary="pause_transcription"]').attributes("data-actions")).toContain("cancel_transcription");
    await wrapper.setProps({ summary: { ...processing, transcription_paused: true } });
    expect(wrapper.get('[data-primary="resume_transcription"]').attributes("data-actions")).toContain("cancel_transcription");
    expect(wrapper.text()).toContain("Resume transcription when you're ready.");
  });

  it("disables a recoverable row while its retry token is pending", () => {
    const wrapper = shallowMount(CaptureRow, {
      props: {
        summary: {
          ...summary,
          status: "interrupted",
          markdown_status: "not_applicable",
          has_recovery_audio: true,
          can_retry_transcription: true,
        },
        allowedActions: ["retry_transcription"],
        busyActions: ["retry_transcription"],
      },
      global: { stubs },
    });

    expect(wrapper.get('[data-primary="retry_transcription"] button').attributes()).toHaveProperty("disabled");
  });

  it("does not navigate when the user is selecting row text", async () => {
    vi.spyOn(window, "getSelection").mockReturnValue({ isCollapsed: false } as Selection);
    const wrapper = mountRow();

    await wrapper.get("article").trigger("click");

    expect(pushMock).not.toHaveBeenCalled();
  });
});
