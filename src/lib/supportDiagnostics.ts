export interface SupportDiagnosticsWire {
  schema_version: number;
  report_id: string;
  generated_at_ms: number;
  murmur_version: string;
  build_channel: string;
  operating_system: string;
  architecture: string;
  features: unknown;
  permissions: unknown;
  models: unknown;
  retention: unknown;
}

const permissionStates = new Set(["granted", "denied", "not-determined", "unknown"]);
const buildChannels = new Set(["production", "development"]);
const operatingSystems = new Set(["macos", "linux", "windows"]);
const architectures = new Set(["aarch64", "x86_64", "x86", "arm"]);
const featureIds = new Set(["meeting-capture", "meeting-capture-debug"]);
const audioRetentions = new Set(["delete_immediately", "after24_hours", "after7_days", "keep"]);

function object(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? value as Record<string, unknown> : {};
}

function allowlisted(value: unknown, allowed: Set<string>, fallback = "unknown"): string {
  return typeof value === "string" && allowed.has(value) ? value : fallback;
}

function boundedToken(value: unknown, pattern: RegExp, fallback = "unknown"): string {
  return typeof value === "string" && pattern.test(value) ? value : fallback;
}

function nonnegativeInteger(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

/**
 * Render only explicitly selected scalar fields. Unknown object keys are never
 * copied, so a future backend field cannot silently add private content to a
 * report that the user expects to be content-free.
 */
export function formatSupportDiagnostics(input: unknown): string {
  const report = object(input);
  const permissions = object(report.permissions);
  const retention = object(report.retention);
  const generated = nonnegativeInteger(report.generated_at_ms);
  const generatedDate = generated > 0 ? new Date(generated) : null;
  const generatedAt = generatedDate && Number.isFinite(generatedDate.getTime())
    ? generatedDate.toISOString()
    : "unknown";
  const features = Array.isArray(report.features)
    ? report.features.map((value) => allowlisted(value, featureIds, "")).filter(Boolean)
    : [];
  const models = Array.isArray(report.models)
    ? report.models.map((value) => {
      const model = object(value);
      const id = boundedToken(model.id, /^[a-z0-9][a-z0-9-]{0,63}$/);
      const installed = model.installed === true ? "installed" : "missing";
      const bytes = nonnegativeInteger(model.bytes);
      const verification = model.verification === "not-run" ? "not-run" : "unknown";
      return `- ${id}: ${installed}, ${bytes} bytes, verification ${verification}`;
    })
    : [];

  return [
    "Murmur support diagnostics",
    `Schema: ${report.schema_version === 1 ? 1 : "unknown"}`,
    `Report ID: ${boundedToken(report.report_id, /^[a-f0-9]{32}$/)}`,
    `Generated: ${generatedAt}`,
    `Murmur: ${boundedToken(report.murmur_version, /^[0-9A-Za-z.+-]{1,40}$/)}`,
    `Build: ${allowlisted(report.build_channel, buildChannels)}`,
    `System: ${allowlisted(report.operating_system, operatingSystems)} / ${allowlisted(report.architecture, architectures)}`,
    `Features: ${features.length ? features.join(", ") : "none"}`,
    "Permissions:",
    `- Microphone: ${allowlisted(permissions.microphone, permissionStates)}`,
    `- Input Monitoring: ${allowlisted(permissions.input_monitoring, permissionStates)}`,
    `- Accessibility: ${allowlisted(permissions.accessibility, permissionStates)}`,
    "Models:",
    ...(models.length ? models : ["- none"]),
    "Retention:",
    `- Dictation history: ${nonnegativeInteger(retention.dictation_history_days)} days (0 means forever)`,
    `- Meeting transcripts: ${nonnegativeInteger(retention.meeting_transcript_days)} days (0 means forever)`,
    `- Meeting audio: ${allowlisted(retention.meeting_audio, audioRetentions)}`,
  ].join("\n");
}
