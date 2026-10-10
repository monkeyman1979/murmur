import { describe, expect, it } from "vitest";
import { formatSupportDiagnostics } from "./supportDiagnostics";

describe("formatSupportDiagnostics", () => {
  it("renders the allowlisted schema and drops unexpected private fields", () => {
    const report = formatSupportDiagnostics({
      schema_version: 1,
      report_id: "0123456789abcdef0123456789abcdef",
      generated_at_ms: Date.UTC(2026, 8, 14),
      murmur_version: "0.1.0-pre.1",
      build_channel: "production",
      operating_system: "macos",
      architecture: "aarch64",
      features: ["meeting-capture", "/Users/private/feature"],
      permissions: {
        microphone: "granted",
        input_monitoring: "not-determined",
        accessibility: "denied",
        transcript: "PRIVATE TRANSCRIPT",
      },
      models: [{
        id: "whisper-small-multilingual",
        installed: true,
        bytes: 487_601_967,
        verification: "not-run",
        path: "/Users/private/Library/model.bin",
        download_url: "https://example.invalid/?token=SECRET",
      }],
      retention: {
        dictation_history_days: 30,
        meeting_transcript_days: 0,
        meeting_audio: "after24_hours",
        license_key: "SECRET-LICENSE-TOKEN",
      },
      focused_app: "Private Client Matter",
      clipboard: "PRIVATE TRANSCRIPT",
    });

    expect(report).toContain("Report ID: 0123456789abcdef0123456789abcdef");
    expect(report).toContain("whisper-small-multilingual: installed");
    expect(report).toContain("Microphone: granted");
    for (const forbidden of [
      "PRIVATE TRANSCRIPT",
      "/Users/private",
      "example.invalid",
      "SECRET",
      "Private Client Matter",
    ]) {
      expect(report).not.toContain(forbidden);
    }
  });

  it("fails closed when a safe integer is outside the JavaScript Date range", () => {
    expect(() => formatSupportDiagnostics({
      generated_at_ms: Number.MAX_SAFE_INTEGER,
    })).not.toThrow();
    expect(formatSupportDiagnostics({
      generated_at_ms: Number.MAX_SAFE_INTEGER,
    })).toContain("Generated: unknown");
  });

  it("fails closed to bounded placeholders for malformed fields", () => {
    const report = formatSupportDiagnostics({
      schema_version: 99,
      report_id: "../../private",
      generated_at_ms: "yesterday",
      murmur_version: "/Users/private",
      build_channel: "customer-name",
      operating_system: "private-hostname",
      architecture: "private-cpu-label",
      permissions: {},
      models: [{ id: "PRIVATE MODEL /Users/private", installed: "yes", bytes: -1 }],
      retention: { meeting_audio: "custom-private-value" },
    });

    expect(report).not.toContain("private");
    expect(report).toContain("Schema: unknown");
    expect(report).toContain("Report ID: unknown");
    expect(report).toContain("Generated: unknown");
    expect(report).toContain("unknown: missing, 0 bytes, verification unknown");
  });
});
