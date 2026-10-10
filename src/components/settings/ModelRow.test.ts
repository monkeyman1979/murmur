import { describe, expect, it } from "vitest";
import "@/test/installEnglishI18n";
import { mount } from "@vue/test-utils";
import ModelRow from "./ModelRow.vue";

const entry = {
  id: "test-model",
  display_name: "Test model",
  hf_repo_path: "example/repo",
  filename: "model.bin",
  approx_bytes: 100,
  sha256: "a".repeat(64),
};
const info = {
  path: "/generic/model.bin",
  exists: true,
  size: 100,
  modified: 1,
  env_override: false,
  download_url: "",
  approx_download_bytes: 100,
};

function render(options: { downloading?: boolean; verifying?: boolean; canDelete?: boolean } = {}) {
  return mount(ModelRow, {
    props: {
      entry,
      info,
      isActive: false,
      canDelete: true,
      control: "status",
      ...options,
    },
    global: {
      stubs: {
        IconTooltip: { template: "<span><slot /></span>" },
      },
    },
  });
}

describe("ModelRow lifecycle actions", () => {
  it("omits Delete entirely when the model is active or the only downloaded model", () => {
    const wrapper = render({ canDelete: false });
    expect(wrapper.find('[aria-label="Delete model"]').exists()).toBe(false);
    expect(wrapper.find('[aria-label="Check this model file for corruption"]').exists()).toBe(true);
  });

  it("offers Delete for an eligible inactive model", async () => {
    const wrapper = render({ canDelete: true });
    await wrapper.get('[aria-label="Delete model"]').trigger("click");
    expect(wrapper.emitted("delete")).toHaveLength(1);
  });

  it.each([{ downloading: true }, { verifying: true }])(
    "hides conflicting maintenance actions while busy",
    (busy) => {
      const wrapper = render(busy);
      for (const label of [
        "Check this model file for corruption",
        "Re-download model",
        "Show in Finder",
        "Delete model",
      ]) {
        expect(wrapper.find(`[aria-label="${label}"]`).exists()).toBe(false);
      }
    },
  );
});
