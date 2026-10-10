//! Localized native notification copy. Map only fixed, content-free product
//! strings; never translate arbitrary backend errors or user-named devices.
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::native_language::Language;

static MESSAGES: LazyLock<HashMap<String, HashMap<String, String>>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../locales/native_notifications.json"))
        .expect("native notification catalog must be valid JSON")
});

fn translate(language: Language, source: &str) -> String {
    let code = match language {
        Language::En => return source.to_owned(),
        Language::Es => "es",
        Language::Fr => "fr",
        Language::Ru => "ru",
    };
    MESSAGES
        .get(source)
        .and_then(|translations| translations.get(code))
        .cloned()
        .unwrap_or_else(|| source.to_owned())
}

pub fn localized(language: Language, title: &str, body: &str) -> (String, String) {
    // Model names are registry labels, not translated product copy.
    let model_title = if let Some(name) = title.strip_suffix(" failed verification") {
        Some(match language {
            Language::En => title.to_owned(),
            Language::Es => format!("Error al verificar {name}"),
            Language::Fr => format!("Échec de la vérification de {name}"),
            Language::Ru => format!("Не удалось проверить {name}"),
        })
    } else {
        title.strip_suffix(" installed").map(|name| match language {
            Language::En => title.to_owned(),
            Language::Es => format!("{name} instalado"),
            Language::Fr => format!("{name} installé"),
            Language::Ru => format!("Модель {name} установлена"),
        })
    };
    (
        model_title.unwrap_or_else(|| translate(language, title)),
        translate(language, body),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_fixed_notification_has_three_nonempty_translations() {
        for (source, translations) in MESSAGES.iter() {
            assert!(!source.is_empty());
            for language in ["es", "fr", "ru"] {
                assert!(!translations
                    .get(language)
                    .unwrap_or_else(|| panic!("{source}: {language}"))
                    .is_empty());
            }
        }
        assert_eq!(
            localized(
                Language::En,
                "No audio detected",
                "Check that your intended audio input is selected."
            ),
            (
                "No audio detected".into(),
                "Check that your intended audio input is selected.".into()
            )
        );
        assert_eq!(
            localized(
                Language::Ru,
                "Whisper installed",
                "Ready to use — select it in Settings → Speech model."
            )
            .0,
            "Модель Whisper установлена"
        );
    }
}
