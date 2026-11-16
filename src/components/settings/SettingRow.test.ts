import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import Row from "./SettingRow.vue";

describe("settings row alignment", () => {
  it("keeps labels at the top beside tall placement controls", () => {
    const wrapper = mount(Row, { props: { title: "Placement", description: "Independent choice" }, slots: { default: "<div>Picker</div>" } });
    expect(wrapper.classes()).toContain("sm:items-start");
    expect(wrapper.classes()).not.toContain("sm:items-center");
    wrapper.unmount();
  });
});
