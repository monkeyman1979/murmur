const LEGACY_DICTATION_LANGUAGE_KEY = "murmur:dictation-language";

/** Remove an old forced language hint. Rust now always auto-detects dictation. */
export function clearLegacyDictationLanguage(): void {
  try {
    globalThis.localStorage?.removeItem(LEGACY_DICTATION_LANGUAGE_KEY);
  } catch {
    // Private mode or unavailable storage cannot alter native auto-detection.
  }
}
