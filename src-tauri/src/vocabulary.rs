//! User vocabulary — words and phrases to bias the STT model toward.
//!
//! Surfaced as Settings → Dictionary → Vocabulary in the UI. Frontend
//! is the source of truth: the list is persisted to
//! `localStorage["murmur:vocabulary"]` and mirrored into this state on
//! mount + every change via `set_vocabulary`. The hotkey pipeline
//! reads the list at transcription time and packs it into the STT
//! engine's `initial_prompt`, which Whisper uses as a biasing context
//! so proper nouns / jargon / acronyms get heard correctly on the
//! first pass.
//!
//! SPEC §2.5.

use std::sync::Mutex;

/// Max words we include in the Whisper bias prompt. Whisper's
/// `initial_prompt` caps at roughly 224 tokens; at ~1.3 tokens per
/// short English word, 50 words is a safe ceiling with headroom for
/// the wrapping `"Glossary: … ."` scaffold. Beyond this, adding more
/// words stops improving recognition and starts hurting it (the model
/// over-indexes on an unrealistic context block).
const MAX_WORDS: usize = 50;

pub struct VocabularyState {
    /// In insertion order. Duplicates are rejected at set-time so the
    /// list here is already unique (case-insensitive dedupe, with the
    /// first-seen casing preserved).
    words: Mutex<Vec<String>>,
}

impl VocabularyState {
    pub fn new() -> Self {
        Self {
            words: Mutex::new(Vec::new()),
        }
    }

    /// Build Whisper's `initial_prompt` from the current list. Format:
    /// `"Glossary: {word1}, {word2}, … ."` — a natural-language hint
    /// that cues the model to expect these terms. Returns `None` when
    /// the list is empty so we don't pass an empty prompt (which
    /// Whisper would happily tokenize into dead context).
    pub fn initial_prompt(&self) -> Option<String> {
        let words = self.words.lock().ok()?;
        if words.is_empty() {
            return None;
        }
        let take = words.len().min(MAX_WORDS);
        Some(format!("Glossary: {}.", words[..take].join(", ")))
    }

    fn replace(&self, incoming: Vec<String>) {
        let mut deduped: Vec<String> = Vec::with_capacity(incoming.len());
        for raw in incoming {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if deduped
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(trimmed))
            {
                continue;
            }
            deduped.push(trimmed.to_string());
        }
        if let Ok(mut slot) = self.words.lock() {
            *slot = deduped;
        }
    }
}

impl Default for VocabularyState {
    fn default() -> Self {
        Self::new()
    }
}

#[tauri::command]
pub fn set_vocabulary(
    state: tauri::State<'_, std::sync::Arc<VocabularyState>>,
    meeting_settings: tauri::State<
        '_,
        std::sync::Arc<crate::captures::processing::MeetingProcessingSettingsState>,
    >,
    words: Vec<String>,
) {
    state.replace(words);
    meeting_settings.set_vocabulary_prompt(state.initial_prompt());
    crate::murmur_dev_log!(
        "[murmur] vocabulary: {} word(s)",
        state.words.lock().map(|w| w.len()).unwrap_or(0)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_returns_no_prompt() {
        let s = VocabularyState::new();
        assert_eq!(s.initial_prompt(), None);
    }

    #[test]
    fn prompt_format_is_glossary_comma_separated() {
        let s = VocabularyState::new();
        s.replace(vec!["Murmur".into(), "Tauri".into(), "whisper.cpp".into()]);
        assert_eq!(
            s.initial_prompt(),
            Some("Glossary: Murmur, Tauri, whisper.cpp.".to_string())
        );
    }

    #[test]
    fn replace_dedupes_case_insensitively_preserving_first_casing() {
        let s = VocabularyState::new();
        s.replace(vec!["API".into(), "api".into(), "Api".into()]);
        assert_eq!(s.initial_prompt(), Some("Glossary: API.".to_string()));
    }

    #[test]
    fn replace_trims_whitespace_and_drops_empties() {
        let s = VocabularyState::new();
        s.replace(vec![
            "  Ian  ".into(),
            "".into(),
            "\t\n".into(),
            "Vue".into(),
        ]);
        assert_eq!(s.initial_prompt(), Some("Glossary: Ian, Vue.".to_string()));
    }

    #[test]
    fn prompt_caps_at_max_words() {
        let s = VocabularyState::new();
        let many: Vec<String> = (0..(MAX_WORDS + 10)).map(|i| format!("word{i}")).collect();
        s.replace(many);
        let prompt = s.initial_prompt().unwrap();
        // Count commas + 1 to get the word count in the glossary body.
        let body = prompt
            .trim_start_matches("Glossary: ")
            .trim_end_matches('.');
        assert_eq!(body.split(", ").count(), MAX_WORDS);
    }
}
