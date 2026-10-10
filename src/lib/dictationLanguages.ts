// Whisper Small Multilingual's 99 language tokens (IDs 0–98). Sourced from
// src-tauri/vendor/whisper-rs-sys/whisper.cpp/src/whisper.cpp::g_lang.
// The vendored decoder also lists Cantonese (ID 99), but the pinned small
// model's vocabulary has 99 language tokens; do not promise that last entry.
// Keep this informational list in sync with the pinned model, not the old
// 16-language manual-hint picker. No network lookup or model download occurs.
export const DICTATION_LANGUAGE_CODES = [
  "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
  "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
  "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
  "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
  "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
  "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln", "ha",
  "ba", "jw", "su",
] as const;

export function displayDictationLanguages(locale: string): { code: string; name: string }[] {
  const names = new Intl.DisplayNames([locale], { type: "language" });
  const collator = new Intl.Collator(locale, { sensitivity: "base" });
  return DICTATION_LANGUAGE_CODES.map((code) => ({
    code,
    // ICU uses the modern ISO code for Javanese, Whisper uses the older jw.
    name: names.of(code === "jw" ? "jv" : code) ?? code,
  })).sort((left, right) => collator.compare(left.name, right.name));
}
