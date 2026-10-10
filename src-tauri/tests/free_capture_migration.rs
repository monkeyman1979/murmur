//! Source-level upgrade fixtures. Installed-app/TCC and real prior-release
//! migrations still require manual QA on the exact signed candidate.
use murmur_lib::history::{TranscriptRecord, TranscriptStore};
use std::path::PathBuf;

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "murmur-free-capture-upgrade-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        Self(std::fs::canonicalize(root).unwrap())
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn existing_dictations_and_legacy_trial_bytes_coexist_after_free_upgrade() {
    for legacy in [
        None,
        Some(r#"{"schema_version":2,"state":"active","words_used":0}"#),
        Some(r#"{"schema_version":2,"state":"expired","words_used":2000}"#),
        Some(r#"{"schema_version":2,"state":"purchased","license_token":"obsolete-test-token"}"#),
        Some("{corrupt record that must remain untouched"),
        Some(r#"{"schema_version":999,"future_field":"keep me"}"#),
    ] {
        let root = FixtureDir::new();
        let db = root.0.join("murmur.db");
        let trial_path = root.0.join("trial.json");
        if let Some(bytes) = legacy {
            std::fs::write(&trial_path, bytes).unwrap();
        }
        let store = TranscriptStore::open(&db).unwrap();
        store
            .insert(&TranscriptRecord::new_now(
                "prior raw words".into(),
                "prior corrected words".into(),
                1_000,
                None,
                None,
            ))
            .unwrap();
        drop(store);

        // Free builds use the same durable history path and have no dependency
        // on trial.json when reopening old dictations or saving new ones.
        let store = TranscriptStore::open(&db).unwrap();
        assert_eq!(
            store.list(10, 0).unwrap()[0].text_final,
            "prior corrected words"
        );
        store
            .insert(&TranscriptRecord::new_now(
                "new free dictation".into(),
                "new free dictation".into(),
                1_000,
                None,
                None,
            ))
            .unwrap();
        assert_eq!(store.list(10, 0).unwrap().len(), 2);
        drop(store);

        match legacy {
            Some(bytes) => assert_eq!(std::fs::read(&trial_path).unwrap(), bytes.as_bytes()),
            None => assert!(!trial_path.exists()),
        }
    }
}
