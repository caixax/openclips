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

    /// Every catalog holds exactly the strings the sources hand to `tr`:
    /// a string missing from a catalog would show in English, a key nobody
    /// uses is dead weight. Strings must be literals right inside the call
    /// for this to see them.
    #[test]
    fn every_source_string_is_in_every_catalog() {
        let used = source_strings();
        assert!(used.len() > 200, "found only {} strings", used.len());
        for (lang, _) in CATALOGS {
            let map = catalogs().get(lang).expect("catalog");
            let missing: Vec<&String> = used.iter().filter(|s| !map.contains_key(*s)).collect();
            assert!(missing.is_empty(), "{} lacks {missing:#?}", lang.code());
            let unused: Vec<&String> = map.keys().filter(|k| !used.contains(k)).collect();
            assert!(
                unused.is_empty(),
                "{} has unused keys {unused:#?}",
                lang.code()
            );
        }
    }

    fn source_strings() -> Vec<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        collect_files(&root.join("ui"), "slint", &mut files);
        collect_files(&root.join("src"), "rs", &mut files);
        let mut found = Vec::new();
        for file in files {
            // This file names the prefixes it looks for; skip it.
            if file.ends_with("i18n.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("read a source file");
            for prefix in ["I18n.tr(", "i18n::tr("] {
                let mut rest = text.as_str();
                while let Some(at) = rest.find(prefix) {
                    rest = &rest[at + prefix.len()..];
                    if let Some(literal) = string_literal(rest.trim_start()) {
                        found.push(literal);
                    }
                }
            }
        }
        found.sort();
        found.dedup();
        found
    }

    fn collect_files(dir: &std::path::Path, extension: &str, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files(&path, extension, out);
            } else if path.extension().is_some_and(|e| e == extension) {
                out.push(path);
            }
        }
    }

    /// The text of a `"..."` literal at the start of `text`, with the
    /// escapes both Rust and Slint use resolved.
    fn string_literal(text: &str) -> Option<String> {
        let mut chars = text.strip_prefix('"')?.chars();
        let mut out = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    let escaped = chars.next()?;
                    out.push(match escaped {
                        'n' => '\n',
                        other => other,
                    });
                }
                '"' => return Some(out),
                other => out.push(other),
            }
        }
        None
    }
}
