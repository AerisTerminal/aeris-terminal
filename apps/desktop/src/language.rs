//! The display-language preference: the user's choice, its resolution against the
//! operating system, and the one place the process display language is set.
//!
//! The choice persists in the workspace document's chrome section (`system` follows the
//! operating system; a language code pins it). Every surface reads translated text through
//! `aeris_localization`, and only this module may call its `set_language`.

use aeris_localization::Language;

/// The user's display-language choice as stored in the workspace document.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LanguagePreference {
    /// Follow the operating system's language, falling back to English.
    #[default]
    System,
    /// A pinned language, whatever the operating system uses.
    Pinned(Language),
}

impl LanguagePreference {
    /// Every choice in picker order.
    pub const ALL: [Self; 3] = [
        Self::System,
        Self::Pinned(Language::English),
        Self::Pinned(Language::Russian),
    ];

    /// The stable identifier stored in the workspace document.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Pinned(language) => language.code(),
        }
    }

    /// Restores a choice by its identifier; an unknown identifier follows the system,
    /// exactly like the other chrome preference identifiers.
    #[must_use]
    pub fn from_identifier(value: &str) -> Self {
        match Language::from_code(value.trim()) {
            Some(language) => Self::Pinned(language),
            None => Self::System,
        }
    }

    /// The language this choice resolves to right now.
    #[must_use]
    pub fn effective(self) -> Language {
        match self {
            Self::System => system_language(),
            Self::Pinned(language) => language,
        }
    }

    /// Applies the choice to the process display language.
    pub fn apply(self) {
        aeris_localization::set_language(self.effective());
    }
}

/// The operating system's display language, or English when it has no catalog.
#[must_use]
pub fn system_language() -> Language {
    sys_locale::get_locale()
        .map(|tag| Language::from_locale_tag(&tag))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{LanguagePreference, system_language};
    use aeris_localization::Language;

    #[test]
    fn identifiers_round_trip_every_choice() {
        for preference in LanguagePreference::ALL {
            assert_eq!(
                LanguagePreference::from_identifier(preference.identifier()),
                preference
            );
        }
        assert_eq!(
            LanguagePreference::from_identifier("klingon"),
            LanguagePreference::System
        );
        assert_eq!(
            LanguagePreference::from_identifier(""),
            LanguagePreference::System
        );
    }

    #[test]
    fn system_language_resolves_to_a_catalog_language() {
        assert!(Language::ALL.contains(&system_language()));
        assert_eq!(
            LanguagePreference::Pinned(Language::Russian).effective(),
            Language::Russian
        );
    }
}
