import { shallowMount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import IconTooltip from "./IconTooltip.vue";

const stubs = {
  TooltipProvider: {
    template: "<div><slot /></div>",
  },
  Tooltip: {
    template: "<div><slot /></div>",
  },
  TooltipTrigger: {
    template: "<div><slot /></div>",
  },
  TooltipContent: {
    props: ["side", "sideOffset"],
    template: '<div data-test="tooltip-content" :data-side="side" :data-side-offset="sideOffset"><slot /></div>',
  },
};

describe("IconTooltip", () => {
  it("renders a shared tooltip label around the action", () => {
    const wrapper = shallowMount(IconTooltip, {
      props: { label: "Copy text", side: "right" },
      slots: {
        default: '<button aria-label="Copy text">Copy</button>',
      },
      global: { stubs },
    });

    expect(wrapper.get('button[aria-label="Copy text"]')).toBeTruthy();
    expect(wrapper.get('[data-test="tooltip-content"]').text()).toBe("Copy text");
    expect(wrapper.get('[data-test="tooltip-content"]').attributes("data-side")).toBe("right");
    expect(wrapper.get('[data-test="tooltip-content"]').attributes("data-side-offset")).toBe("6");
  });
});
