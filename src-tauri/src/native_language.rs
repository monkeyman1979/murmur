//! Native UI labels use the App language, never the Whisper hint or meeting profile.
//! The main webview hydrates the persisted preference after the tray starts in English.
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    En = 0,
    Es = 1,
    Fr = 2,
    Ru = 3,
}

impl Language {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Self::En),
            "es" => Some(Self::Es),
            "fr" => Some(Self::Fr),
            "ru" => Some(Self::Ru),
            _ => None,
        }
    }
}

static LANGUAGE: AtomicU8 = AtomicU8::new(Language::En as u8);

pub fn current() -> Language {
    match LANGUAGE.load(Ordering::Relaxed) {
        1 => Language::Es,
        2 => Language::Fr,
        3 => Language::Ru,
        _ => Language::En,
    }
}

pub fn set(language: Language) {
    LANGUAGE.store(language as u8, Ordering::Relaxed);
}

/// Native tray copy only. IDs are stable; user-named audio devices are not translated.
pub fn tray(language: Language, key: &str) -> &'static str {
    match key {
        "automatic" => match language {
            Language::En => "Automatic",
            Language::Es => "Automático",
            Language::Fr => "Automatique",
            Language::Ru => "Автоматически",
        },
        "no_microphones" => match language {
            Language::En => "No microphones available",
            Language::Es => "No hay micrófonos disponibles",
            Language::Fr => "Aucun microphone disponible",
            Language::Ru => "Микрофоны недоступны",
        },
        "microphone_access" => match language {
            Language::En => "Microphone access required",
            Language::Es => "Se necesita acceso al micrófono",
            Language::Fr => "Accès au microphone requis",
            Language::Ru => "Нужен доступ к микрофону",
        },
        "microphone" => match language {
            Language::En => "Microphone",
            Language::Es => "Micrófono",
            Language::Fr => "Microphone",
            Language::Ru => "Микрофон",
        },
        "no_outputs" => match language {
            Language::En => "No outputs available",
            Language::Es => "No hay salidas disponibles",
            Language::Fr => "Aucune sortie disponible",
            Language::Ru => "Аудиовыходы недоступны",
        },
        "output" => match language {
            Language::En => "Output",
            Language::Es => "Salida",
            Language::Fr => "Sortie",
            Language::Ru => "Выход",
        },
        "open" => match language {
            Language::En => "Open Murmur",
            Language::Es => "Abrir Murmur",
            Language::Fr => "Ouvrir Murmur",
            Language::Ru => "Открыть Murmur",
        },
        "record" => match language {
            Language::En => "Record Meeting",
            Language::Es => "Grabar reunión",
            Language::Fr => "Enregistrer une réunion",
            Language::Ru => "Записать встречу",
        },
        "pause" => match language {
            Language::En => "Pause Meeting",
            Language::Es => "Pausar reunión",
            Language::Fr => "Mettre la réunion en pause",
            Language::Ru => "Приостановить запись встречи",
        },
        "resume" => match language {
            Language::En => "Resume Meeting",
            Language::Es => "Reanudar reunión",
            Language::Fr => "Reprendre la réunion",
            Language::Ru => "Возобновить запись встречи",
        },
        "stop" => match language {
            Language::En => "Finish Meeting & Transcribe",
            Language::Es => "Terminar reunión y transcribir",
            Language::Fr => "Terminer la réunion et transcrire",
            Language::Ru => "Завершить встречу и расшифровать",
        },
        "copy_dictation" => match language {
            Language::En => "Copy Last Dictation",
            Language::Es => "Copiar último dictado",
            Language::Fr => "Copier la dernière dictée",
            Language::Ru => "Скопировать последнюю диктовку",
        },
        "copy_transcript" => match language {
            Language::En => "Copy Last Meeting Transcript",
            Language::Es => "Copiar última transcripción de reunión",
            Language::Fr => "Copier la dernière transcription de réunion",
            Language::Ru => "Скопировать последнюю расшифровку встречи",
        },
        "copy_for_ai" => match language {
            Language::En => "Copy Last Meeting Transcript for AI",
            Language::Es => "Copiar última transcripción de reunión para IA",
            Language::Fr => "Copier la dernière transcription de réunion pour l’IA",
            Language::Ru => "Скопировать последнюю расшифровку встречи для ИИ",
        },
        "settings" => match language {
            Language::En => "Settings…",
            Language::Es => "Ajustes…",
            Language::Fr => "Réglages…",
            Language::Ru => "Настройки…",
        },
        "quit" => match language {
            Language::En => "Quit Murmur",
            Language::Es => "Salir de Murmur",
            Language::Fr => "Quitter Murmur",
            Language::Ru => "Выйти из Murmur",
        },
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tray_labels_exist_in_every_locale_and_are_not_key_fallbacks() {
        for key in [
            "automatic",
            "no_microphones",
            "microphone_access",
            "microphone",
            "no_outputs",
            "output",
            "open",
            "record",
            "pause",
            "resume",
            "stop",
            "copy_dictation",
            "copy_transcript",
            "copy_for_ai",
            "settings",
            "quit",
        ] {
            for language in [Language::En, Language::Es, Language::Fr, Language::Ru] {
                assert!(!tray(language, key).trim().is_empty(), "{language:?}/{key}");
            }
        }
        assert_eq!(Language::parse("xx"), None);
    }
}
