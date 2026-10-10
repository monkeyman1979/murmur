//! Deterministic find/replace applied after STT and before history/paste (§2.6).
//!
//! Surfaced as Settings → Dictionary → Corrections in the UI. Frontend
//! is the source of truth: the list is persisted to
//! `localStorage["murmur:corrections"]` and mirrored into this state
//! on mount + every change via `set_corrections`. The hotkey pipeline
//! calls [`CorrectionsState::apply`] on the Whisper transcript before
//! history and paste.
//!
//! Rules are applied literally, not as regex — user input is escaped
//! with [`regex::escape`] so punctuation in the `find` field doesn't
//! accidentally act as a pattern. `whole_word` wraps the pattern in
//! `\b…\b`; `case_sensitive = false` prepends `(?i)`. Compile happens
//! per-apply; at the scale corrections run (tens of rules, one
//! hold-to-talk at a time) the cost is dwarfed by Whisper.

use std::sync::Mutex;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionRule {
    pub find: String,
    pub replace: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl CorrectionRule {
    /// Compile this rule into a [`Regex`]. Returns `None` when the
    /// `find` field is empty after trimming — an empty pattern would
    /// match every position in the input and replace endlessly.
    fn compile(&self) -> Option<Regex> {
        let pattern = self.find.trim();
        if pattern.is_empty() {
            return None;
        }
        let escaped = regex::escape(pattern);
        let wrapped = if self.whole_word {
            format!(r"\b{escaped}\b")
        } else {
            escaped
        };
        RegexBuilder::new(&wrapped)
            .case_insensitive(!self.case_sensitive)
            .build()
            .ok()
    }

    /// Two rules collide when they would target the same text with the
    /// same matching semantics. `replace` is intentionally *not* part
    /// of the key — the user changing the replacement for a given
    /// trigger is an edit, not a new rule.
    fn same_target(&self, other: &Self) -> bool {
        self.find.trim().eq_ignore_ascii_case(other.find.trim())
            && self.case_sensitive == other.case_sensitive
            && self.whole_word == other.whole_word
    }
}

pub struct CorrectionsState {
    /// Rules in user-specified order. Applied top-to-bottom so users
    /// can chain corrections (e.g. a broad fix, then a narrower
    /// override). Duplicates (same find + same flags) are rejected at
    /// set-time; first-seen wins.
    rules: Mutex<Vec<CorrectionRule>>,
}

impl CorrectionsState {
    pub fn new() -> Self {
        Self {
            rules: Mutex::new(Vec::new()),
        }
    }

    /// Run every rule against `input` in list order. Rules with an
    /// empty `find` or a malformed pattern are skipped silently — the
    /// frontend rejects empty `find` at add-time, so reaching this
    /// code path means someone hand-edited localStorage and we'd
    /// rather degrade than panic.
    pub fn apply(&self, input: &str) -> String {
        let Ok(rules) = self.rules.lock() else {
            return input.to_string();
        };
        apply_correction_rules(&rules, input)
    }

    pub fn snapshot(&self) -> Vec<CorrectionRule> {
        self.rules
            .lock()
            .map(|rules| rules.clone())
            .unwrap_or_default()
    }

    pub(crate) fn replace(&self, incoming: Vec<CorrectionRule>) {
        let deduped = normalize_rules(incoming);
        if let Ok(mut slot) = self.rules.lock() {
            *slot = deduped;
        }
    }
}

pub fn apply_correction_rules(rules: &[CorrectionRule], input: &str) -> String {
    let mut out = input.to_string();
    for rule in rules {
        let Some(re) = rule.compile() else { continue };
        out = re.replace_all(&out, rule.replace.as_str()).into_owned();
    }
    out
}

fn normalize_rules(incoming: Vec<CorrectionRule>) -> Vec<CorrectionRule> {
    let mut deduped: Vec<CorrectionRule> = Vec::with_capacity(incoming.len());
    for rule in incoming {
        if rule.find.trim().is_empty() {
            continue;
        }
        if deduped.iter().any(|existing| existing.same_target(&rule)) {
            continue;
        }
        deduped.push(rule);
    }
    deduped
}

impl Default for CorrectionsState {
    fn default() -> Self {
        Self::new()
    }
}

#[tauri::command]
pub fn set_corrections(
    state: tauri::State<'_, std::sync::Arc<CorrectionsState>>,
    meeting_settings: tauri::State<
        '_,
        std::sync::Arc<crate::captures::processing::MeetingProcessingSettingsState>,
    >,
    rules: Vec<CorrectionRule>,
) {
    state.replace(rules);
    meeting_settings.set_corrections(state.snapshot());
    crate::murmur_dev_log!("[murmur] corrections: {} rule(s)", state.snapshot().len());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(find: &str, replace: &str, case_sensitive: bool, whole_word: bool) -> CorrectionRule {
        CorrectionRule {
            find: find.into(),
            replace: replace.into(),
            case_sensitive,
            whole_word,
        }
    }

    #[test]
    fn empty_ruleset_is_passthrough() {
        let s = CorrectionsState::new();
        assert_eq!(s.apply("hello world"), "hello world");
    }

    #[test]
    fn substring_replaces_everywhere() {
        let s = CorrectionsState::new();
        s.replace(vec![rule("cat", "dog", false, false)]);
        assert_eq!(s.apply("cat cats concatenate"), "dog dogs condogenate");
    }

    #[test]
    fn whole_word_respects_boundaries() {
        let s = CorrectionsState::new();
        s.replace(vec![rule("cat", "dog", false, true)]);
        assert_eq!(s.apply("cat cats concatenate"), "dog cats concatenate");
    }

    #[test]
    fn case_insensitive_by_default() {
        let s = CorrectionsState::new();
        s.replace(vec![rule("tauri", "Tauri", false, true)]);
        assert_eq!(s.apply("tauri TAURI Tauri"), "Tauri Tauri Tauri");
    }

    #[test]
    fn case_sensitive_only_exact() {
        let s = CorrectionsState::new();
        s.replace(vec![rule("i", "I", true, true)]);
        assert_eq!(s.apply("i think I am"), "I think I am");
    }

    #[test]
    fn rules_apply_in_list_order() {
        let s = CorrectionsState::new();
        s.replace(vec![
            rule("ok", "okay", false, true),
            rule("okay", "alright", false, true),
        ]);
        assert_eq!(s.apply("ok"), "alright");
    }

    #[test]
    fn empty_find_is_rejected_at_set_time() {
        let s = CorrectionsState::new();
        s.replace(vec![
            rule("   ", "junk", false, true),
            rule("keep", "kept", false, true),
        ]);
        assert_eq!(s.apply("keep going"), "kept going");
    }

    #[test]
    fn duplicate_target_first_wins() {
        let s = CorrectionsState::new();
        s.replace(vec![
            rule("lol", "haha", false, true),
            rule("lol", "ignored", false, true),
        ]);
        assert_eq!(s.apply("lol"), "haha");
    }

    #[test]
    fn same_find_different_flags_both_kept() {
        let s = CorrectionsState::new();
        s.replace(vec![
            rule("ok", "okay", false, true),
            rule("ok", "case-sensitive!", true, true),
        ]);
        // Sequential application: the first rule rewrites "ok" → "okay",
        // so the case-sensitive "ok" pattern no longer matches. That's
        // deliberate — user sequencing dominates.
        assert_eq!(s.apply("ok"), "okay");
    }

    #[test]
    fn regex_metacharacters_in_find_are_literal() {
        let s = CorrectionsState::new();
        s.replace(vec![rule("foo.bar", "baz", false, false)]);
        // "fooXbar" would match if the dot were treated as regex.
        assert_eq!(s.apply("fooXbar foo.bar"), "fooXbar baz");
    }

    #[test]
    fn empty_replace_acts_as_deletion() {
        let s = CorrectionsState::new();
        s.replace(vec![rule("um", "", false, true)]);
        assert_eq!(s.apply("well um yeah"), "well  yeah");
    }
}
