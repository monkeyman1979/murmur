import "@/style.css";

import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import { nextTick } from "vue";
import { toast } from "vue-sonner";

import AppToaster from "@/components/AppToaster.vue";

afterEach(() => {
  toast.dismiss();
  document.body.innerHTML = "";
});

describe("AppToaster", () => {
  it("uses neutral toast colors without a visible leading status icon", async () => {
    const wrapper = mount(AppToaster, {
      props: { theme: "light" },
      attachTo: document.body,
    });

    toast.message("Saved");
    await nextTick();
    await nextTick();

    const toastElement = wrapper.get("[data-sonner-toast]");
    expect(toastElement.attributes("data-rich-colors")).toBe("false");

    expect(toastElement.find("[data-icon]").exists()).toBe(false);
    expect(toastElement.find("[data-description]").exists()).toBe(false);
    expect(
      toastElement.get("[data-close-button]").attributes("data-close-button-position"),
    ).toBe("top-right");

    wrapper.unmount();
  });
});
