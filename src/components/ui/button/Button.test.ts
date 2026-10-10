import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import { cn } from "@/lib/utils";
import Button from "./Button.vue";
import { buttonVariants } from ".";

describe("Button", () => {
  it("uses a pointer cursor for clickable actions", () => {
    const wrapper = mount(Button, { slots: { default: "Delete now" } });

    expect(wrapper.get("button").classes()).toContain("cursor-pointer");
  });

  it("allows an individual button to opt out of the press translation", () => {
    const classes = cn(
      buttonVariants(),
      "active:not-aria-[haspopup]:translate-y-0",
    );

    expect(classes).toContain("active:not-aria-[haspopup]:translate-y-0");
    expect(classes).not.toContain("active:not-aria-[haspopup]:translate-y-px");
  });

  it("keeps sidebar outline buttons outlined across light and dark content themes", () => {
    const classes = buttonVariants({ variant: "sidebarOutline" });

    expect(classes).toContain("border-sidebar-border");
    expect(classes).toContain("text-sidebar-foreground");
    expect(classes).toContain("hover:bg-transparent");
    expect(classes).toContain("active:bg-sidebar-hover");
    expect(classes).not.toContain("hover:bg-muted");
    expect(classes).not.toContain("dark:hover:bg-muted/50");
  });
});
