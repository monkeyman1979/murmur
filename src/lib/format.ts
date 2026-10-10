const numberFormatter = new Intl.NumberFormat(undefined);
const timeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: "numeric",
  minute: "2-digit",
});

export function formatInteger(n: number, locale?: string): string {
  return (locale ? new Intl.NumberFormat(locale) : numberFormatter).format(Math.round(n));
}

export interface DurationPart {
  value: string;
  unit: string;
}

function durationPart(value: number, singular: string, plural: string, locale?: string): DurationPart {
  return { value: formatInteger(value, locale), unit: value === 1 ? singular : plural };
}

/**
 * Structured duration for stat tiles. Keeping values separate from units lets
 * the numbers retain the visual emphasis used by the other dashboard stats.
 */
export function formatDurationParts(ms: number, locale?: string): DurationPart[] {
  if (ms <= 0) return [durationPart(0, "second", "seconds", locale)];
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return [durationPart(seconds, "second", "seconds", locale)];
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return [durationPart(minutes, "minute", "minutes", locale)];
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return [durationPart(hours, "hour", "hours", locale)];
  const days = Math.floor(hours / 24);
  const remHours = hours - days * 24;
  return [
    durationPart(days, "day", "days", locale),
    ...(remHours > 0 ? [durationPart(remHours, "hour", "hours", locale)] : []),
  ];
}

/** Human-friendly compact plain-text form for non-visual consumers. */
export function formatDurationShort(ms: number): string {
  if (ms <= 0) return "0 sec";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds} sec`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} hr`;
  const days = Math.floor(hours / 24);
  const remHours = hours - days * 24;
  return remHours > 0 ? `${days} d ${remHours} hr` : `${days} d`;
}

/**
 * Capture duration used in the transcript rows. Stays in seconds for
 * short captures, which is almost all of them.
 */
export function formatCaptureDuration(ms: number): string {
  return (ms / 1000).toFixed(1) + "s";
}

/** Compact an HH:MM:SS transcript offset while retaining hours when meaningful. */
export function formatTranscriptTimestamp(timestamp: string): string {
  const match = /^(\d+):(\d{2}):(\d{2})$/.exec(timestamp);
  if (!match) return timestamp;
  const [, hours, minutes, seconds] = match;
  return Number(hours) > 0
    ? `${Number(hours)}:${minutes}:${seconds}`
    : `${Number(minutes)}:${seconds}`;
}

/** Format long meeting durations without losing hour-level precision. */
export function formatMeetingDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.round(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) {
    return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

export function formatCaptureDate(ms: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(ms));
}

/**
 * Wall-clock time for when a transcript landed. Uses the viewer's locale
 * so a user in 24-hour regions sees "17:42" while US viewers see "5:42 PM".
 */
export function formatTimestamp(ms: number, locale?: string): string {
  return (locale ? new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit" }) : timeFormatter).format(new Date(ms));
}

/**
 * Truncate with a trailing ellipsis. Used for the recent-transcripts
 * preview so very long captures don't push the timestamp + duration
 * metadata off the row.
 */
export function truncate(text: string, max = 80): string {
  return text.length > max ? text.slice(0, max) + "…" : text;
}

const bucketDateFormatter = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "long",
  day: "numeric",
});

/**
 * Stable YYYY-MM-DD key in the viewer's local timezone. Used for
 * date-bucketed archive views — two timestamps in the same local day
 * produce the same key, independent of UTC boundaries (crucial for users
 * in non-UTC timezones who don't want a single local day to split across
 * two buckets just because UTC midnight fell in the middle of their afternoon).
 */
export function localDateKey(ms: number): string {
  const d = new Date(ms);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/**
 * Human-friendly bucket label: "Today" / "Yesterday" for the two most
 * recent days, the full localized date ("April 19, 2026") for
 * anything older. Matches the iMessage / Slack / Linear pattern.
 * Locale-aware via Intl so a user in another region sees the date in
 * their conventions.
 */
export function formatDateBucket(ms: number, locale?: string): string {
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);

  const key = localDateKey(ms);
  if (key === localDateKey(now.getTime())) return "Today";
  if (key === localDateKey(yesterday.getTime())) return "Yesterday";
  return (locale ? new Intl.DateTimeFormat(locale, { year: "numeric", month: "long", day: "numeric" }) : bucketDateFormatter).format(new Date(ms));
}

export interface LocalDateBucket<T> {
  key: string;
  label: string;
  rows: T[];
}

/** Group newest-first archive rows without disturbing their existing order. */
export function groupByLocalDate<T>(
  rows: readonly T[],
  timestampFor: (row: T) => number,
  locale?: string,
): LocalDateBucket<T>[] {
  const buckets = new Map<string, LocalDateBucket<T>>();
  for (const row of rows) {
    const timestamp = timestampFor(row);
    const key = localDateKey(timestamp);
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = { key, label: formatDateBucket(timestamp, locale), rows: [] };
      buckets.set(key, bucket);
    }
    bucket.rows.push(row);
  }
  return [...buckets.values()];
}

/**
 * Compact human-friendly byte size. GB gets one decimal, MB rounds to
 * the nearest integer — the calling surfaces (speech-model row labels,
 * disk-usage footer) have tight horizontal space and don't benefit
 * from sub-megabyte precision. Binary units (GiB/MiB) to match what
 * `ls -lh` and Finder show for disk sizes; the common "78 MB" vs
 * "77.7 MB" confusion is noise at this zoom level.
 */
export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) {
    return (bytes / (1024 * 1024 * 1024)).toFixed(1) + " GB";
  }
  return Math.round(bytes / (1024 * 1024)) + " MB";
}
