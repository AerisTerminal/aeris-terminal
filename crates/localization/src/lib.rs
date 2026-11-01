//! Translated user-interface text for every Aeris surface.
//!
//! Each area of the product keeps its text in `catalog/<language>/<area>.lang`; the build
//! turns every key into a typed constant (`aeris_localization::<area>::<KEY>`), so a key that
//! does not exist fails to compile and a translation that misses a key or a placeholder fails
//! the build. Text is looked up in the process's one display language, which only the desktop's
//! language preference owner sets.

use std::fmt::{self, Display, Write as _};
use std::sync::atomic::{AtomicU8, Ordering};

include!(concat!(env!("OUT_DIR"), "/catalog.rs"));

/// A user-interface language with a complete catalog.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Language {
    #[default]
    English,
    Russian,
}

impl Language {
    /// Every language in picker order; the order of the generated catalog columns.
    pub const ALL: [Self; 2] = [Self::English, Self::Russian];

    /// The stable identifier stored in user preferences.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian => "ru",
        }
    }

    /// Looks a language up by its stable identifier.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.code() == code)
    }

    /// The language for an operating-system locale tag such as `ru-RU` or `en_US.UTF-8`;
    /// a language without a catalog reads as English.
    #[must_use]
    pub fn from_locale_tag(tag: &str) -> Self {
        let primary = tag
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        Self::from_code(&primary).unwrap_or_default()
    }

    /// The language's own name, as a language picker shows it.
    #[must_use]
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    const fn from_index(index: u8) -> Self {
        match index {
            1 => Self::Russian,
            _ => Self::English,
        }
    }

    fn plural_category(self, count: u64) -> PluralCategory {
        match self {
            Self::English if count == 1 => PluralCategory::One,
            Self::English => PluralCategory::Other,
            Self::Russian => match (count % 10, count % 100) {
                (1, tens) if tens != 11 => PluralCategory::One,
                (2..=4, tens) if !(12..=14).contains(&tens) => PluralCategory::Few,
                _ => PluralCategory::Many,
            },
        }
    }
}

/// CLDR plural categories in the generated form order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PluralCategory {
    One,
    Few,
    Many,
    Other,
}

static DISPLAY_LANGUAGE: AtomicU8 = AtomicU8::new(0);

/// Sets the process's display language. Windows re-render to pick it up.
pub fn set_language(language: Language) {
    #[allow(clippy::cast_possible_truncation)] // `Language` has two variants.
    DISPLAY_LANGUAGE.store(language.index() as u8, Ordering::Relaxed);
}

/// The process's display language.
#[must_use]
pub fn language() -> Language {
    Language::from_index(DISPLAY_LANGUAGE.load(Ordering::Relaxed))
}

/// One named value substituted for a `{name}` placeholder.
pub type Argument<'a> = (&'a str, &'a dyn Display);

/// A catalog text, resolved in the display language.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Text(u16);

impl Text {
    /// The text in the display language.
    #[must_use]
    pub fn get(self) -> &'static str {
        self.in_language(language())
    }

    /// The text in an explicit language.
    #[must_use]
    pub fn in_language(self, language: Language) -> &'static str {
        TEXTS[usize::from(self.0)][language.index()]
    }

    /// The text in the display language with its placeholders filled in.
    #[must_use]
    pub fn format(self, arguments: &[Argument<'_>]) -> String {
        self.format_in(language(), arguments)
    }

    /// The text in an explicit language with its placeholders filled in.
    #[must_use]
    pub fn format_in(self, language: Language, arguments: &[Argument<'_>]) -> String {
        substitute(self.in_language(language), arguments)
    }
}

/// A count-dependent catalog text (`{count} order`, `{count} orders`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PluralText(u16);

impl PluralText {
    /// The form for `count` in the display language; `{count}` and the other placeholders
    /// are filled in.
    #[must_use]
    pub fn format(self, count: i128, arguments: &[Argument<'_>]) -> String {
        self.format_in(language(), count, arguments)
    }

    /// The form for `count` in an explicit language.
    #[must_use]
    pub fn format_in(self, language: Language, count: i128, arguments: &[Argument<'_>]) -> String {
        let forms = PLURALS[usize::from(self.0)][language.index()];
        let magnitude = u64::try_from(count.unsigned_abs()).unwrap_or(u64::MAX);
        let form = forms[language.plural_category(magnitude) as usize];
        let mut all: Vec<Argument<'_>> = Vec::with_capacity(arguments.len() + 1);
        all.push(("count", &count));
        all.extend_from_slice(arguments);
        substitute(form, &all)
    }
}

/// Widens any primitive integer for `tr_count!`; every Rust integer except an
/// out-of-range `u128` fits, and that one saturates.
#[doc(hidden)]
#[must_use]
pub fn plural_count(count: impl TryInto<i128>) -> i128 {
    count.try_into().unwrap_or(i128::MAX)
}

fn substitute(text: &str, arguments: &[Argument<'_>]) -> String {
    let mut output = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(position) = rest.find(['{', '}']) {
        output.push_str(&rest[..position]);
        let tail = &rest[position..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            output.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let Some(end) = tail.find('}').filter(|_| tail.starts_with('{')) else {
            output.push_str(&tail[..1]);
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..end];
        if let Some((_, value)) = arguments.iter().find(|(argument, _)| *argument == name) {
            write_value(&mut output, *value);
        } else {
            debug_assert!(
                false,
                "no value for the `{{{name}}}` placeholder in {text:?}"
            );
            output.push_str(&tail[..=end]);
        }
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    output
}

fn write_value(output: &mut String, value: &dyn Display) {
    if write!(output, "{value}").is_err() {
        output.push('?');
    }
}

impl Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.get())
    }
}

/// Looks a text up in the display language: `tr!(area::KEY)` is the `&'static str`, and
/// `tr!(area::KEY, name = value, …)` fills the placeholders into a `String`.
#[macro_export]
macro_rules! tr {
    ($key:expr $(,)?) => {
        $crate::Text::get($key)
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::Text::format(
            $key,
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// Looks a count-dependent text up in the display language:
/// `tr_count!(area::KEY, count, name = value, …)`.
#[macro_export]
macro_rules! tr_count {
    ($key:expr, $count:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::PluralText::format(
            $key,
            $crate::plural_count($count),
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),*],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::{Language, common, substitute};

    #[test]
    fn locale_tags_pick_a_catalog_language_or_english() {
        assert_eq!(Language::from_locale_tag("ru-RU"), Language::Russian);
        assert_eq!(Language::from_locale_tag("ru_UA.UTF-8"), Language::Russian);
        assert_eq!(Language::from_locale_tag("RU"), Language::Russian);
        assert_eq!(Language::from_locale_tag("en-GB"), Language::English);
        assert_eq!(Language::from_locale_tag("de-DE"), Language::English);
        assert_eq!(Language::from_locale_tag(""), Language::English);
        for language in Language::ALL {
            assert_eq!(Language::from_code(language.code()), Some(language));
        }
        assert_eq!(Language::from_code("xx"), None);
    }

    #[test]
    fn russian_plural_forms_follow_cldr_integer_rules() {
        let category = |count| Language::Russian.plural_category(count);
        for one in [1, 21, 101, 1001] {
            assert_eq!(category(one) as usize, 0, "{one}");
        }
        for few in [2, 3, 4, 22, 34, 102] {
            assert_eq!(category(few) as usize, 1, "{few}");
        }
        for many in [0, 5, 11, 12, 14, 19, 25, 111, 112] {
            assert_eq!(category(many) as usize, 2, "{many}");
        }
        assert_eq!(Language::English.plural_category(1) as usize, 0);
        assert_eq!(Language::English.plural_category(0) as usize, 3);
        assert_eq!(Language::English.plural_category(2) as usize, 3);
    }

    #[test]
    fn placeholders_take_named_values_and_braces_escape() {
        let quantity = 3;
        assert_eq!(
            substitute(
                "Buy {quantity} at {price}",
                &[("quantity", &quantity), ("price", &"1.5")]
            ),
            "Buy 3 at 1.5"
        );
        assert_eq!(substitute("{{literal}}", &[]), "{literal}");
    }

    #[test]
    fn every_language_resolves_every_text_and_count_form() {
        assert_eq!(common::CANCEL.in_language(Language::English), "Cancel");
        assert_eq!(common::CANCEL.in_language(Language::Russian), "Отмена");
        assert_eq!(common::ITEMS.format_in(Language::English, 1, &[]), "1 item");
        assert_eq!(
            common::ITEMS.format_in(Language::English, 5, &[]),
            "5 items"
        );
        assert_eq!(
            common::ITEMS.format_in(Language::Russian, 1, &[]),
            "1 элемент"
        );
        assert_eq!(
            common::ITEMS.format_in(Language::Russian, 3, &[]),
            "3 элемента"
        );
        assert_eq!(
            common::ITEMS.format_in(Language::Russian, 11, &[]),
            "11 элементов"
        );
    }
}
