import "@/test/installEnglishI18n";
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import TranscriptRow from "@/components/history/TranscriptRow.vue";
import { IconTooltip } from "@/components/ui/tooltip";
import type { TranscriptRecord } from "@/composables/useHistory";

function transcript(id: number): TranscriptRecord {
  return {
    id,
    created_at: Date.now(),
    text_raw: `Test dictation ${id}`,
    text_final: `Test dictation ${id}`,
    duration_ms: 1_500,
    word_count: 3,
    app_name: "Test App",
    app_bundle: "com.example.test",
  };
}

function actions(wrapper: ReturnType<typeof mount>) {
  return wrapper.get("[data-transcript-actions]");
}

describe("TranscriptRow actions", () => {
  it("keeps the copy icon unchanged after emitting a copy request", async () => {
    const row = transcript(1);
    const wrapper = mount(TranscriptRow, { props: { row } });
    const copyButton = wrapper.get('button[aria-label="Copy text"]');

    await copyButton.trigger("click");

    expect(wrapper.emitted("copy")).toEqual([[row]]);
    expect(copyButton.attributes("aria-label")).toBe("Copy text");
    expect(copyButton.attributes("title")).toBeUndefined();
    expect(wrapper.findAllComponents(IconTooltip).map((tooltip) => tooltip.props("label"))).toEqual([
      "Remember a correction",
      "Copy text",
      "Delete entry",
    ]);
    expect(copyButton.find("svg").exists()).toBe(true);
  });

  it("keeps actions visible on only the row currently under the pointer", async () => {
    const first = mount(TranscriptRow, { props: { row: transcript(1) }, attachTo: document.body });
    const second = mount(TranscriptRow, { props: { row: transcript(2) }, attachTo: document.body });

    expect(actions(first).attributes("style")).toContain("display: none");
    expect(actions(second).attributes("style")).toContain("display: none");

    await first.trigger("pointermove");
    expect(actions(first).attributes("style") ?? "").not.toContain("display: none");
    expect(actions(second).attributes("style")).toContain("display: none");

    await second.trigger("pointermove");
    expect(actions(first).attributes("style")).toContain("display: none");
    expect(actions(second).attributes("style") ?? "").not.toContain("display: none");

    document.body.dispatchEvent(new MouseEvent("pointermove", { bubbles: true }));
    await second.vm.$nextTick();
    expect(actions(first).attributes("style")).toContain("display: none");
    expect(actions(second).attributes("style")).toContain("display: none");

    first.unmount();
    second.unmount();
  });
});
