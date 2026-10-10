import "@/test/installEnglishI18n";
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import Acknowledgements from "./Acknowledgements.vue";

describe("Acknowledgements", () => {
  it("shows concise credits without displaying the complete license texts", () => {
    const wrapper = mount(Acknowledgements, { global: { stubs: { RouterLink: true } } });
    const text = wrapper.text();
    expect(text).toContain("MIT licensed");
    expect(text).toContain("monkeyman1979");
    expect(text).toContain("Lucide");
    expect(text).toContain("Google Sans Flex");
    expect(text).toContain("THIRD-PARTY-NOTICES.txt");
    expect(text).not.toContain('THE SOFTWARE IS PROVIDED "AS IS"');
    expect(wrapper.findAll("pre")).toHaveLength(0);
    wrapper.unmount();
  });
});
