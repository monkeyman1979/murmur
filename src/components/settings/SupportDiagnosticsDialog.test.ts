import "@/test/installEnglishI18n";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import SupportDiagnosticsDialog from "./SupportDiagnosticsDialog.vue";

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));

const clipboard = { writeText: vi.fn() };

function diagnostic(reportId = "0123456789abcdef0123456789abcdef") {
  return {
    schema_version: 1,
    report_id: reportId,
    generated_at_ms: Date.UTC(2026, 8, 14),
    murmur_version: "0.1.0-pre.1",
    build_channel: "production",
    operating_system: "macos",
    architecture: "aarch64",
    features: ["meeting-capture"],
    permissions: {
      microphone: "granted",
      input_monitoring: "granted",
      accessibility: "granted",
    },
    models: [{
      id: "whisper-small-multilingual",
      installed: true,
      bytes: 487_601_967,
      verification: "not-run",
      path: "/Users/private/model.bin",
    }],
    retention: {
      dictation_history_days: 30,
      meeting_transcript_days: 0,
      meeting_audio: "after24_hours",
      license_key: "SECRET-LICENSE-TOKEN",
    },
    transcript: "PRIVATE TRANSCRIPT",
  };
}

beforeEach(() => {
  native.invoke.mockResolvedValue(diagnostic());
  clipboard.writeText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
});

afterEach(() => {
  document.body.innerHTML = "";
  vi.clearAllMocks();
});

describe("SupportDiagnosticsDialog", () => {
  it("previews an allowlisted local report before an explicit copy", async () => {
    const wrapper = mount(SupportDiagnosticsDialog, {
      attachTo: document.body,
      props: { open: true },
    });
    await flushPromises();

    expect(native.invoke).toHaveBeenCalledWith("support_diagnostics");
    expect(document.body.textContent).toContain("paste only what you want to share");
    expect(document.body.textContent).toContain("Public issue reports are visible to everyone");
    expect(document.body.textContent).not.toContain("license data");
    const preview = document.querySelector<HTMLTextAreaElement>(
      'textarea[aria-label="Support diagnostics preview"]',
    )!;
    expect(preview.value).toContain("Murmur support diagnostics");
    expect(preview.value).not.toContain("PRIVATE TRANSCRIPT");
    expect(preview.value).not.toContain("/Users/private");
    expect(preview.value).not.toContain("SECRET-LICENSE-TOKEN");
    expect(clipboard.writeText).not.toHaveBeenCalled();

    const copy = [...document.querySelectorAll("button")]
      .find((button) => button.textContent?.trim() === "Copy diagnostics")!;
    copy.click();
    await flushPromises();
    expect(clipboard.writeText).toHaveBeenCalledWith(preview.value);
    expect(document.body.textContent).toContain("Diagnostics copied");
    wrapper.unmount();
  });

  it("retains the preview and permits retry after a content-free clipboard failure", async () => {
    clipboard.writeText.mockRejectedValueOnce(new Error("/Users/private SECRET-LICENSE-TOKEN"));
    const wrapper = mount(SupportDiagnosticsDialog, {
      attachTo: document.body,
      props: { open: true },
    });
    await flushPromises();
    const preview = document.querySelector<HTMLTextAreaElement>(
      'textarea[aria-label="Support diagnostics preview"]',
    )!;
    const expected = preview.value;

    const copy = [...document.querySelectorAll("button")]
      .find((button) => button.textContent?.trim() === "Copy diagnostics")!;
    copy.click();
    await flushPromises();
    expect(document.body.textContent).toContain("Diagnostics could not be copied");
    expect(document.body.textContent).not.toContain("/Users/private");
    expect(preview.value).toBe(expected);

    copy.click();
    await flushPromises();
    expect(clipboard.writeText).toHaveBeenLastCalledWith(expected);
    expect(document.body.textContent).toContain("Diagnostics copied");
    wrapper.unmount();
  });

  it("ignores stale copy completion across close and reopen", async () => {
    let finishCopy!: () => void;
    clipboard.writeText.mockReturnValueOnce(new Promise<void>((resolve) => { finishCopy = resolve; }));
    native.invoke
      .mockResolvedValueOnce(diagnostic("0123456789abcdef0123456789abcdef"))
      .mockResolvedValueOnce(diagnostic("fedcba9876543210fedcba9876543210"));
    const wrapper = mount(SupportDiagnosticsDialog, {
      attachTo: document.body,
      props: { open: true },
    });
    await flushPromises();
    const firstPreview = document.querySelector<HTMLTextAreaElement>(
      'textarea[aria-label="Support diagnostics preview"]',
    )!.value;
    [...document.querySelectorAll("button")]
      .find((button) => button.textContent?.trim() === "Copy diagnostics")!
      .click();
    await flushPromises();

    await wrapper.setProps({ open: false });
    await wrapper.setProps({ open: true });
    await flushPromises();
    const secondPreview = document.querySelector<HTMLTextAreaElement>(
      'textarea[aria-label="Support diagnostics preview"]',
    )!.value;
    expect(secondPreview).toContain("fedcba9876543210fedcba9876543210");
    expect(secondPreview).not.toBe(firstPreview);

    finishCopy();
    await flushPromises();
    expect(clipboard.writeText).toHaveBeenCalledWith(firstPreview);
    expect(document.body.textContent).not.toContain("Diagnostics copied");
    wrapper.unmount();
  });

  it("does not display an opaque native generation error", async () => {
    native.invoke.mockRejectedValue(new Error("/Users/private SECRET-LICENSE-TOKEN"));
    const wrapper = mount(SupportDiagnosticsDialog, {
      attachTo: document.body,
      props: { open: true },
    });
    await flushPromises();

    expect(document.body.textContent).toContain("Diagnostics could not be generated");
    expect(document.body.textContent).not.toContain("/Users/private");
    expect(document.body.textContent).not.toContain("SECRET-LICENSE-TOKEN");
    wrapper.unmount();
  });
});
