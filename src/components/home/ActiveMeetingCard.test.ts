import "@/test/installEnglishI18n";
import { shallowMount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

const stopMock = vi.hoisted(() => vi.fn());

vi.mock("@/composables/useMeetingCapture", async () => {
  const { ref } = await import("vue");
  return {
    useMeetingCapture: () => ({
      activeCapture: ref({ id: 42, title: "Meeting 42", status: "recording" }),
      activeDurationMs: ref(12_000),
      action: ref(null),
      controls: ref(["pause", "stop", "cancel"]),
      error: ref(null),
      pause: vi.fn(),
      resume: vi.fn(),
      stop: stopMock,
      cancel: vi.fn(),
    }),
  };
});

import ActiveMeetingCard from "./ActiveMeetingCard.vue";

const stubs = {
  Card: { template: "<section><slot /></section>" },
  CardContent: { template: "<div><slot /></div>" },
  Button: { template: '<button v-bind="$attrs"><slot /></button>' },
  DropdownMenu: { template: "<div><slot /></div>" },
  DropdownMenuTrigger: { template: "<div><slot /></div>" },
  DropdownMenuContent: {
    template: '<div data-test="discard-menu" v-bind="$attrs"><slot /></div>',
  },
  DropdownMenuItem: {
    template: '<button data-test="discard-item" v-bind="$attrs"><slot /></button>',
  },
  AlertDialog: { template: "<div />" },
};

describe("ActiveMeetingCard", () => {
  it("uses the shared Stop label for the recording stop action", async () => {
    stopMock.mockReset();
    const wrapper = shallowMount(ActiveMeetingCard, { global: { stubs } });

    const stop = wrapper.get('[aria-label="Stop meeting recording and transcribe"]');
    expect(stop.text()).toBe("Stop");
    expect(stop.attributes("title")).toBeUndefined();

    await stop.trigger("click");
    expect(stopMock).toHaveBeenCalledOnce();
  });

  it("keeps the discard action on one line", () => {
    const wrapper = shallowMount(ActiveMeetingCard, { global: { stubs } });

    expect(wrapper.get('[data-test="discard-menu"]').classes()).toEqual(
      expect.arrayContaining(["w-max", "min-w-40"]),
    );
    expect(wrapper.get('[data-test="discard-item"]').classes()).toContain("whitespace-nowrap");
    expect(wrapper.get('[data-test="discard-item"]').text()).toBe("Discard recording");
  });
});
