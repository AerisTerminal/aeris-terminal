//! Compiles the translation catalogs under `catalog/<language>/<area>.lang` into typed keys.
//!
//! A catalog line is `key = text`, or `key[category] = text` for a count-dependent message,
//! and `#` starts a comment line. `{name}` is a placeholder; `{{` and `}}` are literal braces,
//! and `\n` and `\\` are the only escapes. English is the reference: every other language must
//! carry exactly its areas and keys, the plural categories its grammar needs, and the same
//! placeholders, or the build fails.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::{self, Write as _},
    fs,
    path::{Path, PathBuf},
};

/// Catalog languages in `Language` order, with the plural categories each must define.
const LANGUAGES: [(&str, &[&str]); 10] = [
    ("en", &["one", "other"]),
    ("de", &["one", "other"]),
    ("es", &["one", "other"]),
    ("fr", &["one", "other"]),
    ("pt", &["one", "other"]),
    ("ru", &["one", "few", "many"]),
    ("hi", &["one", "other"]),
    ("zh", &["other"]),
    ("ja", &["other"]),
    ("ko", &["other"]),
];

/// Every plural category in the generated form order.
const CATEGORIES: [&str; 4] = ["one", "few", "many", "other"];

fn main() {
    println!("cargo:rerun-if-changed=catalog");
    if let Err(error) = compile() {
        panic!("invalid translation catalog: {error}");
    }
}

#[derive(Debug)]
enum Forms {
    Simple(String),
    Plural(BTreeMap<String, String>),
}

#[derive(Debug)]
struct Message {
    key: String,
    forms: Forms,
}

struct Area {
    name: String,
    /// Messages per language, in `LANGUAGES` order; each keeps the English key order.
    languages: Vec<Vec<Message>>,
}

fn compile() -> Result<(), String> {
    let root = PathBuf::from("catalog");
    let reference = area_names(&root.join(LANGUAGES[0].0))?;
    for (code, _) in &LANGUAGES[1..] {
        let names = area_names(&root.join(code))?;
        if names != reference {
            return Err(format!(
                "catalog/{code} must hold exactly the English areas {reference:?}, found {names:?}"
            ));
        }
    }
    let mut areas = Vec::new();
    for name in reference {
        let mut languages = Vec::new();
        for (code, categories) in LANGUAGES {
            let path = root.join(code).join(format!("{name}.lang"));
            let messages = parse_catalog(&path, categories)?;
            languages.push(messages);
        }
        align_with_reference(&name, &mut languages)?;
        areas.push(Area { name, languages });
    }
    let out = PathBuf::from(env::var("OUT_DIR").map_err(|error| error.to_string())?);
    let code = generate(&areas).map_err(|error| error.to_string())?;
    fs::write(out.join("catalog.rs"), code).map_err(|error| error.to_string())
}

fn area_names(directory: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("{} is unreadable: {error}", directory.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        let stem = path.file_stem().and_then(|stem| stem.to_str());
        match (
            stem,
            path.extension().and_then(|extension| extension.to_str()),
        ) {
            (Some(stem), Some("lang")) if is_identifier(stem) => names.push(stem.to_string()),
            _ => {
                return Err(format!(
                    "{} is not a `<snake_case>.lang` catalog",
                    path.display()
                ));
            }
        }
    }
    names.sort();
    Ok(names)
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

fn parse_catalog(path: &Path, categories: &[&str]) -> Result<Vec<Message>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    let mut messages: Vec<Message> = Vec::new();
    for (index, raw) in source.lines().enumerate() {
        let location = format!("{}:{}", path.display(), index + 1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| format!("{location}: expected `key = text`"))?;
        let (name, value) = (name.trim(), unescape(value.trim(), &location)?);
        if value.is_empty() {
            return Err(format!("{location}: `{name}` has no text"));
        }
        placeholders(&value, &location)?;
        let (key, category) = match name.split_once('[') {
            Some((key, rest)) => {
                let category = rest
                    .strip_suffix(']')
                    .ok_or_else(|| format!("{location}: unterminated plural category"))?;
                if !categories.contains(&category) {
                    return Err(format!(
                        "{location}: `{category}` is not one of this language's plural categories {categories:?}"
                    ));
                }
                (key.trim(), Some(category))
            }
            None => (name, None),
        };
        if !is_identifier(key) {
            return Err(format!("{location}: `{key}` is not a snake_case key"));
        }
        let existing = messages.iter_mut().find(|message| message.key == key);
        match (existing, category) {
            (None, None) => messages.push(Message {
                key: key.to_string(),
                forms: Forms::Simple(value),
            }),
            (None, Some(category)) => messages.push(Message {
                key: key.to_string(),
                forms: Forms::Plural(BTreeMap::from([(category.to_string(), value)])),
            }),
            (Some(message), Some(category)) => match &mut message.forms {
                Forms::Plural(forms) if !forms.contains_key(category) => {
                    forms.insert(category.to_string(), value);
                }
                _ => return Err(format!("{location}: `{name}` is defined twice")),
            },
            (Some(_), None) => return Err(format!("{location}: `{key}` is defined twice")),
        }
    }
    for message in &messages {
        if let Forms::Plural(forms) = &message.forms {
            let missing: Vec<_> = categories
                .iter()
                .filter(|category| !forms.contains_key(**category))
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "{}: `{}` is missing plural categories {missing:?}",
                    path.display(),
                    message.key
                ));
            }
        }
    }
    Ok(messages)
}

fn unescape(value: &str, location: &str) -> Result<String, String> {
    let mut text = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            text.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => text.push('\n'),
            Some('\\') => text.push('\\'),
            other => {
                return Err(format!(
                    "{location}: unknown escape `\\{}`",
                    other.unwrap_or(' ')
                ));
            }
        }
    }
    Ok(text)
}

/// The placeholder names in one text; rejects unbalanced braces.
fn placeholders(value: &str, location: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match characters.next() {
                        Some('}') => break,
                        Some(character) => name.push(character),
                        None => return Err(format!("{location}: unterminated placeholder")),
                    }
                }
                if !is_identifier(&name) {
                    return Err(format!(
                        "{location}: `{{{name}}}` is not a snake_case placeholder"
                    ));
                }
                names.insert(name);
            }
            '}' => return Err(format!("{location}: unmatched `}}`; write `}}}}`")),
            _ => {}
        }
    }
    Ok(names)
}

fn form_placeholders(forms: &Forms) -> BTreeSet<String> {
    let texts: Vec<&String> = match forms {
        Forms::Simple(text) => vec![text],
        Forms::Plural(forms) => forms.values().collect(),
    };
    texts
        .into_iter()
        .flat_map(|text| placeholders(text, "").unwrap_or_default())
        .collect()
}

/// Orders every translation like English and checks it carries the same keys and placeholders.
fn align_with_reference(area: &str, languages: &mut [Vec<Message>]) -> Result<(), String> {
    let (reference, translations) = languages
        .split_first_mut()
        .ok_or_else(|| "no reference language".to_string())?;
    for (offset, translation) in translations.iter_mut().enumerate() {
        let code = LANGUAGES[offset + 1].0;
        let mut ordered = Vec::with_capacity(reference.len());
        for english in reference.iter() {
            let position = translation
                .iter()
                .position(|message| message.key == english.key)
                .ok_or_else(|| {
                    format!("catalog/{code}/{area}.lang is missing `{}`", english.key)
                })?;
            let message = translation.swap_remove(position);
            match (&english.forms, &message.forms) {
                (Forms::Simple(_), Forms::Simple(_)) | (Forms::Plural(_), Forms::Plural(_)) => {}
                _ => {
                    return Err(format!(
                        "catalog/{code}/{area}.lang: `{}` must be {} like English",
                        english.key,
                        if matches!(english.forms, Forms::Plural(_)) {
                            "count-dependent"
                        } else {
                            "a single text"
                        }
                    ));
                }
            }
            let (expected, found) = (
                form_placeholders(&english.forms),
                form_placeholders(&message.forms),
            );
            if expected != found {
                return Err(format!(
                    "catalog/{code}/{area}.lang: `{}` uses placeholders {found:?}, English uses {expected:?}",
                    english.key
                ));
            }
            ordered.push(message);
        }
        if let Some(extra) = translation.first() {
            return Err(format!(
                "catalog/{code}/{area}.lang has `{}`, which English lacks",
                extra.key
            ));
        }
        *translation = ordered;
    }
    Ok(())
}

/// The English reference as one-line code span, which keeps product names and line breaks out
/// of rustdoc lints.
fn doc_span(reference: &str) -> String {
    format!("`{}`", reference.replace('\n', " "))
}

fn generate(areas: &[Area]) -> Result<String, fmt::Error> {
    let mut code = String::new();
    let mut texts = String::new();
    let mut plurals = String::new();
    let (mut text_count, mut plural_count) = (0_usize, 0_usize);
    for area in areas {
        let name = &area.name;
        write!(
            code,
            "/// Text keys of the `{name}` catalog.\npub mod {name} {{\n"
        )?;
        for (index, english) in area.languages[0].iter().enumerate() {
            let constant = english.key.to_ascii_uppercase();
            match &english.forms {
                Forms::Simple(reference) => {
                    let doc = doc_span(reference);
                    write!(
                        code,
                        "    #[doc = {doc:?}]\n    pub const {constant}: crate::Text = crate::Text({text_count});\n"
                    )?;
                    texts.push_str("    [");
                    for language in &area.languages {
                        if let Forms::Simple(text) = &language[index].forms {
                            write!(texts, "{text:?}, ")?;
                        }
                    }
                    texts.push_str("],\n");
                    text_count += 1;
                }
                Forms::Plural(reference) => {
                    let summary = reference
                        .values()
                        .map(|form| doc_span(form))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    write!(
                        code,
                        "    #[doc = {summary:?}]\n    pub const {constant}: crate::PluralText = crate::PluralText({plural_count});\n"
                    )?;
                    plurals.push_str("    [");
                    for (language, (_, required)) in area.languages.iter().zip(LANGUAGES) {
                        if let Forms::Plural(forms) = &language[index].forms {
                            // A category the language does not use falls back to its last
                            // required form (`other` in English, `many` in Russian).
                            let fallback = &forms[*required.last().unwrap_or(&"other")];
                            plurals.push('[');
                            for category in CATEGORIES {
                                let form = forms.get(category).unwrap_or(fallback);
                                write!(plurals, "{form:?}, ")?;
                            }
                            plurals.push_str("], ");
                        }
                    }
                    plurals.push_str("],\n");
                    plural_count += 1;
                }
            }
        }
        code.push_str("}\n");
    }
    let languages = LANGUAGES.len();
    write!(
        code,
        "static TEXTS: [[&str; {languages}]; {text_count}] = [\n{texts}];\n"
    )?;
    write!(
        code,
        "static PLURALS: [[[&str; 4]; {languages}]; {plural_count}] = [\n{plurals}];\n"
    )?;
    Ok(code)
}
