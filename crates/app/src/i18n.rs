//! Interface translations.
//!
//! English is the source language: every translatable string in the UI and in
//! user facing messages is written in English and used verbatim as the lookup
//! key. The other languages are JSON catalogs (`i18n/<code>.json`) mapping the
//! English string to its translation; a missing entry falls back to English,
//! so a half translated catalog still works.
//!
//! Both Slint (through the `I18n.tr` global) and Rust code (through [`tr`])
//! translate against the one current language, which the settings page sets.
//! The window is rebuilt when the language changes, so nothing needs to react
//! to it mid frame.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use openclips_core::config::Language;

/// The language every [`tr`] call resolves against.
static CURRENT: RwLock<Language> = RwLock::new(Language::English);

/// Raw catalogs, bundled into the binary.
const CATALOGS: &[(Language, &str)] = &[
    (Language::Spanish, include_str!("../i18n/es.json")),
    (Language::French, include_str!("../i18n/fr.json")),
    (Language::German, include_str!("../i18n/de.json")),
    (Language::Russian, include_str!("../i18n/ru.json")),
    (Language::Portuguese, include_str!("../i18n/pt.json")),
    (Language::Italian, include_str!("../i18n/it.json")),
];

fn catalogs() -> &'static HashMap<Language, HashMap<String, String>> {
    static PARSED: OnceLock<HashMap<Language, HashMap<String, String>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(lang, raw)| {
                let map: HashMap<String, String> = serde_json::from_str(raw).unwrap_or_else(|e| {
                    // A malformed catalog degrades to English rather than
                    // taking the app down.
                    tracing::error!("could not parse the {} translations: {e}", lang.code());
                    HashMap::new()
                });
                (*lang, map)
            })
            .collect()
    })
}

/// Sets the language all later [`tr`] calls resolve against.
pub fn set_language(language: Language) {
    *CURRENT.write().unwrap_or_else(|p| p.into_inner()) = language;
}

/// The current language.
pub fn language() -> Language {
    *CURRENT.read().unwrap_or_else(|p| p.into_inner())
}

/// Translates an English source string into the current language, falling
/// back to the input when the language is English or the string is not in the
/// catalog.
pub fn tr(text: &str) -> String {
    translate(language(), text)
}

/// Translates against a specific language.
pub fn translate(language: Language, text: &str) -> String {
    if language == Language::English {
        return text.to_owned();
    }
    catalogs()
        .get(&language)
        .and_then(|map| map.get(text))
        .cloned()
        .unwrap_or_else(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_is_identity() {
        assert_eq!(translate(Language::English, "Settings"), "Settings");
    }

    #[test]
    fn every_catalog_parses() {
        for (lang, _) in CATALOGS {
            assert!(
                catalogs().contains_key(lang),
                "catalog for {} did not load",
                lang.code()
            );
        }
    }

    #[test]
    fn missing_key_falls_back_to_english() {
        assert_eq!(
            translate(Language::Spanish, "\u{1}not a real key\u{1}"),
            "\u{1}not a real key\u{1}"
        );
    }
}
