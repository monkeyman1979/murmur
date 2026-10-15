import "@/test/installEnglishI18n";
import { shallowMount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import CaptureActions from "./CaptureActions.vue";

describe("CaptureActions", () => {
  it("renders transcript navigation before actions and keeps the primary action trailing", () => {
    const wrapper = shallowMount(CaptureActions, {
      props: {
        actions: ["copy_for_ai", "save_audio", "delete"],
        primary: "copy_for_ai",
        primaryLast: true,
        detailTo: "/captures/42",
      },
      slots: {
        default: '<span data-test="metadata">Metadata</span>',
      },
      global: {
        stubs: {
          Button: {
            template: '<button v-bind="$attrs"><slot /></button>',
          },
          DropdownMenu: {
            template: '<div data-test="overflow"><slot /></div>',
          },
          DropdownMenuTrigger: {
            template: "<div><slot /></div>",
          },
          DropdownMenuContent: {
            template: '<div data-test="menu"><slot /></div>',
          },
          DropdownMenuItem: {
            template: "<div><slot /></div>",
          },
          DropdownMenuSeparator: {
            template: "<hr />",
          },
          RouterLink: {
            props: ["to"],
            template: '<a :href="to"><slot /></a>',
          },
        },
      },
    });

    const metadata = wrapper.get('[data-test="metadata"]').element;
    const overflow = wrapper.get('[data-test="overflow"]').element;
    const transcriptLink = wrapper.get('a[href="/captures/42"]');
    const primary = wrapper.findAll("button").find((button) =>
      button.text().includes("Copy for AI"),
    )!.element;

    expect(transcriptLink.text()).toBe("View transcript");
    expect(metadata.compareDocumentPosition(overflow) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(overflow.compareDocumentPosition(primary) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(overflow.closest("[data-row-interactive]")).not.toBeNull();
    expect(primary.closest("[data-row-interactive]")).not.toBeNull();
    expect(primary.classList.contains("cursor-pointer")).toBe(true);
    expect(wrapper.get('[data-test="menu"]').text()).toContain("Save audio copy");
  });

  it("uses the retry action token and reflects its actual pending state", async () => {
    const wrapper = shallowMount(CaptureActions, {
      props: {
        actions: ["retry_transcription"],
        primary: "retry_transcription",
      },
      global: {
        stubs: {
          Button: {
            props: ["disabled"],
            template: '<button :disabled="disabled"><slot /></button>',
          },
        },
      },
    });

    expect(wrapper.classes()).toEqual(expect.arrayContaining(["min-w-0", "flex-wrap", "lg:flex-nowrap"]));
    expect(wrapper.get("button").text()).toBe("Retry transcription");
    await wrapper.setProps({ busyActions: ["retry_transcription"] });
    expect(wrapper.get("button").attributes()).toHaveProperty("disabled");
    expect(wrapper.get("button").text()).toBe("Working…");
    await wrapper.get("button").trigger("click");
    expect(wrapper.emitted("action")).toBeUndefined();
  });

  it("keeps cancellation in overflow and toggles the accessible pause/play icon", async () => {
    const wrapper = shallowMount(CaptureActions, {
      props: {
        actions: ["pause_transcription", "cancel_transcription"],
        primary: "pause_transcription",
      },
      global: {
        stubs: {
          Button: { template: '<button v-bind="$attrs"><slot /></button>' },
          Tooltip: { template: '<div><slot /></div>' },
          TooltipTrigger: { template: '<div><slot /></div>' },
          TooltipContent: { template: '<span><slot /></span>' },
          DropdownMenu: { template: '<div><slot /></div>' },
          DropdownMenuTrigger: { template: '<div><slot /></div>' },
          DropdownMenuContent: { template: '<div><slot /></div>' },
          DropdownMenuItem: { template: '<div><slot /></div>' },
          Pause: { template: '<svg data-icon="pause" />' },
          Play: { template: '<svg data-icon="play" />' },
        },
      },
    });
    expect(wrapper.get('button[aria-label="Pause transcription"]')).toBeTruthy();
    expect(wrapper.text()).toContain("Cancel transcription");
    await wrapper.get('button[aria-label="Pause transcription"]').trigger("click");
    expect(wrapper.emitted("action")?.[0]).toEqual(["pause_transcription"]);
    await wrapper.setProps({ actions: ["resume_transcription", "cancel_transcription"], primary: "resume_transcription" });
    expect(wrapper.get('button[aria-label="Resume transcription"]')).toBeTruthy();
  });
});
