use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Trees whose names are not ours to choose: build output, vendored dependencies, third-party
/// icon and font assets shipped under their upstream names, and gitignored runtime data.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "__pycache__",
    "assets",
    "local-data",
    "node_modules",
    "target",
    "third_party",
    "tmp",
];
const REPOSITORY_MARKDOWN_FILES: &[&str] = &["AGENTS.md", "CSS.md"];

fn has_process_global_desktop_trading(source: &str) -> bool {
    let production = production_source_prefix(source);
    [
        "static TRADING_SERVICE",
        "trading::handle()",
        "trading::install(",
        "static TRADING_FEEDBACK",
        "trading::latest_feedback()",
    ]
    .iter()
    .any(|forbidden| production.contains(forbidden))
}

fn has_process_global_desktop_account(source: &str) -> bool {
    let production = production_source_prefix(source);
    [
        "DesktopAccount::shared(",
        "static INSTALLED_ACCOUNT",
        "static SERVICE: OnceLock<AccountService>",
        "account::begin_refresh_quiesce(",
    ]
    .iter()
    .any(|forbidden| production.contains(forbidden))
}

fn has_process_global_desktop_market(source: &str) -> bool {
    let production = production_source_prefix(source);
    [
        "static MARKET_RUNTIME",
        "shared_market_runtime(",
        "started_market_runtime(",
        "static MARKET_SERVICE",
    ]
    .iter()
    .any(|forbidden| production.contains(forbidden))
}
const MARKET_CHARACTERIZATION_TESTS: &[&str] = &[
    "characterization_reseed_during_live_flow_keeps_one_forming_bar_and_contiguous_history",
    "characterization_outbox_overflow_emits_covering_snapshot",
    "characterization_stale_generation_history_completion_is_ignored",
    "characterization_symbol_or_timeframe_change_keeps_provider_session",
];
const MARKET_REPLAY_TESTS: &[&str] = &[
    "replay_day3_round_trip",
    "replay_stall_while_open",
    "replay_stall_while_closed",
    "replay_cme_roll_across_dst",
    "replay_non_cme_calendar_resolution",
    "replay_backoff_schedules",
    "replay_stale_epoch_completion",
    "replay_overflow_covering_snapshot",
    "replay_reseed_during_live_flow",
    "replay_switch_storm",
    "replay_gap_recovery",
    "replay_history_exhaustion",
    "replay_superseded_generations",
    "replay_quote_lease_share_release",
    "replay_sanitized_public_fixture",
];
const PLATFORM_FILE_EXCEPTIONS: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "Dockerfile",
    "Readme",
    "rust-toolchain.toml",
];
/// Legal and OS-owned filenames must keep their exact spelling, but only at these paths.
const PLATFORM_PATH_EXCEPTIONS: &[&str] = &["LICENSE", "apps/desktop/packaging/macos/Info.plist"];
const REVIEWED_SAFE_DEBUG_TYPES: &[(&str, &str)] = &[
    (
        "ColorToken",
        "design-system color value, never an authentication token",
    ),
    (
        "CredentialSource",
        "evidence enum identifying the native vault, not its contents",
    ),
    (
        "KeyringCredentialBackend",
        "unit backend marker with no stored secrets",
    ),
    (
        "NativeCredentialVaultError",
        "vault operation error classes, not secret values",
    ),
    (
        "ProviderCredentialRequirement",
        "provider policy enum, not credential material",
    ),
    (
        "RadiusToken",
        "design-system radius value, never an authentication token",
    ),
    (
        "RithmicCredentialError",
        "credential validation error classes, not input values",
    ),
    (
        "SpotToken",
        "public spot-asset metadata, not authentication material",
    ),
    (
        "VerifiedTokens",
        "its secret-bearing fields are SecretToken values with explicit redacting Debug",
    ),
];

fn main() -> ExitCode {
    let root = match env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("failed to resolve repository root: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut violations = Vec::new();
    if let Err(error) = inspect_directory(&root, &root, &mut violations) {
        eprintln!("failed to inspect repository names: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = inspect_market_test_names(&root, &mut violations) {
        eprintln!("failed to read market test names: {error}");
        return ExitCode::FAILURE;
    }
    for path in [
        "crates/market_runtime/src/market_service/calendar/mod.rs",
        "crates/market_runtime/src/market_service/mod.rs",
        "crates/market_runtime/src/market_service/realtime.rs",
        "crates/market_runtime/src/market_service/history.rs",
        "crates/market_runtime/src/market_service/coordinator.rs",
        "crates/market_runtime/src/market_service/runtime.rs",
    ] {
        let file = root.join(path);
        match fs::read_to_string(&file) {
            Ok(source) if violates_calendar_boundary(path, &source) => {
                violations.push(relative_to(&root, &file));
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("failed to read calendar boundary at {path}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    match generic_market_provider_violations(&root) {
        Ok(found) => violations.extend(found),
        Err(error) => {
            eprintln!("failed to inspect generic market provider boundary: {error}");
            return ExitCode::FAILURE;
        }
    }
    if let Err(error) =
        inspect_desktop_boundaries(&root, &root.join("apps/desktop/src"), &mut violations)
    {
        eprintln!("failed to inspect desktop quote boundary: {error}");
        return ExitCode::FAILURE;
    }
    for path in [
        "crates/market_runtime/src/market_service/coordinator.rs",
        "crates/market_runtime/src/market_service/realtime.rs",
        "crates/market_runtime/src/market_service/lifecycle/mod.rs",
        "crates/market_runtime/src/market_service/lifecycle/handlers.rs",
    ] {
        let file = root.join(path);
        match fs::read_to_string(&file) {
            Ok(source) if violates_series_lifecycle_boundary(path, &source) => {
                violations.push(relative_to(&root, &file));
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("failed to read series lifecycle boundary at {path}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let event_path = "crates/market_runtime/src/market_service/provider_event.rs";
    let event_file = root.join(event_path);
    match fs::read_to_string(&event_file) {
        Ok(source) if public_provider_event_has_raw_generation(&source) => {
            violations.push(relative_to(&root, &event_file));
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("failed to read provider event boundary: {error}");
            return ExitCode::FAILURE;
        }
    }
    if let Err(error) = inspect_consumer_delivery(&root, &mut violations) {
        eprintln!("failed to inspect consumer event fencing boundary: {error}");
        return ExitCode::FAILURE;
    }

    if violations.is_empty() {
        println!("snake_case naming check passed");
        return ExitCode::SUCCESS;
    }

    eprintln!("repository naming and source-policy violations:");
    for violation in violations {
        eprintln!("  {}", violation.display());
    }
    ExitCode::FAILURE
}

fn generic_market_provider_violations(root: &Path) -> io::Result<Vec<PathBuf>> {
    let generic_root = root.join("crates/market_runtime/src/market_service");
    let mut files = [
        "calendar/mod.rs",
        "coordinator.rs",
        "realtime.rs",
        "runtime.rs",
        "publication.rs",
    ]
    .into_iter()
    .map(|name| generic_root.join(name))
    .collect::<Vec<_>>();
    for folder in ["lifecycle", "history"] {
        for entry in fs::read_dir(generic_root.join(folder))? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    for name in ["quotes.rs", "outbox.rs"] {
        let path = generic_root.join(name);
        if path.exists() {
            files.push(path);
        }
    }
    let mut violations = Vec::new();
    for file in files {
        let relative = relative_to(root, &file);
        let path = relative.to_string_lossy().replace('\\', "/");
        if violates_generic_market_provider_boundary(&path, &fs::read_to_string(&file)?) {
            violations.push(relative);
        }
    }
    Ok(violations)
}

fn violates_desktop_quote_boundary(path: &str, source: &str) -> bool {
    path.starts_with("apps/desktop/src/")
        && production_source_prefix(source).contains("SimulatedMarketObservation")
}

// The fixture is available to the diagnostic executable and tests, never to the default
// desktop. The only diagnostic caller is the explicitly gated conformance command.
fn violates_fixture_market_boundary(path: &str, source: &str) -> bool {
    path.starts_with("apps/desktop/src/")
        && !path.starts_with("apps/desktop/src/desktop/test_support/")
        && path != "apps/desktop/src/readiness_conformance.rs"
        && production_source_prefix(source).contains("FixtureMarketWorker")
}

fn inspect_desktop_boundaries(
    root: &Path,
    directory: &Path,
    violations: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            inspect_desktop_boundaries(root, &path, violations)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let relative = relative_to(root, &path);
            let relative_path = relative.to_string_lossy().replace('\\', "/");
            let source = fs::read_to_string(&path)?;
            if violates_desktop_quote_boundary(&relative_path, &source)
                || violates_fixture_market_boundary(&relative_path, &source)
                || has_process_global_desktop_trading(&source)
                || has_process_global_desktop_account(&source)
                || has_process_global_desktop_market(&source)
            {
                violations.push(relative);
            }
        }
    }
    Ok(())
}

fn inspect_market_test_names(root: &Path, violations: &mut Vec<PathBuf>) -> io::Result<()> {
    for (path, missing) in [
        (
            "crates/market_runtime/src/market_service/tests/characterization.rs",
            missing_market_characterization_tests as fn(&str) -> Vec<&'static str>,
        ),
        (
            "crates/market_runtime/src/market_service/replay/mod.rs",
            missing_market_replay_tests as fn(&str) -> Vec<&'static str>,
        ),
    ] {
        let file = root.join(path);
        if !missing(&fs::read_to_string(&file)?).is_empty() {
            violations.push(relative_to(root, &file));
        }
    }
    Ok(())
}

fn missing_market_characterization_tests(source: &str) -> Vec<&'static str> {
    MARKET_CHARACTERIZATION_TESTS
        .iter()
        .copied()
        .filter(|name| {
            !source
                .lines()
                .any(|line| line.trim() == format!("fn {name}() {{"))
        })
        .collect()
}

fn missing_market_replay_tests(source: &str) -> Vec<&'static str> {
    MARKET_REPLAY_TESTS
        .iter()
        .copied()
        .filter(|name| {
            !source
                .lines()
                .any(|line| line.trim() == format!("fn {name}() {{"))
        })
        .collect()
}

fn violates_calendar_boundary(path: &str, source: &str) -> bool {
    let production = production_source_prefix(source);
    if path.ends_with("/calendar/mod.rs")
        && [
            "\"rithmic\"",
            "\"tastytrade\"",
            "\"ctrader\"",
            "\"hyperliquid\"",
        ]
        .iter()
        .any(|literal| production.contains(literal))
    {
        return true;
    }
    production.contains("RithmicExchangeCalendar")
}
fn violates_generic_market_provider_boundary(path: &str, source: &str) -> bool {
    let generic = [
        "/lifecycle/",
        "/history/",
        "/calendar/mod.rs",
        "/quotes.rs",
        "/outbox.rs",
        "/coordinator.rs",
        "/realtime.rs",
        "/runtime.rs",
        "/publication.rs",
    ];
    if !path.contains("/market_service/") || !generic.iter().any(|suffix| path.contains(suffix)) {
        return false;
    }
    let production = production_source_prefix(source).to_ascii_lowercase();
    production.split('"').skip(1).step_by(2).any(|literal| {
        ["rithmic", "hyperliquid", "tastytrade", "ctrader"]
            .iter()
            .any(|name| literal.contains(name))
    })
}

fn violates_series_lifecycle_boundary(path: &str, source: &str) -> bool {
    let production = production_source_prefix(source);
    if production.contains("SeriesLiveMap") {
        return true;
    }
    if path.ends_with("/coordinator.rs")
        && !production.contains("series: lifecycle::SeriesLifecycles")
    {
        return true;
    }
    if path.contains("/lifecycle/") && production.contains("Instant::now()") {
        return true;
    }
    if path.ends_with("/realtime.rs")
        && [
            "candle_provider_",
            "candle_series_recovering",
            "publish_candle_live",
            "CandleLiveHandoff",
        ]
        .iter()
        .any(|old| production.contains(old))
    {
        return true;
    }
    false
}

fn contains_retired_series_map(source: &str) -> bool {
    production_source_prefix(source).contains("SeriesLiveMap")
}

fn public_provider_event_has_raw_generation(source: &str) -> bool {
    let production = production_source_prefix(source);
    let Some(event) = production.split("struct ProviderEvent {").nth(1) else {
        return true;
    };
    let Some(fields) = event.split('}').next() else {
        return true;
    };
    let fields: Vec<_> = fields
        .split([',', '\n'])
        .map(|field| field.trim().trim_start_matches("pub(super) ").trim())
        .collect();
    fields.contains(&"generation: u64") || !fields.contains(&"generation: Epoch<ProviderTag>")
}

fn consumer_delivery_is_unfenced(consumer: &str, runtime: &str, commands: &str) -> bool {
    let consumer = production_source_prefix(consumer);
    let runtime = production_source_prefix(runtime);
    let commands = production_source_prefix(commands);
    !consumer.contains("pub type MarketConsumerEvent =")
        || !consumer.contains("Fenced<aeris_market_engine::fence::ConsumerTag, MarketRuntimeEvent>")
        || !runtime.contains("Result<Option<crate::MarketConsumerEvent>, String>")
        || !runtime.contains("Result<Vec<(u64, crate::MarketConsumerEvent)>, String>")
        || !commands.contains("Reply<Option<crate::MarketConsumerEvent>>")
        || !commands.contains("Reply<Vec<(u64, crate::MarketConsumerEvent)>>")
        || runtime.contains("Result<Option<MarketRuntimeEvent>, String>")
        || runtime.contains("Result<Vec<(u64, MarketRuntimeEvent)>, String>")
}

fn inspect_consumer_delivery(root: &Path, violations: &mut Vec<PathBuf>) -> io::Result<()> {
    let consumer_file = root.join("crates/market_runtime/src/lib.rs");
    let consumer = fs::read_to_string(&consumer_file)?;
    let runtime =
        fs::read_to_string(root.join("crates/market_runtime/src/market_service/runtime.rs"))?;
    let commands =
        fs::read_to_string(root.join("crates/market_runtime/src/market_service/mod.rs"))?;
    if consumer_delivery_is_unfenced(&consumer, &runtime, &commands) {
        violations.push(relative_to(root, &consumer_file));
    }
    Ok(())
}

fn inspect_directory(
    root: &Path,
    directory: &Path,
    violations: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let path = entry.path();

        if file_type.is_dir() {
            if should_skip_directory(&name) {
                continue;
            }
            if !is_snake_case(&name.to_string_lossy()) {
                violations.push(relative_to(root, &path));
            }
            inspect_directory(root, &path, violations)?;
        } else if file_type.is_file() {
            let relative = relative_to(root, &path);
            if !is_valid_file_name(&name, &relative) {
                violations.push(relative.clone());
            }
            if path.extension().is_some_and(|extension| extension == "rs")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "src")
                && let Ok(source) = fs::read_to_string(&path)
            {
                let in_market_service = relative
                    .to_string_lossy()
                    .replace('\\', "/")
                    .starts_with("crates/market_runtime/src/market_service/");
                if contains_sensitive_debug(&production_source_prefix(&source))
                    || (in_market_service && contains_retired_series_map(&source))
                {
                    violations.push(relative);
                }
            }
        }
    }
    Ok(())
}

fn production_source_prefix(contents: &str) -> String {
    if contents.starts_with("#![cfg(test)]") {
        return String::new();
    }
    let mut production = String::with_capacity(contents.len());
    let mut cursor = 0;
    while let Some(relative_index) = contents[cursor..].find("#[cfg(test)]") {
        let index = cursor + relative_index;
        let after_attribute = &contents[index + "#[cfg(test)]".len()..];
        let whitespace = after_attribute.len() - after_attribute.trim_start().len();
        let declaration_start = index + "#[cfg(test)]".len() + whitespace;
        let declaration = &contents[declaration_start..];
        if declaration.starts_with("mod ")
            && let Some(open_relative) = declaration.find('{')
            && declaration
                .find(';')
                .is_none_or(|semi| open_relative < semi)
        {
            let open = declaration_start + open_relative;
            if let Some(end) = source_block_end(contents, open) {
                production.push_str(&contents[cursor..index]);
                cursor = end;
                continue;
            }
        }
        production.push_str(&contents[cursor..index + "#[cfg(test)]".len()]);
        cursor = index + "#[cfg(test)]".len();
    }
    production.push_str(&contents[cursor..]);
    production
}

fn source_block_end(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0_u32;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                let mut comment_depth = 1_u32;
                while index < bytes.len() && comment_depth > 0 {
                    if bytes[index..].starts_with(b"/*") {
                        comment_depth += 1;
                        index += 2;
                    } else if bytes[index..].starts_with(b"*/") {
                        comment_depth -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
            }
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2);
                    } else {
                        let quote = bytes[index] == b'"';
                        index += 1;
                        if quote {
                            break;
                        }
                    }
                }
            }
            b'\'' => {
                let character_end = if bytes.get(index + 1) == Some(&b'\\') {
                    index.saturating_add(3)
                } else {
                    source[index + 1..]
                        .chars()
                        .next()
                        .map_or(index + 1, |character| index + 1 + character.len_utf8())
                };
                if bytes.get(character_end) == Some(&b'\'') {
                    index = character_end + 1;
                } else {
                    index += 1;
                }
            }
            b'r' if bytes.get(index + 1) == Some(&b'"') || bytes.get(index + 1) == Some(&b'#') => {
                let mut marker_end = index + 1;
                while bytes.get(marker_end) == Some(&b'#') {
                    marker_end += 1;
                }
                if bytes.get(marker_end) == Some(&b'"') {
                    let hashes = marker_end - index - 1;
                    index = marker_end + 1;
                    while index < bytes.len() {
                        if bytes[index] == b'"'
                            && bytes
                                .get(index + 1..index + 1 + hashes)
                                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
                        {
                            index += 1 + hashes;
                            break;
                        }
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

fn contains_sensitive_debug(source: &str) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        if let Some(name) = derived_debug_type_name(&lines[index..])
            && is_sensitive_name(name)
            && !REVIEWED_SAFE_DEBUG_TYPES
                .iter()
                .any(|(reviewed, reason)| *reviewed == name && !reason.trim().is_empty())
        {
            return true;
        }
        let lower = line.to_ascii_lowercase();
        if (lower.contains(":?}") || lower.contains(":#?}"))
            && ["token", "secret", "password", "credential"]
                .iter()
                .any(|word| debug_capture_contains(&lower, word))
        {
            return true;
        }
    }
    false
}

fn derived_debug_type_name<'a>(lines: &[&'a str]) -> Option<&'a str> {
    let line = lines.first()?;
    (line.contains("derive(") && line.contains("Debug"))
        .then(|| {
            lines
                .iter()
                .skip(1)
                .take(6)
                .find_map(|line| declared_type_name(line))
        })
        .flatten()
}

fn declared_type_name(line: &str) -> Option<&str> {
    let line = line.trim();
    ["struct ", "enum ", "type "]
        .into_iter()
        .find_map(|marker| {
            let declaration = line
                .strip_prefix(marker)
                .or_else(|| {
                    line.strip_prefix("pub ")
                        .and_then(|line| line.strip_prefix(marker))
                })
                .or_else(|| {
                    line.strip_prefix("pub(crate) ")
                        .and_then(|line| line.strip_prefix(marker))
                })?;
            declaration
                .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .next()
                .filter(|name| !name.is_empty())
        })
}

fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["token", "secret", "password", "credential"]
        .iter()
        .any(|word| lower.contains(word))
}

fn debug_capture_contains(line: &str, word: &str) -> bool {
    line.split('{').skip(1).any(|capture| {
        capture
            .split('}')
            .next()
            .is_some_and(|capture| capture.contains(":?") || capture.contains(":#?"))
            && capture
                .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .any(|identifier| identifier.contains(word))
    })
}

fn should_skip_directory(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || SKIPPED_DIRECTORIES.contains(&name.as_ref())
}

fn is_valid_file_name(name: &OsStr, relative: &Path) -> bool {
    let name = name.to_string_lossy();
    if name.starts_with('.')
        || PLATFORM_FILE_EXCEPTIONS.contains(&name.as_ref())
        || REPOSITORY_MARKDOWN_FILES.contains(&name.as_ref())
        || PLATFORM_PATH_EXCEPTIONS
            .iter()
            .any(|exception| relative == Path::new(exception))
    {
        return true;
    }

    Path::new(name.as_ref())
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(is_snake_case)
}

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('_')
        && !name.ends_with('_')
        && !name.contains("__")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::production_source_prefix as production_prefix;

    #[test]
    fn public_provider_event_requires_typed_epoch_even_after_test_module() {
        assert!(super::public_provider_event_has_raw_generation(
            "pub(super) struct ProviderEvent { generation: u64 }\n"
        ));
        assert!(!super::public_provider_event_has_raw_generation(
            "#[cfg(test)] mod tests { struct ProviderEvent { generation: u64 } }\n\
             pub(super) struct ProviderEvent { generation: Epoch<ProviderTag>, kind: EventKind }"
        ));
        assert!(super::public_provider_event_has_raw_generation(
            "#[cfg(test)] mod tests { struct ProviderEvent { generation: Epoch<ProviderTag> } }\n\
             pub(super) struct ProviderEvent { generation: u64 }"
        ));
    }

    #[test]
    fn consumer_events_require_one_typed_envelope_even_after_a_test_module() {
        let runtime = "fn drain() -> Result<Option<crate::MarketConsumerEvent>, String> { todo!() }\n\
            fn drain_client() -> Result<Vec<(u64, crate::MarketConsumerEvent)>, String> { todo!() }";
        let commands = "Drain(Reply<Option<crate::MarketConsumerEvent>>),\n\
            DrainClient(Reply<Vec<(u64, crate::MarketConsumerEvent)>>)";
        let fenced = "#[cfg(test)] mod tests { pub type MarketConsumerEvent = MarketRuntimeEvent; }\n\
            pub type MarketConsumerEvent =\n\
            aeris_market_engine::fence::Fenced<aeris_market_engine::fence::ConsumerTag, MarketRuntimeEvent>;";
        assert!(!super::consumer_delivery_is_unfenced(
            fenced, runtime, commands
        ));
        assert!(super::consumer_delivery_is_unfenced(
            "#[cfg(test)] mod tests { pub type MarketConsumerEvent = Fenced<aeris_market_engine::fence::ConsumerTag, MarketRuntimeEvent>; }\n\
             pub type MarketConsumerEvent = MarketRuntimeEvent;",
            runtime,
            commands
        ));
        assert!(super::consumer_delivery_is_unfenced(
            fenced,
            "fn drain() -> Result<Option<MarketRuntimeEvent>, String> { todo!() }",
            commands
        ));
    }
    use std::{
        collections::BTreeSet,
        fs,
        path::{Path, PathBuf},
    };

    #[test]
    fn calendar_boundary_rejects_provider_literals_and_adapter_calendar_imports() {
        let calendar = "crates/market_runtime/src/market_service/calendar/mod.rs";
        let generic = "crates/market_runtime/src/market_service/realtime.rs";
        assert!(super::violates_calendar_boundary(
            calendar,
            "fn source() { let _ = \"rithmic\"; }"
        ));
        assert!(super::violates_calendar_boundary(
            generic,
            "use aeris_rithmic_protocol_adapter::RithmicExchangeCalendar;"
        ));
        assert!(super::violates_calendar_boundary(
            calendar,
            "#[cfg(test)] mod tests { fn fixture() { let _ = \"rithmic\"; } }\nfn source() { let _ = \"tastytrade\"; }"
        ));
    }

    #[test]
    fn calendar_boundary_accepts_provider_neutral_production_after_tests() {
        let calendar = "crates/market_runtime/src/market_service/calendar/mod.rs";
        assert!(!super::violates_calendar_boundary(
            calendar,
            "#[cfg(test)] mod tests { fn fixture() { let _ = \"rithmic\"; } }\nfn source() { let _ = \"CME\"; }"
        ));
    }
    #[test]
    fn generic_market_files_reject_provider_literals_even_after_test_modules() {
        for path in [
            "lifecycle/mod.rs",
            "lifecycle/handlers.rs",
            "history/mod.rs",
            "history/tracker.rs",
            "calendar/mod.rs",
            "quotes.rs",
            "outbox.rs",
            "coordinator.rs",
            "realtime.rs",
            "runtime.rs",
            "publication.rs",
        ] {
            let path = format!("crates/market_runtime/src/market_service/{path}");
            for provider in ["rithmic", "hyperliquid", "tastytrade", "ctrader"] {
                let fixture = format!(
                    "#[cfg(test)] mod tests {{ fn fixture() {{ let _ = \"{provider}\"; }} }}\nfn production() {{ let _ = \"{provider}\"; }}"
                );
                assert!(
                    super::violates_generic_market_provider_boundary(&path, &fixture),
                    "{path}: {provider}"
                );
                let wording = format!("fn production() {{ let _ = \"Aeris {provider} worker\"; }}");
                assert!(
                    super::violates_generic_market_provider_boundary(&path, &wording),
                    "{path}: embedded {provider}"
                );
            }
            assert!(
                !super::violates_generic_market_provider_boundary(
                    &path,
                    "#[cfg(test)] mod tests { fn fixture() { let _ = \"rithmic\"; } }\nfn production() { let _ = \"CME\"; }"
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn sensitive_debug_rule_rejects_derived_and_formatted_secrets() {
        assert!(super::contains_sensitive_debug(
            "#[derive(Clone, Debug)]\nstruct ApiCredential { value: String }\n"
        ));
        assert!(super::contains_sensitive_debug(
            "fn leak(password: &str) { println!(\"{password:?}\"); }\n"
        ));
    }

    #[test]
    fn sensitive_debug_rule_accepts_reviewed_redaction_and_plain_values() {
        assert!(!super::contains_sensitive_debug(
            "#[derive(Debug)]\nstruct VerifiedTokens { access: SecretToken }\n"
        ));
        assert!(!super::contains_sensitive_debug(
            "#[derive(Debug)]\nstruct MarketGeneration(u64);\n"
        ));
    }

    #[test]
    fn required_platform_filenames_are_scoped_to_their_real_paths() {
        use super::is_valid_file_name;

        assert!(is_valid_file_name(
            std::ffi::OsStr::new("Info.plist"),
            Path::new("apps/desktop/packaging/macos/Info.plist")
        ));
        assert!(is_valid_file_name(
            std::ffi::OsStr::new("LICENSE"),
            Path::new("LICENSE")
        ));
        assert!(!is_valid_file_name(
            std::ffi::OsStr::new("Info.plist"),
            Path::new("docs/Info.plist")
        ));
        assert!(!is_valid_file_name(
            std::ffi::OsStr::new("LICENSE"),
            Path::new("docs/LICENSE")
        ));
    }

    fn repository_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("naming check remains under tools/naming_check")
    }

    fn manifest(relative: &str) -> String {
        fs::read_to_string(repository_root().join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
    }

    fn collect_files(directory: &Path, matches: fn(&Path) -> bool, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        {
            let entry = entry.expect("repository directory entry is readable");
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| {
                    name == "provider_kit" || super::should_skip_directory(name)
                }) {
                    continue;
                }
                collect_files(&path, matches, files);
            } else if matches(&path) {
                files.push(path);
            }
        }
    }

    fn workspace_manifests() -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_files(
            repository_root(),
            |path| path.file_name().is_some_and(|name| name == "Cargo.toml"),
            &mut files,
        );
        files
    }

    fn production_rust_sources() -> Vec<PathBuf> {
        let mut files = Vec::new();
        for root in ["apps", "crates"] {
            collect_files(
                &repository_root().join(root),
                |path| path.extension().is_some_and(|extension| extension == "rs"),
                &mut files,
            );
        }
        files
    }

    fn production_sources_under(relative: &str) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_files(
            &repository_root().join(relative),
            |path| path.extension().is_some_and(|extension| extension == "rs"),
            &mut files,
        );
        files
    }

    fn relative_string(path: &Path) -> String {
        path.strip_prefix(repository_root())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    #[test]
    fn production_prefix_removes_only_balanced_test_modules() {
        let source = "fn before() {}\n#[cfg(test)]\nmod tests {\nmod nested { fn hidden() {} }\n}\nfn after() { forbidden(); }\n";
        let production = super::production_source_prefix(source);
        assert!(production.contains("fn before()"));
        assert!(production.contains("fn after() { forbidden(); }"));
        assert!(!production.contains("fn hidden()"));
    }

    #[test]
    fn retired_series_map_is_rejected_in_production_but_not_test_fixtures() {
        assert!(!super::contains_retired_series_map(
            "struct SeriesLifecycles;\n#[cfg(test)] mod tests { fn fixture() { let _ = \"SeriesLiveMap\"; } }\n"
        ));
        assert!(super::contains_retired_series_map(
            "struct SeriesLifecycles;\n#[cfg(test)] mod tests { fn fixture() {} }\nstruct SeriesLiveMap;\n"
        ));
        assert!(super::violates_series_lifecycle_boundary(
            "crates/market_runtime/src/market_service/lifecycle/mod.rs",
            "struct SeriesLiveMap;"
        ));
    }

    #[test]
    fn out_of_line_test_module_does_not_hide_following_production_rules() {
        let source = "#[cfg(test)]\nmod tests;\nfn after() { unsafe { fs::write(path, bytes); } SeriesCadence::FixedSeconds(60); AccountId; }\n#[derive(Debug)]\nstruct ApiCredential { value: String }\n";
        let production = super::production_source_prefix(source);
        assert!(
            production.contains("fn after() {"),
            "production after mod tests; was lost"
        );
        assert!(contains_unsafe_code(&production));
        assert!(production.contains("fs::write("));
        assert!(production.contains("SeriesCadence::"));
        assert!(contains_identifier(&production, "AccountId"));
        assert!(super::contains_sensitive_debug(&production));
    }

    #[test]
    fn inline_test_module_hides_only_its_body_and_scans_following_production() {
        let source = "#[cfg(test)]\nmod tests {\n fn hidden() { unsafe { fs::write(path, bytes); } }\n}\nfn after() { unsafe { fs::write(path, bytes); } }\n";
        let production = super::production_source_prefix(source);
        assert!(!production.contains("fn hidden()"));
        assert!(production.contains("fn after() {"));
        assert!(contains_unsafe_code(&production));
        assert!(production.contains("fs::write("));
    }

    fn contains_identifier(source: &str, identifier: &str) -> bool {
        source
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .any(|token| token == identifier)
    }

    #[test]
    fn account_boundary_matches_complete_identifiers() {
        assert!(contains_identifier("let account: AccountId", "AccountId"));
        assert!(!contains_identifier(
            "aeris_trading::TradingAccountId",
            "AccountId"
        ));
        assert!(!contains_identifier("AccountIdentity", "AccountId"));
    }

    fn declared_trait_name(line: &str) -> Option<&str> {
        let line = line.trim();
        let declaration = line
            .strip_prefix("trait ")
            .or_else(|| line.strip_prefix("pub trait "))
            .or_else(|| line.strip_prefix("pub(crate) trait "))?;
        declaration
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .next()
            .filter(|name| !name.is_empty())
    }

    fn assert_excludes(relative: &str, forbidden: &[&str]) {
        let contents = manifest(relative);
        for line in contents.lines() {
            let declared = line
                .trim()
                .strip_prefix("[dependencies.")
                .unwrap_or(line.trim())
                .trim_end_matches(']');
            let name = declared
                .split([' ', '=', '.', '['])
                .next()
                .unwrap_or_default();
            for dependency in forbidden {
                // Whole dependency names only: a substring match would ban
                // aeris_hyperliquid_market_adapter through "hyper" while
                // the gate targets public server frameworks.
                assert!(
                    name != *dependency
                        && !name.starts_with(&format!("{dependency}-"))
                        && !name.starts_with(&format!("{dependency}_")),
                    "{relative} must not depend on {dependency}"
                );
            }
        }
    }

    fn production_dependencies(relative: &str) -> String {
        manifest(relative)
            .split_once("[dependencies]")
            .map(|(_, dependencies)| dependencies)
            .and_then(|dependencies| dependencies.split("\n[").next())
            .unwrap_or_default()
            .to_string()
    }

    fn assert_dependencies_are(relative: &str, allowed: &[&str]) {
        for line in production_dependencies(relative)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let (dependency, _) = line
                .split_once('=')
                .unwrap_or_else(|| panic!("invalid dependency in {relative}: {line}"));
            let dependency = dependency.trim();
            let dependency = dependency
                .split_once('.')
                .map_or(dependency, |(name, _)| name);
            assert!(
                allowed.contains(&dependency),
                "{relative} production dependency {dependency} is outside the pure core"
            );
        }
    }

    const INTERIM_PLATFORM_RUNTIME_UNSAFE_FILES: &[(&str, &str)] = &[
        (
            "crates/platform_runtime/src/display_timing.rs",
            "existing native display probes await the M8.3 FFI consolidation",
        ),
        (
            "crates/platform_runtime/src/io_cancellation.rs",
            "existing Windows CancelIoEx boundary awaits the M8.3 abort-latency decision",
        ),
        (
            "crates/platform_runtime/src/network_notifications.rs",
            "existing native interface-change callbacks await M8.3 extraction",
        ),
        (
            "crates/platform_runtime/src/power_notifications.rs",
            "existing native suspend/resume callbacks await M8.3 extraction",
        ),
        (
            "crates/platform_runtime/src/session_shutdown.rs",
            "existing native shutdown callbacks await M8.3 manual logoff verification",
        ),
    ];

    fn contains_unsafe_code(source: &str) -> bool {
        source.contains("allow(unsafe_code)")
            || source.contains("unsafe {")
            || source.contains("unsafe fn ")
            || source.contains("unsafe extern ")
            || source.contains("unsafe impl ")
            || source.contains("unsafe trait ")
    }

    fn unsafe_location_is_allowed(relative: &str) -> bool {
        relative.starts_with("crates/platform_sys/src/windows/")
            || relative.starts_with("crates/platform_sys/src/macos/")
            || INTERIM_PLATFORM_RUNTIME_UNSAFE_FILES
                .iter()
                .any(|(path, reason)| *path == relative && !reason.trim().is_empty())
    }

    fn declares_platform_sys_dependency(manifest: &str) -> bool {
        manifest
            .lines()
            .map(str::trim)
            .any(|line| line.starts_with("aeris_platform_sys") && line.contains('='))
    }

    fn platform_sys_dependent_is_allowed(relative: &str) -> bool {
        [
            "crates/observability/Cargo.toml",
            "crates/platform_runtime/Cargo.toml",
            "crates/trading_runtime/Cargo.toml",
        ]
        .contains(&relative)
    }

    const RAW_FILE_MUTATION_ALLOWLIST: &[(&str, &str, &str)] = &[
        (
            "apps/desktop/build.rs",
            "fs::write(",
            "Cargo build output is generated inside OUT_DIR",
        ),
        (
            "crates/platform_runtime/build.rs",
            "fs::write(",
            "Cargo build output is generated inside OUT_DIR",
        ),
        (
            "crates/platform_runtime/src/bin/aeris_launcher.rs",
            "fs::rename(",
            "launcher self-replacement stages an executable before the platform replacement boundary",
        ),
        (
            "crates/platform_runtime/src/data_root.rs",
            "fs::rename(",
            "owned legacy data migration moves entries without replacing files",
        ),
        (
            "crates/platform_sys/src/fs.rs",
            "fs::rename(",
            "platform_sys owns the raw move and atomic replacement implementations",
        ),
        (
            "crates/ui/design_system/build.rs",
            "fs::write(",
            "Cargo build output is generated inside OUT_DIR",
        ),
    ];

    fn raw_file_mutation_is_allowed(relative: &str, operation: &str) -> bool {
        RAW_FILE_MUTATION_ALLOWLIST
            .iter()
            .any(|(path, allowed_operation, reason)| {
                !reason.is_empty() && relative == *path && operation == *allowed_operation
            })
    }

    #[test]
    fn unsafe_location_rule_accepts_platform_ffi_and_rejects_other_sources() {
        assert!(unsafe_location_is_allowed(
            "crates/platform_sys/src/windows/fs.rs"
        ));
        assert!(unsafe_location_is_allowed(
            "crates/platform_runtime/src/io_cancellation.rs"
        ));
        assert!(!unsafe_location_is_allowed(
            "crates/market_runtime/src/provider.rs"
        ));
        assert!(contains_unsafe_code(
            "#![allow(unsafe_code)]\nfn call() { unsafe { ffi() } }"
        ));
        assert!(!contains_unsafe_code(
            "/// Reject an unsafe path.\nfn safe_call() {}"
        ));
    }

    #[test]
    fn unsafe_is_isolated_to_platform_sys_and_the_interim_exact_allowlist() {
        let mut interim = BTreeSet::new();
        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path).expect("source");
            let production = production_prefix(&contents);
            if !contains_unsafe_code(&production) {
                continue;
            }
            let relative = relative_string(&path);
            assert!(
                unsafe_location_is_allowed(&relative),
                "{relative} contains unsafe code outside the platform FFI boundary"
            );
            if relative.starts_with("crates/platform_runtime/") {
                interim.insert(relative);
            }
        }
        assert_eq!(
            interim,
            INTERIM_PLATFORM_RUNTIME_UNSAFE_FILES
                .iter()
                .map(|(path, reason)| {
                    assert!(
                        !reason.trim().is_empty(),
                        "{path} requires an explicit reason"
                    );
                    (*path).to_string()
                })
                .collect(),
            "the temporary platform_runtime FFI allowlist must stay exact until M8.3"
        );
    }

    #[test]
    fn reviewed_safe_debug_types_have_reasons_and_are_not_stale() {
        let mut observed = BTreeSet::new();
        for path in production_rust_sources() {
            let source = fs::read_to_string(&path).expect("source");
            let production = production_prefix(&source);
            let lines: Vec<_> = production.lines().collect();
            for index in 0..lines.len() {
                if let Some(name) = super::derived_debug_type_name(&lines[index..])
                    && super::is_sensitive_name(name)
                    && super::REVIEWED_SAFE_DEBUG_TYPES
                        .iter()
                        .any(|(reviewed, _)| *reviewed == name)
                {
                    observed.insert(name.to_string());
                }
            }
        }
        let expected: BTreeSet<_> = super::REVIEWED_SAFE_DEBUG_TYPES
            .iter()
            .map(|(name, reason)| {
                assert!(
                    !reason.trim().is_empty(),
                    "{name} requires an explicit reason"
                );
                (*name).to_string()
            })
            .collect();
        assert_eq!(
            observed, expected,
            "the reviewed Debug allowlist must not retain stale entries"
        );
    }

    #[test]
    fn platform_sys_dependent_rule_accepts_and_rejects_fixtures() {
        assert!(declares_platform_sys_dependency(
            "aeris_platform_sys = { path = \"../platform_sys\" }"
        ));
        assert!(!declares_platform_sys_dependency(
            "aeris_platform_runtime = { path = \"../platform_runtime\" }"
        ));
        assert!(platform_sys_dependent_is_allowed(
            "crates/trading_runtime/Cargo.toml"
        ));
        assert!(!platform_sys_dependent_is_allowed(
            "apps/desktop/Cargo.toml"
        ));
    }

    #[test]
    fn platform_sys_is_leaf_and_has_only_allowed_dependents() {
        assert_dependencies_are("crates/platform_sys/Cargo.toml", &[]);
        for path in workspace_manifests() {
            let contents = fs::read_to_string(&path).expect("manifest");
            if declares_platform_sys_dependency(&contents) {
                let relative = relative_string(&path);
                assert!(
                    platform_sys_dependent_is_allowed(&relative),
                    "{relative} must not depend directly on aeris_platform_sys"
                );
            }
        }
    }

    #[test]
    fn raw_file_mutation_rule_accepts_and_rejects_fixtures() {
        assert!(raw_file_mutation_is_allowed(
            "crates/platform_sys/src/fs.rs",
            "fs::rename("
        ));
        assert!(!raw_file_mutation_is_allowed(
            "crates/platform_sys/src/fs.rs",
            "fs::write("
        ));
        assert!(!raw_file_mutation_is_allowed(
            "apps/desktop/src/desktop/local_state.rs",
            "fs::rename("
        ));
    }

    #[test]
    fn production_raw_file_mutation_is_exactly_allowlisted() {
        let mut observed = BTreeSet::new();
        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path).expect("source");
            let production = production_prefix(&contents);
            let relative = relative_string(&path);
            for operation in ["fs::write(", "fs::rename("] {
                if !production.contains(operation) {
                    continue;
                }
                assert!(
                    raw_file_mutation_is_allowed(&relative, operation),
                    "{relative} performs raw {operation} outside the exact allowlist"
                );
                observed.insert((relative.clone(), operation));
            }
        }
        let expected = RAW_FILE_MUTATION_ALLOWLIST
            .iter()
            .map(|(path, operation, reason)| {
                assert!(
                    !reason.trim().is_empty(),
                    "{path} {operation} requires an explicit reason"
                );
                ((*path).to_string(), *operation)
            })
            .collect();
        assert_eq!(
            observed, expected,
            "the raw file mutation allowlist must not retain stale entries"
        );
    }

    #[test]
    fn cargo_dependency_direction_excludes_ui_from_backend_layers() {
        let ui = ["gpui", "aeris_chart_integration", "aeris_terminal_ui"];
        for relative in [
            "crates/market_engine/Cargo.toml",
            "crates/market_runtime/Cargo.toml",
            "crates/account_runtime/Cargo.toml",
            "crates/domain/instruments/Cargo.toml",
            "crates/domain/market_data/Cargo.toml",
            "crates/adapters/rithmic_protocol/Cargo.toml",
            "crates/adapters/hyperliquid_market/Cargo.toml",
            "crates/adapters/tastytrade_market/Cargo.toml",
            "crates/adapters/ctrader_open_api/Cargo.toml",
        ] {
            assert_excludes(relative, &ui);
        }
        assert_excludes(
            "crates/ui/chart_integration/Cargo.toml",
            &[
                "aeris_rithmic_protocol_adapter",
                "aeris_hyperliquid_market_adapter",
                "aeris_tastytrade_market_adapter",
                "aeris_ctrader_open_api_adapter",
            ],
        );
    }
    #[test]
    fn transitional_backend_crate_names_do_not_return() {
        let workspace = manifest("Cargo.toml");
        for retired in [
            "apps/engine",
            "crates/local_engine_client",
            "crates/local_history",
            "crates/local_storage",
            "crates/engine_protocol",
            "crates/transport",
            "crates/desktop_market_runtime",
            "crates/desktop_provider_runtime",
        ] {
            assert!(
                !repository_root().join(retired).exists(),
                "retired architecture boundary {retired} must not return"
            );
        }
        for replacement in ["market_runtime", "account_runtime", "contracts"] {
            assert!(
                repository_root()
                    .join("crates")
                    .join(replacement)
                    .join("Cargo.toml")
                    .is_file(),
                "current boundary crates/{replacement} is missing"
            );
            assert!(
                workspace.contains(replacement),
                "workspace lost crates/{replacement}"
            );
        }
    }
    #[test]
    fn provider_kit_remains_vendor_only() {
        assert!(
            !manifest("Cargo.toml").contains("provider_kit"),
            "provider_kit must not become a workspace application crate"
        );

        // The Rithmic kit is gitignored vendor input. A clean checkout does not have it and the
        // adapter's build script compiles without it, so only its contents are checked here.
        let provider_kit = repository_root().join("provider_kit");
        if !provider_kit.is_dir() {
            return;
        }

        let mut application_sources = Vec::new();
        collect_files(
            &provider_kit,
            |path| {
                path.file_name().is_some_and(|name| name == "Cargo.toml")
                    || path.extension().is_some_and(|extension| extension == "rs")
            },
            &mut application_sources,
        );
        assert!(
            application_sources.is_empty(),
            "provider_kit must not gain Aeris Rust/application source: {:?}",
            application_sources
                .iter()
                .map(|path| relative_string(path))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn provider_wire_and_aeris_charts_boundaries_remain_isolated() {
        let rithmic_adapter = manifest("crates/adapters/rithmic_protocol/src/lib.rs");
        assert!(rithmic_adapter.contains("mod generated {"));
        assert!(!rithmic_adapter.contains("pub mod generated"));

        let root_manifest = manifest("Cargo.toml");
        let expected_source = "https://github.com/AerisTerminal/aeris-charts.git";
        let expected_revision = "36d5b92e5a6df3a2646e7253db6c46c3740e8988";
        for dependency in [
            "aeris_charts_engine",
            "aeris_charts_indicators",
            "aeris_charts_render",
            "aeris_charts_render_gpui",
            "aeris_charts_native",
        ] {
            assert!(
                root_manifest.contains(&format!(
                    "{dependency} = {{ git = \"{expected_source}\", rev = \"{expected_revision}\""
                )),
                "{dependency} must remain pinned to the approved Aeris Charts revision"
            );
        }

        for path in workspace_manifests() {
            let relative = relative_string(&path);
            let contents = fs::read_to_string(&path).expect("manifest");
            if contents.contains("aeris_charts_indicators.workspace") {
                assert_eq!(
                    relative, "crates/study_sdk/Cargo.toml",
                    "pure Aeris Charts TA must enter Aeris only through the Study SDK facade"
                );
            }
            if contents.contains("aeris_charts_engine.workspace")
                || contents.contains("aeris_charts_render.workspace")
                || contents.contains("aeris_charts_render_gpui.workspace")
                || contents.contains("aeris_charts_native.workspace")
            {
                assert_eq!(relative, "crates/ui/chart_integration/Cargo.toml");
            }
        }
    }

    /// With default features disabled, `gpui_platform` only enables the macOS text system through
    /// `font-kit`; without it GPUI installs a no-op text system and macOS renders no text at all.
    #[test]
    fn gpui_platform_keeps_the_macos_text_system() {
        let root_manifest = manifest("Cargo.toml");
        let gpui_platform = root_manifest
            .lines()
            .find(|line| line.trim_start().starts_with("gpui_platform = "))
            .expect("workspace declares gpui_platform");
        assert!(
            gpui_platform.contains("\"font-kit\""),
            "gpui_platform must enable `font-kit` or macOS renders no text: {gpui_platform}"
        );
    }

    /// Chart interaction is Aeris Charts behavior. Terminal binds GPUI listeners through the
    /// `aeris_charts_render_gpui::input` adapter and the engine's `input_*` controller; it never
    /// routes gestures, arbitrates hover, picks cursors, or composes chart features from engine
    /// primitives. A feature that seems to need these calls is missing an engine operation.
    #[test]
    fn chart_integration_routes_interaction_through_aeris_charts() {
        const ENGINE_INTERACTION_PRIMITIVES: &[&str] = &[
            "drawing_create_begin",
            "drawing_create_click",
            "drawing_create_move",
            "drawing_create_finish",
            "drawing_tool_pointer_down",
            "drawing_tool_pointer_move",
            "drawing_tool_pointer_up",
            "drawing_tool_activate",
            "drawing_drag_start_at",
            "drawing_drag_to",
            "brush_create_start",
            "brush_create_add",
            "update_drawing_hover",
            "drawing_hover_at",
            "hit_test_series",
            "set_hovered_series",
            "set_separator_hover",
            "set_crosshair_ohlc_magnet",
            "delta_tooltip_mouse_down_with_shift",
            "set_area_brush_state",
            "time_scale_start_scroll",
            "time_axis_start_scale",
            "price_axis_start_scale",
            "price_axis_start_scroll",
            "kinetic_begin_sampling",
            "start_keyboard_scroll",
            "set_trading_hover",
            "set_trading_pressed",
            "trading_drag_start_at",
            "trading_activate_at",
            "activate_alert_create_at",
            "measure_pointer_down",
            "GestureResolver",
            "ChartDrawingTool",
        ];
        for path in production_sources_under("crates/ui/chart_integration/src") {
            let relative = relative_string(&path);
            let contents = fs::read_to_string(&path).expect("chart integration source");
            let production = production_prefix(&contents);
            for primitive in ENGINE_INTERACTION_PRIMITIVES {
                assert!(
                    !contains_identifier(&production, primitive),
                    "{relative} uses `{primitive}`; chart interaction belongs in the Aeris Charts \
                     input controller and the shared GPUI adapter"
                );
            }
        }
    }

    #[test]
    fn trusted_native_study_packaging_keeps_core_owners_and_dynamic_loading_out_of_the_sdk() {
        assert_excludes(
            "crates/study_sdk/Cargo.toml",
            &[
                "aeris_market_engine",
                "aeris_account_runtime",
                "aeris_chart_integration",
                "aeris_rithmic_protocol_adapter",
                "aeris_hyperliquid_market_adapter",
                "aeris_tastytrade_market_adapter",
                "aeris_ctrader_open_api_adapter",
                "aeris_charts_engine",
                "aeris_charts_render",
                "aeris_charts_render_gpui",
                "gpui",
                "libloading",
            ],
        );
        let packages = manifest("apps/desktop/src/study_packages.rs");
        assert!(packages.contains("TRUSTED_NATIVE_STUDY_PACKAGES"));
        for forbidden in ["LoadLibrary", "dlopen", "read_dir", "libloading"] {
            assert!(
                !packages.contains(forbidden),
                "trusted native package boundary must remain static; found {forbidden}"
            );
        }
    }
    #[test]
    fn desktop_presentation_layers_exclude_provider_and_storage_ownership() {
        for relative in [
            "apps/desktop/Cargo.toml",
            "crates/ui/chart_integration/Cargo.toml",
            "crates/ui/terminal_ui/Cargo.toml",
        ] {
            assert_excludes(
                relative,
                &[
                    "aeris_rithmic_protocol_adapter",
                    "aeris_hyperliquid_market_adapter",
                    "aeris_tastytrade_market_adapter",
                    "aeris_ctrader_open_api_adapter",
                    "aeris_local_history",
                    "aeris_local_storage",
                    "aeris_market_engine",
                    "aeris_provider_history",
                    "rusqlite",
                ],
            );
        }

        for root in ["apps/desktop/src", "crates/ui"] {
            for path in production_sources_under(root) {
                let contents = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
                let production = production_prefix(&contents);
                for forbidden in [
                    "rusqlite::",
                    "HistoryStore",
                    "RithmicHistoryConnection",
                    "RithmicTickerConnection",
                ] {
                    assert!(
                        !production.contains(forbidden),
                        "{} gives a presentation layer backend ownership through {forbidden}",
                        relative_string(&path)
                    );
                }
            }
        }
    }

    fn constructs_series_cadence(source: &str) -> bool {
        production_prefix(source).contains("SeriesCadence::")
    }

    #[test]
    fn series_cadence_construction_rule_accepts_and_rejects_fixtures() {
        assert!(!constructs_series_cadence(
            "fn restore() { aeris_market_runtime::legacy_interval(series); }"
        ));
        assert!(constructs_series_cadence(
            "fn persist() { let cadence = SeriesCadence::FixedSeconds; }"
        ));
    }

    #[test]
    fn desktop_never_constructs_series_cadence() {
        for path in production_sources_under("apps/desktop/src") {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            assert!(
                !constructs_series_cadence(&contents),
                "{} constructs SeriesCadence; cadence mapping belongs to market_runtime",
                relative_string(&path)
            );
        }
    }

    #[test]
    fn desktop_quote_authority_rule_scans_after_test_modules() {
        let path = "apps/desktop/src/trading.rs";
        assert!(!super::violates_desktop_quote_boundary(
            path,
            "#[cfg(test)] mod tests { fn observation() { SimulatedMarketObservation; } }\nfn command() {}"
        ));
        assert!(super::violates_desktop_quote_boundary(
            path,
            "#[cfg(test)] mod tests { fn observation() { SimulatedMarketObservation; } }\nfn command() { SimulatedMarketObservation; }"
        ));
        assert!(!super::violates_desktop_quote_boundary(
            "crates/trading_runtime/src/core/mod.rs",
            "fn owner() { SimulatedMarketObservation; }"
        ));
    }

    #[test]
    fn fixture_market_boundary_accepts_only_gated_test_and_diagnostic_sources() {
        let fixture = "apps/desktop/src/desktop/test_support/fixture_market.rs";
        let diagnostic = "apps/desktop/src/readiness_conformance.rs";
        let production = "apps/desktop/src/market_worker.rs";
        assert!(!super::violates_fixture_market_boundary(
            fixture,
            "pub struct FixtureMarketWorker;"
        ));
        assert!(!super::violates_fixture_market_boundary(
            diagnostic,
            "use aeris_desktop::test_support::FixtureMarketWorker;"
        ));
        assert!(!super::violates_fixture_market_boundary(
            production,
            "#[cfg(test)] mod tests { use FixtureMarketWorker; }\nfn production() {}"
        ));
        let library = manifest("apps/desktop/src/lib.rs");
        let desktop = manifest("apps/desktop/src/desktop.rs");
        assert!(library.contains(
            "#[cfg(any(test, feature = \"diagnostics\"))]\n#[path = \"desktop/test_support/mod.rs\"]"
        ));
        assert!(desktop.contains(
            "#[cfg(any(test, feature = \"diagnostics\"))]\n#[path = \"readiness_conformance.rs\"]"
        ));
    }

    #[test]
    fn fixture_market_boundary_rejects_production_after_test_module() {
        assert!(super::violates_fixture_market_boundary(
            "apps/desktop/src/market_worker.rs",
            "#[cfg(test)] mod tests { use FixtureMarketWorker; }\nstruct Production(FixtureMarketWorker);"
        ));
    }

    #[test]
    fn fixture_market_worker_stays_inside_test_support() {
        for path in production_sources_under("apps/desktop/src") {
            let relative = relative_string(&path);
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
            assert!(
                !super::violates_fixture_market_boundary(&relative, &source),
                "{relative} exposes the fixture outside desktop test support"
            );
        }
    }

    #[test]
    fn desktop_never_constructs_simulated_market_observations() {
        for path in production_sources_under("apps/desktop/src") {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            assert!(
                !super::violates_desktop_quote_boundary(&relative_string(&path), &contents),
                "{} bypasses the trading owner's QuoteBook",
                relative_string(&path)
            );
        }
    }

    #[test]
    fn market_engine_has_one_lock_free_mutable_owner() {
        let owner = manifest("crates/market_engine/src/lib.rs");
        for field in [
            "demands: DemandRegistry",
            "providers: ProviderManager",
            "series: SeriesStore",
            "subscriptions: SubscriptionRegistry",
            "publications: PublicationManager",
        ] {
            assert!(
                owner.contains(field),
                "MarketEngine must retain authoritative ownership of {field}"
            );
        }

        for path in production_sources_under("crates/market_engine/src") {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let production = production_prefix(&contents);
            for forbidden in ["Mutex<", "RwLock<", "static mut", "OnceLock<", "LazyLock<"] {
                assert!(
                    !production.contains(forbidden),
                    "{} splits MarketEngine authority through {forbidden}",
                    relative_string(&path)
                );
            }
        }
    }

    #[test]
    fn market_characterization_names_are_required_individually() {
        let source = fs::read_to_string(
            repository_root()
                .join("crates/market_runtime/src/market_service/tests/characterization.rs"),
        )
        .expect("characterization test file");
        assert_eq!(
            super::missing_market_characterization_tests(&source),
            Vec::<&str>::new()
        );
        for required in super::MARKET_CHARACTERIZATION_TESTS {
            let without = source.replace(&format!("fn {required}() {{"), "");
            assert_eq!(
                super::missing_market_characterization_tests(&without),
                vec![*required],
                "{required} must be individually enforced"
            );
        }
    }

    #[test]
    fn market_replay_names_are_required_individually() {
        let source = fs::read_to_string(
            repository_root().join("crates/market_runtime/src/market_service/replay/mod.rs"),
        )
        .expect("market replay tests");
        assert_eq!(
            super::missing_market_replay_tests(&source),
            Vec::<&str>::new()
        );
        for required in super::MARKET_REPLAY_TESTS {
            let without = source.replace(&format!("fn {required}() {{"), "");
            assert_eq!(
                super::missing_market_replay_tests(&without),
                vec![*required],
                "{required} must be individually enforced"
            );
        }
    }

    #[test]
    /// Only the files are asserted here, never the names of the tests inside
    /// them.
    ///
    /// Asserting that a source contains a particular test-function name proves
    /// nothing about behaviour: renaming a test failed the gate while gutting
    /// its body passed it, so the assertion punished the one change that was
    /// safe and waved through the one that was not. What a test covers is the
    /// test's job to assert; this one only keeps the boundaries from vanishing.
    fn durable_migration_test_boundaries_remain_present() {
        for relative in [
            "tools/run_rithmic_protocol_conformance.sh",
            "crates/market_runtime/src/market_service/history/tracker.rs",
            "crates/provider_history/tests/handoff_conformance.rs",
            "crates/contracts/tests/contracts.rs",
            "crates/market_runtime/src/market_service/tests.rs",
            "crates/market_runtime/src/market_service/tastytrade_tests.rs",
            "crates/market_runtime/src/study/tests.rs",
            "crates/account_runtime/src/account_service/tests.rs",
            "apps/desktop/src/desktop/workspace_surface_tests.rs",
            "apps/desktop/src/http_wiring_tests.rs",
            "apps/desktop/src/readiness_conformance.rs",
        ] {
            assert!(
                repository_root().join(relative).is_file(),
                "durable test boundary {relative} is missing"
            );
        }
    }
    #[test]
    fn repository_markdown_inventory_is_exact() {
        let actual = fs::read_dir(repository_root())
            .expect("repository root is readable")
            .filter_map(|entry| {
                let path = entry.expect("repository entry is readable").path();
                (path.extension().is_some_and(|extension| extension == "md"))
                    .then(|| path.file_name().expect("Markdown has a name").to_owned())
            })
            .collect::<BTreeSet<_>>();
        let expected = super::REPOSITORY_MARKDOWN_FILES
            .iter()
            .copied()
            .map(std::ffi::OsString::from)
            .collect();
        assert_eq!(actual, expected, "repository Markdown inventory drifted");
    }

    #[test]
    fn desktop_manifest_excludes_backend_implementation_crates() {
        assert_excludes(
            "apps/desktop/Cargo.toml",
            &[
                "aeris_desktop_market_runtime",
                "aeris_desktop_provider_runtime",
                "aeris_rithmic_protocol_adapter",
                "aeris_hyperliquid_market_adapter",
                "aeris_tastytrade_market_adapter",
                "aeris_ctrader_open_api_adapter",
                "aeris_provider_history",
                "aeris_local_storage",
                "aeris_local_history",
                "aeris_market_engine",
            ],
        );
    }

    #[test]
    fn workspace_uses_only_aeris_owned_gpui_controls() {
        const RETIRED_COMPONENT_IDENTITIES: &[&str] = &[
            concat!("gpui", "-component"),
            concat!("gpui", "_component"),
            concat!("long", "bridge"),
        ];

        let mut inspected = workspace_manifests();
        inspected.push(repository_root().join("Cargo.lock"));
        inspected.extend(production_rust_sources());

        for path in inspected {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let normalized = contents.to_ascii_lowercase();
            for retired in RETIRED_COMPONENT_IDENTITIES {
                assert!(
                    !normalized.contains(retired),
                    "{} restores retired external GPUI component identity {retired}",
                    relative_string(&path)
                );
            }
        }
    }

    #[test]
    fn desktop_rendering_remains_component_owned() {
        for relative in [
            "apps/desktop/src/components/chart_context_menus.rs",
            "apps/desktop/src/components/chart_surface.rs",
            "apps/desktop/src/components/chart_toolbar_menus.rs",
            "apps/desktop/src/components/chrome_menu.rs",
            "apps/desktop/src/components/drawing_toolbar.rs",
            "apps/desktop/src/components/indicator_menu.rs",
            "apps/desktop/src/components/symbol_menu.rs",
            "apps/desktop/src/components/terminal_chrome.rs",
            "apps/desktop/src/components/terminal_view.rs",
            "apps/desktop/src/components/workspace_layout.rs",
        ] {
            assert!(
                repository_root().join(relative).is_file(),
                "desktop component boundary {relative} is missing"
            );
        }

        let main = manifest("apps/desktop/src/main.rs");
        assert!(main.contains("desktop::run("));
        assert!(!main.contains("struct WorkspaceSurface"));
        let production = production_prefix(&main);
        for rendering_primitive in [
            "impl Render for",
            "Button::new(",
            "Input::new(",
            "Loader::",
            "MenuRow::",
            "compact_menu_panel(",
            "div()",
        ] {
            assert!(
                !production.contains(rendering_primitive),
                "desktop main must compose component modules instead of rendering {rendering_primitive} inline"
            );
        }
    }

    /// Buttons, menus, dialogs, tabs and switches are `native_ui` components. Desktop components
    /// compose them; they never hand-build a click target, menu panel, scrim or tab, and the
    /// retired per-file helpers stay gone. The few remaining exceptions are listed with why.
    #[test]
    fn desktop_controls_come_from_native_ui_components() {
        // Retired helpers, built with `concat!` so this rule never matches itself.
        const RETIRED_HELPERS: &[&str] = &[
            concat!("flat_compact_", "menu_panel"),
            concat!("flush_", "in_panel"),
            concat!("round_panel_", "ends"),
            concat!("round_icon_", "button"),
            concat!("chrome_icon_", "button"),
            concat!("chrome_button_", "style"),
            concat!("button_", "activation"),
            concat!("Modal", "Layer"),
            concat!("modal_", "header("),
            concat!("modal_", "footer("),
            concat!("native_ui::", "control"),
        ];
        // Each entry: a hand-built pattern, and the files that may still use it with the reason.
        let owned_patterns: &[(&str, &[(&str, &str)])] = &[
            (
                concat!(".cursor_", "pointer()"),
                &[
                    ("accounts_panel.rs", "selectable account rows"),
                    (
                        "market_screener_view.rs",
                        "sortable column headers and market rows",
                    ),
                    ("platform_menu.rs", "theme radio cards"),
                    ("watchlist_panel.rs", "selectable watchlist rows"),
                ],
            ),
            (
                concat!("Role::", "Button"),
                &[
                    ("accounts_panel.rs", "selectable account rows"),
                    ("drawing_toolbar.rs", "favorites toolbar drag handle"),
                    (
                        "market_screener_view.rs",
                        "sortable column headers and market rows",
                    ),
                    (
                        "terminal_chrome.rs",
                        "window caption controls own native hit-testing",
                    ),
                    ("watchlist_panel.rs", "selectable watchlist rows"),
                ],
            ),
            (concat!("Role::", "Tab)"), &[]),
            (
                concat!("Role::", "TabList"),
                &[(
                    "terminal_view.rs",
                    "draggable, closable workspace tab strip",
                )],
            ),
            (
                concat!("gpui_base::", "Button"),
                &[("platform_menu.rs", "theme radio cards")],
            ),
            (concat!("surface_", "overlay"), &[]),
        ];

        for path in production_sources_under("apps/desktop/src/components") {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let production = production_prefix(&contents);
            let relative = relative_string(&path);
            let file = relative.rsplit('/').next().unwrap_or_default();
            for retired in RETIRED_HELPERS {
                assert!(
                    !production.contains(retired),
                    "{relative} restores the retired UI helper {retired}; use the native_ui component"
                );
            }
            for (pattern, allowed) in owned_patterns {
                if allowed
                    .iter()
                    .any(|(allowed_file, _)| *allowed_file == file)
                {
                    continue;
                }
                assert!(
                    !production.contains(pattern),
                    "{relative} hand-builds {pattern}; use the native_ui Button, MenuPanel, \
                     Dialog, TabList/Tab or Switch component"
                );
            }
        }
    }

    #[test]
    fn ci_cargo_packages_and_targets_exist_in_the_workspace() {
        // Validate executable targets as well as artifact names: an obsolete package
        // must fail here before a hosted lane burns its timeout on a missing target.
        let manifests = workspace_manifests();
        for workflow in [
            ".github/workflows/ci.yml",
            ".github/workflows/live_market_gates.yml",
            ".github/workflows/macos_preview.yml",
        ] {
            let source = manifest(workflow);
            for line in source.lines().filter(|line| line.contains("cargo ")) {
                let words: Vec<_> = line.split_whitespace().collect();
                let Some(package_index) = words
                    .iter()
                    .position(|word| matches!(*word, "-p" | "--package"))
                else {
                    continue;
                };
                let package = words.get(package_index + 1).expect("package argument");
                let package_manifest = manifests
                    .iter()
                    .find(|path| {
                        fs::read_to_string(path)
                            .expect("manifest")
                            .lines()
                            .any(|line| line.trim() == format!("name = \"{package}\""))
                    })
                    .unwrap_or_else(|| panic!("{workflow} references absent package {package}"));
                let directory = package_manifest.parent().expect("package directory");
                for (flag, folder) in [
                    ("--example", "examples"),
                    ("--test", "tests"),
                    ("--bin", "src/bin"),
                ] {
                    if let Some(index) = words.iter().position(|word| *word == flag) {
                        let target = words.get(index + 1).expect("target argument");
                        assert!(
                            directory
                                .join(folder)
                                .join(format!("{target}.rs"))
                                .is_file()
                                || directory
                                    .join(folder)
                                    .join(target)
                                    .join("main.rs")
                                    .is_file(),
                            "{workflow} references absent {flag} {target} in {package}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn market_core_manifests_exclude_process_and_runtime_dependencies() {
        assert_dependencies_are(
            "crates/application/Cargo.toml",
            &["aeris_instruments", "aeris_market_data", "sha2"],
        );
        // Instrument quote leases use the leaf identity type, not trading or a provider runtime.
        assert_dependencies_are(
            "crates/market_engine/Cargo.toml",
            &["aeris_instruments", "aeris_market_data"],
        );
        // The shared weekly-hours calendar needs the already-pinned IANA zone
        // database for DST-correct market status and trading DAY recovery.
        assert_dependencies_are(
            "crates/domain/instruments/Cargo.toml",
            &["chrono", "chrono-tz"],
        );
        assert_dependencies_are("crates/domain/market_data/Cargo.toml", &[]);
    }

    #[test]
    fn retired_runtime_wrappers_do_not_reenter_the_workspace() {
        for relative in ["Cargo.toml", "apps/desktop/Cargo.toml"] {
            assert_excludes(
                relative,
                &[
                    "aeris_desktop_market_runtime",
                    "aeris_desktop_provider_runtime",
                    "aeris_local_engine_client",
                    "aeris_local_history",
                ],
            );
        }
        for relative in [
            "apps/engine",
            "crates/local_engine_client",
            "crates/local_history",
            "crates/desktop_market_runtime",
            "crates/desktop_provider_runtime",
        ] {
            assert!(
                !repository_root().join(relative).exists(),
                "retired runtime {relative} returned"
            );
        }
    }
    #[test]
    fn workspace_excludes_distributed_system_dependencies() {
        for path in workspace_manifests() {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for forbidden in [
                "actix-web",
                "axum",
                "aws-sdk-sqs",
                "consul",
                "etcd",
                "kafka",
                "kube",
                "mongodb",
                "mysql",
                "nats",
                "postgres",
                "rabbitmq",
                "redis",
                "rocket",
                "tonic",
                "warp",
            ] {
                assert!(
                    !contents.contains(forbidden),
                    "{} must not introduce distributed-system dependency {forbidden}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn workspace_has_exactly_one_application_process() {
        let applications = fs::read_dir(repository_root().join("apps"))
            .expect("apps directory is readable")
            .filter_map(|entry| {
                let path = entry.expect("application entry is readable").path();
                path.join("Cargo.toml").is_file().then(|| {
                    path.file_name()
                        .expect("application has a name")
                        .to_string_lossy()
                        .into_owned()
                })
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            applications,
            ["desktop".to_string()].into(),
            "Aeris must have exactly one application process"
        );
    }
    #[test]
    fn production_sources_have_no_placeholder_architecture() {
        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for placeholder in ["todo!", "unimplemented!"] {
                assert!(
                    !contents.contains(placeholder),
                    "{} contains placeholder macro {placeholder}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn production_diagnostics_never_panic_on_a_closed_stderr() {
        // `eprintln!` panics when stderr is a closed pipe. On the GPUI main thread that panic
        // cannot unwind out of the window procedure and aborts the desktop; on a runtime worker
        // it ends the worker. Command-line tools, build scripts and tests keep `eprintln!`.
        for path in production_rust_sources() {
            let relative = relative_string(&path);
            if relative.contains("/src/bin/")
                || relative.contains("/examples/")
                || relative.contains("/tests/")
                || relative.ends_with("/build.rs")
                || relative.ends_with("tests.rs")
            {
                continue;
            }
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            if contents.starts_with("#![cfg(test)]") {
                continue;
            }
            assert!(
                !lines_outside_test_modules(&contents)
                    .iter()
                    .any(|line| line.contains("eprintln!(")),
                "{relative} writes diagnostics with eprintln!; use aeris_observability::diagnostic!"
            );
        }
    }

    /// Lines outside `#[cfg(test)]` modules at any nesting depth. Relies on rustfmt placing a
    /// module's closing brace alone at the module's indentation.
    fn lines_outside_test_modules(contents: &str) -> Vec<&str> {
        let mut lines = contents.lines().peekable();
        let mut production = Vec::new();
        while let Some(line) = lines.next() {
            if line.trim() == "#[cfg(test)]"
                && let Some(module) = lines.peek()
                && module.trim_start().starts_with("mod ")
                && module.ends_with('{')
            {
                let close = format!("{}}}", &module[..module.len() - module.trim_start().len()]);
                for skipped in lines.by_ref() {
                    if skipped == close {
                        break;
                    }
                }
                continue;
            }
            production.push(line);
        }
        production
    }

    #[test]
    fn nested_test_modules_are_not_production_lines() {
        let source = "fn a() {}\npub mod inner {\n    fn b() {}\n\n    #[cfg(test)]\n    mod tests {\n        fn c() {}\n    }\n\n    fn d() {}\n}\n";
        assert_eq!(
            lines_outside_test_modules(source),
            [
                "fn a() {}",
                "pub mod inner {",
                "    fn b() {}",
                "",
                "",
                "    fn d() {}",
                "}"
            ]
        );
    }

    #[test]
    fn dead_code_suppressions_remain_at_external_decode_boundaries() {
        let allowed = BTreeSet::from([
            "crates/adapters/rithmic_protocol/src/lib.rs",
            "crates/adapters/ctrader_open_api/src/lib.rs",
        ]);

        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            if contents.contains("#[allow(dead_code") {
                let relative = relative_string(&path);
                assert!(
                    allowed.contains(relative.as_str()),
                    "{relative} suppresses dead-code analysis outside an external decode boundary"
                );
            }
        }
    }

    #[test]
    fn production_traits_remain_justified_boundaries() {
        let mut actual = BTreeSet::new();
        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let relative = relative_string(&path);
            for name in production_prefix(&contents)
                .lines()
                .filter_map(declared_trait_name)
            {
                actual.insert(format!("{relative}::{name}"));
            }
        }
        let expected = BTreeSet::from([
            "apps/desktop/src/desktop.rs::AccountRefreshExitClaim".to_string(),
            "crates/context_runtime/src/lib.rs::ContextCredentialStore".to_string(),
            "crates/context_runtime/src/lib.rs::ContextFetcher".to_string(),
            // The single recurring-session projection accepts domain and provider wire segments
            // without importing the provider-owned contracts into the leaf instrument crate.
            "crates/domain/instruments/src/calendar/weekly_hours.rs::WeeklySegment".to_string(),
            // Trading publishes bounded quote interest without depending on market_runtime.
            "crates/domain/trading/src/market.rs::QuoteInterestSink".to_string(),
            "crates/market_runtime/src/market_service/ctrader.rs::CtraderMarketLink".to_string(),
            // The market owner and provider workers share one injectable time boundary.
            "crates/market_runtime/src/market_service/clock.rs::Clock".to_string(),
            "crates/market_runtime/src/market_service/mod.rs::HistorySource".to_string(),
            // One bounded catalog command/event boundary for all registered provider workers.
            "crates/market_runtime/src/market_service/mod.rs::ProviderCatalogChannel".to_string(),
            "crates/adapters/rithmic_protocol/src/history_adapter.rs::RithmicHistoryTransport"
                .to_string(),
            "crates/adapters/rithmic_protocol/src/provider_runtime.rs::ProviderSessionDriver"
                .to_string(),
            "crates/platform_runtime/src/credential_vault.rs::CredentialVault".to_string(),
            "crates/platform_runtime/src/credential_vault.rs::NativeCredentialBackend".to_string(),
            "crates/platform_runtime/src/lifecycle.rs::LifecycleHooks".to_string(),
            "crates/provider_history/src/model.rs::ProviderHistoryAdapter".to_string(),
            // The sole trading owner receives injectable wall and monotonic time.
            "crates/trading_runtime/src/clock.rs::Clock".to_string(),
            // The simulated venue's explicitly approved, touch-only execution policy hooks.
            "crates/trading_runtime/src/execution/mod.rs::FillPrice".to_string(),
            // The same venue fills full quantity without modeling order-book depth.
            "crates/trading_runtime/src/execution/mod.rs::Liquidity".to_string(),
        ]);
        assert_eq!(
            actual, expected,
            "production traits require an intentional provider/platform/security or execution-policy boundary"
        );
    }
    #[test]
    fn production_market_queues_exclude_unbounded_channels() {
        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for forbidden in ["mpsc::channel(", "unbounded(", "unbounded_channel("] {
                assert!(
                    !production_prefix(&contents).contains(forbidden),
                    "{} contains unbounded queue constructor {forbidden}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn market_contracts_exclude_trading_execution_commands() {
        let protocol = manifest("crates/contracts/src/messages.rs");
        for forbidden in [
            "CancelOrder",
            "ExecutionReport",
            "FlattenPosition",
            "OrderAction",
            "OrderRequest",
            "PlaceOrder",
            "Position",
            "ReplaceOrder",
            "SubmitOrder",
        ] {
            assert!(
                !protocol.contains(forbidden),
                "market-data contracts must not add future execution message {forbidden}"
            );
        }
    }

    #[test]
    fn market_engine_remains_one_cohesive_crate() {
        for forbidden in [
            "market_engine_core",
            "market_engine_runtime",
            "market_engine_services",
            "market_engine_common",
            "market_engine_types",
        ] {
            assert!(
                !repository_root().join("crates").join(forbidden).exists(),
                "{forbidden} must remain part of the cohesive market_engine crate"
            );
            assert!(
                !manifest("Cargo.toml").contains(forbidden),
                "workspace must not contain speculative crate {forbidden}"
            );
        }
    }

    #[test]
    fn production_excludes_cross_process_shared_memory() {
        for path in workspace_manifests()
            .into_iter()
            .chain(production_rust_sources())
        {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for forbidden in [
                "CreateFileMapping",
                "MapViewOfFile",
                "memmap",
                "mmap(",
                "shared-memory",
                "shared_memory",
                "shmem",
            ] {
                assert!(
                    !contents.contains(forbidden),
                    "{} must not introduce cross-process shared-memory primitive {forbidden}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn in_process_runtime_manifests_exclude_server_dependencies() {
        for retired in ["apps/engine", "crates/local_engine_client"] {
            assert!(
                !repository_root().join(retired).exists(),
                "retired cross-process boundary {retired} returned"
            );
        }
        for relative in [
            "crates/market_runtime/Cargo.toml",
            "crates/account_runtime/Cargo.toml",
            "apps/desktop/Cargo.toml",
        ] {
            assert_excludes(
                relative,
                &[
                    concat!("inter", "process"),
                    "actix",
                    "axum",
                    "hyper",
                    "rocket",
                    "tonic",
                    "warp",
                ],
            );
        }
    }
    #[test]
    fn runtime_owns_startup_catalog_resolution_and_selection() {
        let source = manifest("apps/desktop/src/engine_market_worker/selection_commands.rs");
        for forbidden in [
            "StartupResolution::Searching",
            "handle_startup_catalog_event",
            "startup_resolve: true",
            "record.startup_command_generation",
            "STARTUP_CATALOG_COMMAND_GENERATION",
            "STARTUP_CATALOG_RESOLUTION_TIMEOUT",
            "poll_market_event_until",
            "recv_timeout",
            "thread::sleep",
        ] {
            assert!(
                !source.contains(forbidden),
                "desktop must not resolve startup catalogs through {forbidden}"
            );
        }
        assert!(source.contains("market.select_and_demand("));
        let runtime = manifest("crates/market_runtime/src/market_service/instrument_selection.rs");
        assert!(
            runtime.contains("fn begin_selection(")
                && runtime.contains("pending_selections")
                && runtime.contains("ProviderCatalogCommand::Search(search)")
                && runtime.contains("SelectionError::Superseded")
        );
        let desktop = manifest("apps/desktop/src/desktop.rs");
        assert!(!desktop.contains("RITHMIC_CATALOG_READY_MESSAGE"));
        let fences = manifest("crates/market_runtime/src/market_service/command_fences.rs");
        for required in [
            "CommandKind::StartupResolve",
            "Epoch<CommandTag>",
            "remove_consumer",
        ] {
            assert!(
                fences.contains(required),
                "catalog command fence lost {required}"
            );
        }
        let coordinator = manifest("crates/market_runtime/src/market_service/coordinator.rs");
        for forbidden in [
            "catalog_searches:",
            "startup_catalog_searches:",
            "catalog_selections:",
            "startup_catalog_selections:",
            "catalog_screens:",
        ] {
            assert!(
                !coordinator.contains(forbidden),
                "duplicate command map {forbidden}"
            );
        }
        let contracts = manifest("crates/contracts/src/messages.rs");
        assert!(!contracts.contains("STARTUP_CATALOG_COMMAND_GENERATION"));
    }

    #[test]
    fn workspace_persistence_has_no_general_transport_layer() {
        assert!(!repository_root().join("crates/transport").exists());
        let contracts = manifest("crates/contracts/Cargo.toml");
        assert!(!contracts.contains("transport"));
        let local = manifest("apps/desktop/src/desktop/local_state.rs");
        for forbidden in ["EnvelopeDecoder", "encode_envelope", "BoundedBinaryFrame"] {
            assert!(
                !local.contains(forbidden),
                "workspace persistence reintroduced transport framing through {forbidden}"
            );
        }
        for required in [
            "WorkspaceState::decode",
            "workspace.encode_to_vec()",
            "LegacyWorkspaceEnvelope",
        ] {
            assert!(
                local.contains(required),
                "workspace persistence lost {required}"
            );
        }
    }
    #[test]
    fn provider_adapters_exclude_storage_and_ui_from_production_dependencies() {
        for relative in [
            "crates/adapters/rithmic_protocol/Cargo.toml",
            "crates/adapters/hyperliquid_market/Cargo.toml",
            "crates/adapters/tastytrade_market/Cargo.toml",
            "crates/adapters/ctrader_open_api/Cargo.toml",
        ] {
            let dependencies = production_dependencies(relative);
            for forbidden in [
                "aeris_local_storage",
                "aeris_local_history",
                "aeris_chart_integration",
                "aeris_terminal_ui",
                "gpui",
            ] {
                assert!(
                    !dependencies.contains(forbidden),
                    "{relative} production dependencies must not contain {forbidden}"
                );
            }
        }
    }

    #[test]
    fn engine_manifest_owns_backend_composition_without_ui() {
        let market = manifest("crates/market_runtime/Cargo.toml");
        for dependency in [
            "aeris_rithmic_protocol_adapter",
            "aeris_hyperliquid_market_adapter",
            "aeris_tastytrade_market_adapter",
            "aeris_market_engine",
            "aeris_provider_history",
            "aeris_platform_runtime",
        ] {
            assert!(
                market.contains(dependency),
                "market_runtime must compose {dependency}"
            );
        }
        let account = manifest("crates/account_runtime/Cargo.toml");
        for dependency in ["aeris_account", "aeris_platform_runtime"] {
            assert!(
                account.contains(dependency),
                "account_runtime must compose {dependency}"
            );
        }
        for relative in [
            "crates/market_runtime/Cargo.toml",
            "crates/account_runtime/Cargo.toml",
        ] {
            assert_excludes(
                relative,
                &["gpui", "aeris_chart_integration", "aeris_terminal_ui"],
            );
        }
    }
    #[test]
    fn market_history_is_on_demand_and_nonpersistent() {
        for retired in ["crates/local_history", "crates/local_storage"] {
            assert!(
                !repository_root().join(retired).exists(),
                "retired market-history persistence boundary {retired} returned"
            );
        }
        let market = manifest("crates/market_runtime/Cargo.toml");
        assert!(
            market.contains("aeris_provider_history"),
            "market runtime lost provider-history adapter boundary"
        );
        assert!(
            !market.contains("aeris_local_storage"),
            "market runtime must not persist market history"
        );
        let module = manifest("crates/market_runtime/src/market_service/mod.rs");
        assert!(
            module.contains("INITIAL_HISTORY_BARS")
                && module.contains("MAXIMUM_HISTORY_BARS_PER_REQUEST")
        );
        let history = manifest("crates/market_runtime/src/market_service/history.rs");
        assert!(
            history.contains("historical_backfill_is_retained_beyond_one_provider_page_limit"),
            "market runtime lost untruncated back-scroll retention regression"
        );
    }

    #[test]
    fn every_runtime_provider_is_mapped_by_the_desktop_or_explicitly_hidden() {
        // A provider id the desktop does not map must never fall back to another
        // provider's presentation; the desktop test enumerates the runtime registry.
        let desktop_tests = manifest("apps/desktop/src/desktop/tests.rs");
        assert!(
            desktop_tests
                .contains("fn every_built_in_provider_maps_to_a_desktop_provider_or_is_hidden")
                && desktop_tests.contains("HIDDEN_RUNTIME_PROVIDERS"),
            "desktop lost the runtime provider mapping regression"
        );
        let desktop = manifest("apps/desktop/src/desktop.rs");
        assert!(
            !desktop.contains("fn terminal_provider_from_id"),
            "unknown provider ids must not fall back to a default desktop provider"
        );
    }
    #[test]
    fn in_process_market_contracts_remain_explicit_and_provider_neutral() {
        let demand = manifest("crates/market_engine/src/demand.rs");
        assert!(
            demand.contains("pub(crate) fn set_viewport")
                && demand.contains("StaleConsumerGeneration")
        );
        let engine_core = manifest("crates/market_engine/src/lib.rs");
        for regression in [
            "provider_and_viewport_generations_are_exactly_fenced",
            "provider_configuration_routes_only_supported_generation_fenced_requests",
            "shared_subscriptions_ref_count_streams_and_release_only_the_last_consumer",
        ] {
            assert!(
                engine_core.contains(regression),
                "market engine lost regression {regression}"
            );
        }
        let coordinator = manifest("crates/market_runtime/src/market_service/coordinator.rs");
        for regression in [
            "provider_connected_after_history_promotes_rithmic_series_live",
            "provider_connected_after_history_promotes_hyperliquid_series_live",
            "fresh_depth_demand_publishes_identified_awaiting_snapshot_frame",
        ] {
            assert!(
                coordinator.contains(regression),
                "market runtime lost initialization regression {regression}"
            );
        }
    }
    #[test]
    fn provider_runtime_registry_remains_the_single_engine_dispatch_boundary() {
        let shared_state = manifest("crates/market_runtime/src/market_service/mod.rs");
        let runtime = manifest("crates/market_runtime/src/market_service/runtime.rs");
        let production = format!(
            "{}\n{}",
            production_prefix(&shared_state),
            production_prefix(&runtime)
        );
        for contract in [
            "struct ProviderRuntimeRegistry",
            "records: BTreeMap<&'static str, ProviderRuntimeRecord>",
            "struct ProviderRuntimeRecord",
            "history: SyncSender<HistoryRequest>",
            "cancellation: Arc<AtomicBool>",
            "lifecycle: Arc<ProviderRuntimeLifecycle>",
            "workers: Vec<thread::JoinHandle<()>>",
            "impl ProviderRuntimeRegistry",
        ] {
            assert!(
                production.contains(contract),
                "market runtime registry lost {contract}"
            );
        }
        for root in ["apps/desktop/src", "crates/market_engine/src", "crates/ui"] {
            for path in production_sources_under(root) {
                let contents = fs::read_to_string(&path).expect("source");
                let production = production_prefix(&contents);
                for boundary in [
                    "hyperliquid_market_adapter::",
                    "rithmic_protocol_adapter::",
                    "tastytrade_market_adapter::",
                    "ctrader_open_api_adapter::",
                ] {
                    assert!(
                        !production.contains(boundary),
                        "{} bypasses market_runtime through {boundary}",
                        relative_string(&path)
                    );
                }
            }
        }
    }
    #[test]
    fn market_service_responsibilities_remain_decomposed_under_one_coordinator() {
        let root = "crates/market_runtime/src/market_service";
        let module = manifest(&format!("{root}/mod.rs"));
        for owner in [
            "coordinator",
            "history",
            "instrument_selection",
            "publication",
            "realtime",
            "runtime",
        ] {
            assert!(
                module.contains(&format!("mod {owner};")),
                "market service lost responsibility owner {owner}"
            );
        }
        assert!(
            manifest(&format!("{root}/coordinator.rs"))
                .contains("pub(super) struct Coordinator<'a>")
        );
        for path in [
            "history.rs",
            "instrument_selection.rs",
            "publication.rs",
            "realtime.rs",
            "provider_event.rs",
            "runtime.rs",
        ] {
            let manifest = manifest(&format!("{root}/{path}"));
            let source = production_prefix(&manifest);
            assert!(
                source.lines().any(|line| {
                    let trimmed = line.trim_start();
                    trimmed.starts_with("fn ")
                        || trimmed.starts_with("pub(")
                        || trimmed.starts_with("struct ")
                        || trimmed.starts_with("enum ")
                        || trimmed.starts_with("impl ")
                }),
                "market-service owner {path} has no production item"
            );
        }
        let realtime = manifest(&format!("{root}/realtime.rs"));
        for superseded in [
            "fn handle_rithmic_realtime(",
            "fn handle_hyperliquid_realtime(",
            "fn hyperliquid_candle(",
        ] {
            assert!(
                !realtime.contains(superseded),
                "parallel provider event handler remains: {superseded}"
            );
        }
        let tastytrade = manifest(&format!("{root}/tastytrade.rs"));
        assert!(
            !production_prefix(&tastytrade).contains("impl Coordinator"),
            "tastytrade provider module owns coordinator behavior"
        );
        let coordinator_source = manifest(&format!("{root}/coordinator.rs"));
        let coordinator = production_prefix(&coordinator_source);
        assert!(coordinator.contains("series: lifecycle::SeriesLifecycles::default()"));
        for superseded in ["rithmic_live:", "candle_live:"] {
            assert!(
                !coordinator.contains(superseded),
                "parallel live-series map remains: {superseded}"
            );
        }
        for path in [
            "coordinator.rs",
            "history.rs",
            "instrument_selection.rs",
            "publication.rs",
            "realtime.rs",
            "runtime.rs",
        ] {
            let source = manifest(&format!("{root}/{path}"));
            let production = production_prefix(&source);
            for branch in ["provider_id == \"", "provider_id != \""] {
                assert!(
                    !production.contains(branch),
                    "neutral market-service owner {path} branches on provider identity: {branch}"
                );
            }
        }
    }
    #[test]
    fn series_lifecycle_boundary_accepts_owner_and_injected_clock() {
        assert!(!super::violates_series_lifecycle_boundary(
            "crates/market_runtime/src/market_service/coordinator.rs",
            "struct Coordinator { series: lifecycle::SeriesLifecycles }"
        ));
        assert!(!super::violates_series_lifecycle_boundary(
            "crates/market_runtime/src/market_service/lifecycle/mod.rs",
            "fn step(clock: &dyn Clock) { clock.monotonic(); }\n#[cfg(test)] mod tests { fn check() { Instant::now(); } }\nfn production_after_tests() { let _now = 1; }",
        ));
    }

    #[test]
    fn series_lifecycle_boundary_rejects_old_owner_handlers_and_wall_clock() {
        assert!(super::violates_series_lifecycle_boundary(
            "crates/market_runtime/src/market_service/coordinator.rs",
            "struct Coordinator { series_live: SeriesLiveMap }",
        ));
        assert!(super::violates_series_lifecycle_boundary(
            "crates/market_runtime/src/market_service/lifecycle/mod.rs",
            "#[cfg(test)] mod tests { fn check() {} }\nfn production_after_tests() { Instant::now(); }",
        ));
        assert!(super::violates_series_lifecycle_boundary(
            "crates/market_runtime/src/market_service/realtime.rs",
            "fn publish_candle_live() {}",
        ));
    }
    #[test]
    fn local_clients_versions_and_workspace_writes_remain_fenced() {
        let engine = manifest("crates/market_engine/src/lib.rs");
        for regression in [
            "disconnect_removes_only_the_owning_clients_consumers",
            "provider_and_viewport_generations_are_exactly_fenced",
            "visibility_changes_only_the_selected_workspace_consumer",
        ] {
            assert!(
                engine.contains(regression),
                "consumer generation fence lost {regression}"
            );
        }
        let local = manifest("apps/desktop/src/desktop/local_state.rs");
        assert!(local.contains("active_workspace_id") && local.contains("save_workspace"));
        let persistence = manifest("apps/desktop/src/desktop/workspace_persistence.rs");
        for contract in [
            "active_workspace_id",
            "shutdown_wait_observes_latest_active_workspace_durability_without_stealing_ui_result",
            "shutdown_wait_surfaces_current_persistence_failure",
            "shutdown_wait_is_captured_without_waiting_on_the_ui_owner",
            "shutdown_wait_rejects_a_newer_layout_request_before_commit",
        ] {
            assert!(
                persistence.contains(contract),
                "local workspace persistence lost {contract}"
            );
        }
        let workspace_tabs = manifest("apps/desktop/src/desktop/workspace_tabs.rs");
        assert!(
            !workspace_tabs.contains("persistence.flush(")
                && workspace_tabs
                    .contains("await_workspace_persistence(persistence.shutdown_wait())"),
            "window shutdown must hand workspace durability to the lifecycle owner without blocking GPUI"
        );
        let prepare_start = workspace_tabs
            .find("fn prepare_update_restart_after_workspace_persistence")
            .expect("update restart persistence preparation");
        let prepare_end = workspace_tabs[prepare_start..]
            .find("pub(super) fn update_presentation")
            .map(|offset| prepare_start + offset)
            .expect("update restart persistence preparation boundary");
        let prepare = &workspace_tabs[prepare_start..prepare_end];
        assert!(
            prepare.contains("wait.wait(Duration::from_secs(2))")
                && prepare.contains("chart_chrome_wait.wait(Duration::from_secs(2))")
                && prepare.contains("let durable_generations = durability.await")
                && prepare.contains("persistence.shutdown_generation_is_current(generation)")
                && prepare.contains("chart_chrome_shutdown_generation_is_current")
                && prepare.contains("commit_update_restart_after_persistence"),
            "update restart must await bounded generation-fenced workspace and chart durability receipts before commit"
        );
        let commit_start = workspace_tabs
            .find("fn commit_update_restart_after_persistence")
            .expect("update restart commit phase");
        let commit_end = workspace_tabs[commit_start..]
            .find("fn prepare_update_restart_after_workspace_persistence")
            .map(|offset| commit_start + offset)
            .expect("update restart commit boundary");
        let commit = &workspace_tabs[commit_start..commit_end];
        let restart = commit
            .find("updater.commit_restart()")
            .expect("prepared update restart commit");
        let close = commit
            .find("self.claim_close(cx)")
            .expect("update restart claims the shared close path");
        let quit = commit
            .find("self.lifecycle.quit_after_shutdown(cx)")
            .expect("update restart requests lifecycle quit");
        assert!(
            restart < close && close < quit,
            "update restart must commit only after durability, then claim shared close before lifecycle quit"
        );
        let update = manifest("apps/desktop/src/update.rs");
        let cleanup_start = update
            .find("impl Drop for PreparedRestart")
            .expect("prepared restart cleanup owner");
        let cleanup_end = update[cleanup_start..]
            .find("fn spawn_update_restart")
            .map(|offset| cleanup_start + offset)
            .expect("prepared restart cleanup boundary");
        let cleanup = &update[cleanup_start..cleanup_end];
        assert!(
            workspace_tabs.contains("drop(cleanup);")
                && cleanup.contains("aeris-update-restart-cleanup")
                && cleanup.contains(".spawn(move || {")
                && cleanup.contains("child.kill()")
                && cleanup.contains("child.wait()"),
            "failed update restart preparation must transfer helper termination and reaping off GPUI"
        );
        let lifecycle = manifest("apps/desktop/src/desktop/lifecycle.rs");
        assert!(
            lifecycle.contains("await_workspace_persistence")
                && lifecycle.contains("persistence.wait(Duration::from_secs(2))"),
            "desktop lifecycle must own the bounded off-UI workspace durability wait"
        );
        let local = manifest("apps/desktop/src/desktop/local_state.rs");
        assert!(local.contains("WorkspaceState::decode") && local.contains("encode_to_vec"));
        assert!(local.contains("LEGACY_WORKSPACE_FILE"));
    }

    #[test]
    fn account_refresh_rotation_remains_runtime_fenced() {
        let account = manifest("crates/account_runtime/src/account_service/mod.rs");
        let restore_start = account
            .find("fn restore_online_session")
            .expect("saved-session restore path");
        let restore_end = account[restore_start..]
            .find("fn restore_local_session")
            .map(|offset| restore_start + offset)
            .expect("saved-session restore boundary");
        let restore = &account[restore_start..restore_end];
        assert!(
            restore.contains("refresh_gate.lock()")
                && restore.contains("claim_refresh_grant()")
                && restore.contains("current_restore_refresh_token(&vault)")
                && restore.contains("accept_online_restore_refresh"),
            "startup restore must serialize refresh admission and reload durable token material after lifecycle quiescing"
        );

        let refresh_start = account
            .find("fn refresh_lease_round")
            .expect("scheduled/profile refresh path");
        let refresh_end = account[refresh_start..]
            .find("fn cached_outcome")
            .map(|offset| refresh_start + offset)
            .expect("refresh round boundary");
        let refresh = &account[refresh_start..refresh_end];
        let claim = refresh
            .find("claim_refresh_grant()")
            .expect("refresh grant lifecycle claim");
        let durable = refresh
            .find("accept_refresh_grant")
            .expect("rotated refresh durability point");
        let release = refresh
            .find("drop(grant_permit)")
            .expect("refresh grant lifecycle release");
        let profile = refresh
            .find("link_subject")
            .expect("post-rotation profile refresh");
        assert!(
            claim < durable && durable < release && release < profile,
            "refresh quiesce must cover only grant-through-durable-token rotation"
        );
    }

    #[test]
    fn account_refresh_rotation_remains_desktop_lifecycle_fenced() {
        let workspace_tabs = manifest("apps/desktop/src/desktop/workspace_tabs.rs");
        let prepare_start = workspace_tabs
            .find("fn prepare_update_restart_after_workspace_persistence")
            .expect("update restart preparation");
        let prepare_end = workspace_tabs[prepare_start..]
            .find("pub(super) fn update_presentation")
            .map(|offset| prepare_start + offset)
            .expect("update restart preparation boundary");
        let prepare = &workspace_tabs[prepare_start..prepare_end];
        assert!(
            prepare.contains("self.lifecycle.account_service().begin_refresh_quiesce()")
                && prepare.contains("account_refresh.wait()?")
                && prepare.contains("drop(account_refresh)"),
            "update restart must quiesce account refresh and release its claim on cancellation"
        );
        let update = manifest("apps/desktop/src/update.rs");
        let commit_start = update
            .find("pub fn commit_restart")
            .expect("update restart commit owner");
        let commit_end = update[commit_start..]
            .find("pub(super) fn cancel_prepared_restart")
            .map(|offset| commit_start + offset)
            .expect("update restart commit boundary");
        assert!(
            update[commit_start..commit_end].contains("self.prepared_restart = Some(prepared);")
                && workspace_tabs
                    .contains("self.cancel_update_restart_after_persistence_failure(error, cx)"),
            "failed restart commit must transfer helper cleanup off GPUI instead of dropping it inline"
        );

        let lifecycle = manifest("apps/desktop/src/desktop/lifecycle.rs");
        assert!(
            lifecycle.contains("self.account_service().begin_refresh_quiesce()")
                && lifecycle.contains(
                    "retain_account_refresh_quiesce_for_exit(account_refresh, \"desktop exit\")"
                )
                && lifecycle.contains("blocks_exit: true")
                && lifecycle.contains("quit_after_shutdown_attempt(cx)")
                && lifecycle.contains("duplicate request must not bypass its durability fences"),
            "normal desktop shutdown must await account refresh durability off GPUI and block unsafe exit"
        );
    }

    #[test]
    fn account_refresh_rotation_remains_native_session_fenced() {
        let desktop = manifest("apps/desktop/src/desktop.rs");
        let readiness_start = desktop
            .find("fn run_desktop_readiness_command")
            .expect("candidate readiness path");
        let readiness_end = desktop[readiness_start..]
            .find("struct LifecycleReadinessReport")
            .map(|offset| readiness_start + offset)
            .expect("candidate readiness path boundary");
        let readiness = &desktop[readiness_start..readiness_end];
        let service = readiness
            .find("AccountService::new(")
            .expect("readiness account owner");
        let readiness_guard = readiness
            .find("native_account_session_shutdown_guard(")
            .expect("readiness native session shutdown guard");
        let restore = readiness
            .find("account_service.start_restore()")
            .expect("readiness restore start");
        assert!(
            service < readiness_guard && readiness_guard < restore,
            "candidate account restoration must not start before native session shutdown fencing"
        );
        assert!(
            readiness.contains("retain_account_refresh_quiesce_for_exit(")
                && readiness.contains("account_service.begin_refresh_quiesce()"),
            "every candidate-readiness exit after account creation must drain refresh-token rotation"
        );

        let platform = manifest("crates/platform_runtime/src/session_shutdown.rs");
        assert!(
            platform.contains("pub struct NativeSessionShutdownGuard")
                && platform.contains("pub struct NativeSessionShutdownPermit")
                && platform.contains("WM_QUERYENDSESSION")
                && platform.contains("query_end_session()")
                && platform.contains("end_session(wparam != 0)"),
            "Windows session termination must be vetoable until account refresh durability settles"
        );
    }

    #[test]
    fn signed_transactional_lifecycle_remains_platform_owned() {
        let lifecycle = manifest("crates/platform_runtime/src/lifecycle.rs");
        for contract in [
            "key.verify(&canonical, &signature)",
            "current_version < minimum_version",
            "verify_candidate_inventory(&root, &signed.manifest.files)",
            "enum UpdateState",
            "pub fn recover<",
            "pub fn uninstall<",
            "native_installation_inventory",
            "remove_owned_path",
            "UpdatePendingCleanup",
        ] {
            assert!(
                lifecycle.contains(contract),
                "platform lifecycle lost {contract}"
            );
        }
        let launcher = manifest("crates/platform_runtime/src/bin/aeris_launcher.rs");
        for contract in [
            "--check-update",
            "--prepare-update",
            "--update-and-restart",
            "--desktop-readiness",
            "DesktopReadinessReport",
            "owned_process_is_running(&self.install_root)",
        ] {
            assert!(
                launcher.contains(contract),
                "stable launcher lost {contract}"
            );
        }
        assert!(
            launcher.contains("bootstrap_requires_remote_install(active_release.as_ref())"),
            "healthy installed startup must decide locally before remote update work"
        );
        let restart_start = launcher
            .find("fn update_and_restart(")
            .expect("update restart function");
        let restart_end = launcher[restart_start..]
            .find("\nstruct PreparedUpdate")
            .map(|offset| restart_start + offset)
            .expect("prepared update boundary");
        let restart = &launcher[restart_start..restart_end];
        let channel_recheck = restart
            .find("checked_release_channel")
            .expect("prepared restart channel recheck");
        let restart_ack = restart
            .find("announce_update_restart_ready")
            .expect("prepared restart acknowledgement");
        assert!(
            restart.contains("preflight_update_restart")
                && restart.contains("prepared.signed_release")
                && !restart.contains("install_remote_update")
                && !restart.contains("spawn_active_launcher_promotion")
                && channel_recheck < restart_ack
                && !restart[restart_ack..].contains("checked_release_channel"),
            "restart handoff must activate only an already prepared local release"
        );
        assert!(!launcher.contains("--launch-engine"));
        assert!(!launcher.contains("BackgroundService"));
        assert!(
            !repository_root()
                .join("crates/platform_runtime/src/background_service.rs")
                .exists()
        );
    }

    #[test]
    fn development_build_has_no_cloud_publisher_or_login_gate() {
        for path in [
            "tools/publish_release.ps1",
            ".github/workflows/release.yml",
            "crates/platform_runtime/src/bin/aeris_release_publisher.rs",
        ] {
            assert!(
                !repository_root().join(path).exists(),
                "retired publisher remains: {path}"
            );
        }
        let desktop = manifest("apps/desktop/src/desktop.rs");
        let run_start = desktop
            .find("pub(super) fn run(")
            .expect("desktop entrypoint");
        let run_end = desktop[run_start..]
            .find("fn schedule_versioned_launcher_promotion")
            .map(|offset| run_start + offset)
            .expect("entrypoint boundary");
        let run = &desktop[run_start..run_end];
        assert!(run.contains("run_desktop(configured, lifecycle)"));
        assert!(!run.contains("DesktopAccount::install()"));
        assert!(!run.contains("run_onboarding()"));
        let updater = manifest("apps/desktop/src/update.rs");
        assert!(updater.contains("const UPDATE_BACKEND_CONFIGURED: bool = false"));
        let account = manifest("apps/desktop/src/account.rs");
        assert!(account.contains("const AUTH_BACKEND_CONFIGURED: bool = cfg!(test)"));
    }

    #[test]
    fn launcher_rollout_and_lkg_startup_failover_remain_bounded() {
        let launcher = manifest("crates/platform_runtime/src/bin/aeris_launcher.rs");
        for contract in [
            "rollout_eligible",
            "current_update_check_report",
            "QUARANTINED_RELEASE_FILE",
            "failed release could not be quarantined",
            "EARLY_DESKTOP_STARTUP_WINDOW",
            "rollback_to_retained_known_good",
            "prepared_matches_current_offer",
            "&current.channel.signed_release == prepared",
            "retained.is_some()",
            "launcher_generation < active.install_generation",
            "spawn_desktop_release(installer, &retained, true)",
            "command.arg(\"--workspace-tabs\")",
        ] {
            assert!(launcher.contains(contract), "launcher lost {contract}");
        }
        let channel_validation = launcher
            .find("fn checked_release_channel(")
            .expect("checked channel function");
        let channel_tail = &launcher[channel_validation..];
        let signature = channel_tail
            .find("verify_release_manifest(")
            .expect("channel signature validation");
        let downgrade = channel_tail
            .find("release channel generation regressed below the active release")
            .expect("channel downgrade rejection");
        let rollout = channel_tail
            .find("release_offer_eligible(")
            .expect("channel rollout policy");
        assert!(
            signature < downgrade && downgrade < rollout,
            "rollout eligibility must be applied only after signature and downgrade validation"
        );
    }
    #[test]
    fn windows_install_shell_keeps_native_identity_and_signed_lifecycle_boundary() {
        for path in [
            "apps/desktop/src/main.rs",
            "crates/platform_runtime/src/bin/aeris_launcher.rs",
        ] {
            assert!(
                manifest(path).contains("windows_subsystem = \"windows\""),
                "Windows GUI binary {path} lost subsystem marker"
            );
        }
        let setup = manifest("tools/windows/aeris_setup.iss");
        for contract in [
            "PrivilegesRequired=lowest",
            "DefaultDirName={localappdata}\\Programs\\Aeris",
            "UninstallDisplayIcon={app}\\aeris_launcher.exe",
            "AppUserModelID: \"com.aeris.desktop\"",
            "Subkey: \"Software\\Classes\\AppUserModelId\\com.aeris.desktop\"; Flags: uninsdeletekey",
            "DestName: \"aeris_launcher.exe\"",
            "--install \"' + Manifest + '\" \"' + Bundle + '\"",
            "ValueName: \"Aeris Engine\"; Flags: deletevalue",
            "RollbackCompatibilityPath",
            "DestName: \"rollback-compatibility.json\"",
            "function PrepareToInstall(var NeedsRestart: Boolean): String;",
            "{localappdata}\\Programs\\.Aeris-lifecycle\\uninstall.json",
            "Finishing the previous Aeris Terminal uninstall...",
        ] {
            assert!(
                setup.contains(contract),
                "Windows installer lost {contract}"
            );
        }
        let pending_cleanup = setup
            .find("function PrepareToInstall")
            .expect("pending uninstall preflight");
        let install_files = setup
            .find("procedure CurStepChanged")
            .expect("signed release installation hook");
        assert!(
            pending_cleanup < install_files,
            "pending uninstall cleanup must run before the bundled release installation hook"
        );
        assert!(!setup.contains("EnginePath"));
    }
    #[test]
    fn platform_filesystem_assumptions_remain_explicitly_guarded() {
        let mut sources = Vec::new();
        for root in [
            "crates/platform_runtime/src",
            "crates/market_runtime/src",
            "crates/account_runtime/src",
            "apps/desktop/src",
        ] {
            sources.extend(production_sources_under(root));
        }
        assert_ne!(sources, [] as [std::path::PathBuf; 0]);
        for path in sources {
            let contents = fs::read_to_string(&path).expect("source");
            let production = production_prefix(&contents);
            let relative = relative_string(&path);
            if production.contains("std::os::unix") {
                assert!(
                    production.contains("cfg(unix)") || production.contains("cfg(target_os"),
                    "{relative} uses Unix APIs without a guard"
                );
            }
            if production.contains("std::os::windows") {
                assert!(
                    production.contains("cfg(windows)") || production.contains("cfg(target_os"),
                    "{relative} uses Windows APIs without a guard"
                );
            }
            for forbidden in [
                "/proc/",
                "\"/tmp",
                "/opt/aeris",
                "/Applications/",
                "C:\\Program",
                "C:/Program",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "{relative} hardcodes platform path {forbidden}"
                );
            }
        }
    }
    /// The required shape of the push/pull-request CI workflow (architecture
    /// section 3.10, naming rule 12). Returns one label per violation; an
    /// empty list means the workflow matches the hosted-runner gate shape.
    fn push_ci_shape_violations(workflow: &str) -> Vec<&'static str> {
        let mut violations = Vec::new();
        for trigger in ["push:", "pull_request:", "workflow_dispatch:"] {
            if !workflow.contains(trigger) {
                violations.push("CI must trigger on push, pull_request and workflow_dispatch");
            }
        }
        if !workflow.contains("branches: [main]") {
            violations.push("push and pull_request triggers must target main");
        }
        if workflow.contains("pull_request_target") {
            violations.push("pull_request_target hands fork code a write token and is forbidden");
        }
        if workflow.contains("self-hosted") {
            violations.push(
                "a pull_request workflow must never run on self-hosted runners (they hold credentials)",
            );
        }
        if !(workflow.contains("permissions:") && workflow.contains("contents: read")) {
            violations.push("CI must declare read-only permissions (contents: read)");
        }
        if !(workflow.contains("concurrency:") && workflow.contains("cancel-in-progress: true")) {
            violations.push("CI must cancel superseded runs (concurrency with cancel-in-progress)");
        }
        for runner in ["runs-on: windows-latest", "runs-on: ubuntu-latest"] {
            if !workflow.contains(runner) {
                violations
                    .push("CI must keep both hosted lanes (windows-latest and ubuntu-latest)");
            }
        }
        if workflow.contains("secrets.") {
            violations.push("public-repository CI must not reference secrets");
        }
        if workflow.matches("timeout-minutes:").count() < 2 {
            violations.push("every CI job must declare timeout-minutes");
        }
        violations
    }

    /// A workflow text with every required element of the hosted push/PR gate
    /// shape; negative tests mutate it one requirement at a time.
    const COMPLIANT_PUSH_CI: &str = r#"name: CI
# Kit-gated Rithmic code is skipped on hosted runners.
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  workflow_dispatch:
permissions:
  contents: read
concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true
env:
  AERIS_RITHMIC_KIT_DISABLED: "1"
jobs:
  windows:
    runs-on: windows-latest
    timeout-minutes: 120
    steps:
      - run: cargo fmt --all -- --check
  ubuntu:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - run: cargo run -p aeris_naming_check --locked
"#;

    #[test]
    fn push_ci_shape_accepts_a_compliant_hosted_workflow() {
        assert_eq!(
            push_ci_shape_violations(COMPLIANT_PUSH_CI),
            Vec::<&'static str>::new(),
            "a workflow with the required triggers, permissions, concurrency and hosted lanes must pass"
        );
    }

    #[test]
    fn push_ci_shape_rejects_pull_request_target() {
        let workflow = COMPLIANT_PUSH_CI.replacen(
            "  workflow_dispatch:",
            "  pull_request_target:\n    branches: [main]\n  workflow_dispatch:",
            1,
        );
        let violations = push_ci_shape_violations(&workflow);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("pull_request_target")),
            "a pull_request_target trigger must be rejected, got {violations:?}"
        );
    }

    #[test]
    fn push_ci_shape_rejects_a_self_hosted_pull_request_runner() {
        let workflow = COMPLIANT_PUSH_CI.replacen(
            "runs-on: windows-latest",
            "runs-on: [self-hosted, windows]",
            1,
        );
        let violations = push_ci_shape_violations(&workflow);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("self-hosted")),
            "a self-hosted runner on a pull_request workflow must be rejected, got {violations:?}"
        );
    }

    #[test]
    fn push_ci_shape_rejects_missing_read_only_permissions() {
        let workflow = COMPLIANT_PUSH_CI.replacen("permissions:\n  contents: read\n", "", 1);
        let violations = push_ci_shape_violations(&workflow);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("permissions")),
            "missing read-only permissions must be rejected, got {violations:?}"
        );
    }

    #[test]
    fn push_ci_shape_rejects_missing_concurrency() {
        let workflow = COMPLIANT_PUSH_CI.replacen(
            "concurrency:\n  group: ci-${{ github.ref }}\n  cancel-in-progress: true\n",
            "",
            1,
        );
        let violations = push_ci_shape_violations(&workflow);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("concurrency")),
            "missing concurrency cancellation must be rejected, got {violations:?}"
        );
    }

    #[test]
    fn hosted_push_ci_shape_remains_required() {
        let workflow = manifest(".github/workflows/ci.yml");
        assert_eq!(
            push_ci_shape_violations(&workflow),
            Vec::<&'static str>::new(),
            "ci.yml must keep the required hosted-runner push/pull-request shape"
        );
        // The header must document why hosted runners skip the Rithmic kit.
        assert!(
            workflow.contains("kit-gated Rithmic"),
            "the CI header must state that kit-gated Rithmic code is skipped on hosted runners"
        );
        // The windows-latest lane runs the full local gate in gate order.
        let (windows, ubuntu) = workflow
            .split_once("runs-on: ubuntu-latest")
            .expect("ci.yml keeps an ubuntu-latest lane after the windows-latest lane");
        let mut windows_offset = 0;
        for gate in [
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
            "cargo test --workspace --all-features --locked",
            "cargo run -p aeris_naming_check --locked",
        ] {
            let found = windows[windows_offset..]
                .find(gate)
                .unwrap_or_else(|| panic!("the windows-latest lane lost or reordered {gate}"));
            windows_offset += found + gate.len();
        }
        let mut ubuntu_offset = 0;
        for gate in [
            "cargo fmt --all -- --check",
            "cargo test -p aeris_naming_check --locked",
            "cargo run -p aeris_naming_check --locked",
        ] {
            let found = ubuntu[ubuntu_offset..]
                .find(gate)
                .unwrap_or_else(|| panic!("the ubuntu-latest lane lost or reordered {gate}"));
            ubuntu_offset += found + gate.len();
        }
    }

    #[test]
    fn hosted_push_ci_keeps_bounded_environment_and_pinned_actions() {
        let workflow = manifest(".github/workflows/ci.yml");
        // The toolchain comes from rust-toolchain.toml on every lane, and
        // hosted runs emulate a machine without the licensed provider kit.
        for (job, next) in [
            ("windows", Some("ubuntu")),
            ("ubuntu", Some("market_performance")),
            ("market_performance", None),
        ] {
            let section = workflow
                .split_once(&format!("  {job}:"))
                .unwrap_or_else(|| panic!("missing {job} CI job"))
                .1;
            let section = next
                .and_then(|next| section.split_once(&format!("\n  {next}:")))
                .map_or(section, |(current, _)| current);
            assert_eq!(
                section.matches("rustup show").count(),
                1,
                "{job} must install the pinned toolchain from rust-toolchain.toml"
            );
        }
        assert!(
            workflow
                .contains("market_performance:\n    if: github.event_name == 'workflow_dispatch'"),
            "market benchmarks must run only on a manual dispatch"
        );
        for environment in [
            "AERIS_RITHMIC_KIT_DISABLED: \"1\"",
            "CARGO_INCREMENTAL: \"0\"",
            "CARGO_PROFILE_DEV_DEBUG: line-tables-only",
        ] {
            assert!(
                workflow.contains(environment),
                "hosted lanes lost the bounded-build environment {environment}"
            );
        }
        // Every third-party action is pinned to a full 40-hex commit SHA.
        let uses_lines = workflow
            .lines()
            .filter(|line| line.contains("uses:"))
            .count();
        assert!(uses_lines > 0, "ci.yml must keep its checkout step");
        for line in workflow.lines().filter(|line| line.contains("uses:")) {
            let reference = line
                .split('@')
                .nth(1)
                .unwrap_or_else(|| panic!("action must pin a revision: {line}"));
            let sha: String = reference.chars().take(40).collect();
            assert!(
                sha.len() == 40
                    && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !reference
                        .chars()
                        .nth(40)
                        .is_some_and(|next| next.is_ascii_hexdigit()),
                "third-party action must pin a full 40-hex commit SHA: {line}"
            );
        }
        // The public chart dependency must not require runner credentials.
        assert!(
            !workflow.contains("NUCLEUS_CHARTS_TOKEN")
                && !workflow.contains("Authenticate private chart dependency")
                && !workflow.contains("GIT_CONFIG_GLOBAL")
                && !workflow.contains("git config --global"),
            "CI must fetch public Aeris Charts without credential rewriting"
        );
        // Stale pacing/evidence verifiers stay out of CI entirely. The
        // performance lane exists only under the dispatch-only job above.
        for retired_step in [
            "test_evidence_verifiers.ps1",
            "test_desktop_endurance_workflow.ps1",
        ] {
            assert!(
                !workflow.contains(retired_step),
                "the push/pull_request gate must not run the retired step {retired_step}"
            );
        }
        for production_release_contract in [
            "release-pair",
            "AERIS_RELEASE_IDENTITY",
            "AERIS_INSTALL_GENERATION",
            "AERIS_RELEASE_VERIFYING_KEY",
        ] {
            assert!(
                !workflow.contains(production_release_contract),
                "production release publication must stay out of GitHub Actions: found {production_release_contract}"
            );
        }
        assert!(
            !workflow.contains("continue-on-error"),
            "CI must keep platform-specific failures visible"
        );
    }

    #[test]
    fn hosted_macos_qualification_stays_manual_and_kitless() {
        // Hosted minutes are free only while the repository is public, so the
        // macOS lane must never queue on its own.
        let workflow = manifest(".github/workflows/macos.yml");
        assert!(workflow.contains("workflow_dispatch:") && workflow.contains("contents: read"));
        for trigger in ["push:", "schedule:", "workflow_run", "repository_dispatch"] {
            assert!(
                !workflow.contains(trigger),
                "hosted macOS qualification must stay manual-dispatch only, found {trigger}"
            );
        }
        assert_eq!(workflow.matches("runs-on:").count(), 1);
        assert!(workflow.contains("runs-on: macos-latest"));
        assert!(!workflow.contains("self-hosted"));
        for gate in [
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
            "cargo build --workspace --all-targets --all-features --locked",
            "cargo test --workspace --all-features --locked",
        ] {
            assert_eq!(
                workflow.matches(gate).count(),
                1,
                "the macOS lane must run {gate}"
            );
        }
        assert!(
            workflow.contains("AERIS_RITHMIC_KIT_DISABLED: \"1\"")
                && !workflow.contains("provider_kit")
                && !workflow.contains("RITHMIC_KIT_ROOT"),
            "public hosted runners must never receive the licensed Rithmic kit"
        );
        assert!(
            !workflow.contains("secrets.")
                && !workflow.contains("continue-on-error")
                && !workflow.contains("git config --global"),
            "the hosted macOS lane must stay credential-free and keep failures visible"
        );
    }

    #[test]
    fn macos_preview_build_stays_an_unpublished_manual_artifact() {
        let workflow = manifest(".github/workflows/macos_preview.yml");
        assert!(workflow.contains("workflow_dispatch:") && workflow.contains("contents: read"));
        for trigger in ["push:", "schedule:", "workflow_run", "repository_dispatch"] {
            assert!(
                !workflow.contains(trigger),
                "the macOS preview build must stay manual-dispatch only, found {trigger}"
            );
        }
        assert_eq!(workflow.matches("runs-on:").count(), 1);
        assert!(workflow.contains("runs-on: macos-latest"));
        assert!(
            workflow.contains("AERIS_RITHMIC_KIT_DISABLED: \"1\"")
                && !workflow.contains("provider_kit")
                && !workflow.contains("RITHMIC_KIT_ROOT"),
            "public hosted runners must never receive the licensed Rithmic kit"
        );
        // A preview is an expiring workflow artifact, never a published or
        // updater-visible release, and it is signed ad hoc without identities.
        for forbidden in [
            "secrets.",
            "gh release",
            "action-gh-release",
            "notarytool",
            "release-pair",
            "AERIS_RELEASE_IDENTITY",
            "AERIS_INSTALL_GENERATION",
            "AERIS_RELEASE_VERIFYING_KEY",
            "continue-on-error",
            "git config --global",
        ] {
            assert!(
                !workflow.contains(forbidden),
                "the macOS preview build must stay an unpublished artifact, found {forbidden}"
            );
        }
        assert!(workflow.contains("codesign --force --sign - --timestamp=none"));
        assert!(workflow.contains("codesign --verify --strict"));
        assert!(
            workflow.contains("hdiutil verify")
                && workflow.contains("macos-arm64-preview.dmg")
                && !workflow.contains(".zip"),
            "the macOS preview ships as one verified disk image"
        );
        assert!(workflow.contains("retention-days: 14"));
        for bundled in [
            "apps/desktop/packaging/macos/Info.plist",
            "apps/desktop/assets/aeris_assets/icons/desktop/macos/aeris.icns",
        ] {
            assert!(workflow.contains(bundled));
            assert!(
                repository_root().join(bundled).is_file(),
                "macOS bundle input {bundled} is missing"
            );
        }

        let info = manifest("apps/desktop/packaging/macos/Info.plist").replace("\r\n", "\n");
        for (key, value) in [
            ("CFBundleExecutable", "aeris_desktop"),
            ("CFBundleIdentifier", "com.aeris.desktop"),
            ("CFBundleIconFile", "aeris"),
            ("CFBundlePackageType", "APPL"),
        ] {
            assert!(
                info.contains(&format!("<key>{key}</key>\n\t<string>{value}</string>")),
                "macOS bundle {key} must be {value}"
            );
        }
        assert!(
            manifest("apps/desktop/Cargo.toml").contains("name = \"aeris_desktop\""),
            "the bundle executable must match the desktop binary"
        );
    }

    #[test]
    fn workflows_never_use_pull_request_target() {
        // Hosted lanes run untrusted fork code only as `pull_request`, where
        // the token is read-only and secrets are withheld. `pull_request_target`
        // would hand fork code a write token, so it is forbidden everywhere.
        let workflows = fs::read_dir(repository_root().join(".github/workflows"))
            .expect("workflow directory is readable")
            .map(|entry| entry.expect("workflow entry is readable").path())
            .collect::<Vec<_>>();
        assert_ne!(workflows, [] as [std::path::PathBuf; 0]);
        for path in workflows {
            let workflow = fs::read_to_string(&path).expect("workflow");
            assert!(
                !workflow.contains("pull_request_target"),
                "{} must never use pull_request_target",
                relative_string(&path)
            );
        }
    }

    fn self_hosted_automatic_trigger_violation(workflow: &str) -> bool {
        let mut in_triggers = false;
        let has_automatic_trigger = workflow.lines().any(|line| {
            let indentation = line.len() - line.trim_start().len();
            let normalized = line.trim().replace(['\'', '"'], "");
            if indentation == 0 {
                in_triggers = normalized == "on:" || normalized.starts_with("on: ");
            }
            if !in_triggers {
                return false;
            }
            if indentation == 0 && normalized.starts_with("jobs:") {
                in_triggers = false;
                return false;
            }
            let trigger_text = normalized
                .strip_prefix("on:")
                .unwrap_or(normalized.as_str());
            trigger_text
                .split(|character: char| {
                    character.is_ascii_whitespace()
                        || matches!(character, ':' | ',' | '[' | ']' | '{' | '}')
                })
                .any(|token| token == "push" || token == "pull_request")
        });
        workflow.contains("self-hosted") && has_automatic_trigger
    }

    #[test]
    fn self_hosted_workflow_rule_rejects_push_and_pull_request_fixtures() {
        for trigger in ["push:", "pull_request:"] {
            let workflow =
                format!("on:\n  {trigger}\njobs:\n  gate:\n    runs-on: [self-hosted, linux]\n");
            assert!(
                self_hosted_automatic_trigger_violation(&workflow),
                "self-hosted workflow with {trigger} must be rejected"
            );
        }
        for workflow in [
            "on: [push, workflow_dispatch]\njobs:\n  gate:\n    runs-on: self-hosted\n",
            "on:\n  'pull_request': {}\njobs:\n  gate:\n    runs-on: self-hosted\n",
        ] {
            assert!(self_hosted_automatic_trigger_violation(workflow));
        }
        assert!(!self_hosted_automatic_trigger_violation(
            "on:\n  workflow_dispatch:\njobs:\n  gate:\n    runs-on: [self-hosted, linux]\n"
        ));
    }

    #[test]
    fn self_hosted_workflows_are_never_triggered_by_push_or_pull_request() {
        for entry in fs::read_dir(repository_root().join(".github/workflows"))
            .expect("workflow directory is readable")
        {
            let path = entry.expect("workflow entry is readable").path();
            let workflow = fs::read_to_string(&path).expect("workflow");
            assert!(
                !self_hosted_automatic_trigger_violation(&workflow),
                "{} uses a self-hosted runner and must remain manual-dispatch only",
                relative_string(&path)
            );
        }
    }

    #[test]
    fn live_market_gates_use_public_chart_source() {
        let workflow = manifest(".github/workflows/live_market_gates.yml");
        assert!(
            !workflow.contains("NUCLEUS_CHARTS_TOKEN")
                && !workflow.contains("Authenticate private chart dependency")
                && !workflow.contains("GIT_CONFIG_GLOBAL"),
            "live-market gates must fetch public Aeris Charts without chart credentials"
        );
        assert!(
            !workflow.contains("continue-on-error"),
            "live-market gates must keep venue failures visible"
        );
        assert!(
            !workflow.contains("git config --global"),
            "live-market gates must not rewrite the runner owner's persistent git configuration"
        );
        assert_eq!(
            workflow.matches("Restore licensed Rithmic kit").count(),
            1,
            "the credentialed Rithmic gate must restore the licensed kit before building"
        );
        assert_eq!(
            workflow.matches("Require Rithmic provider kit").count(),
            1,
            "the credentialed Rithmic gate must fail fast when the kit is absent"
        );
    }

    #[test]
    fn live_market_gates_keep_candidate_bound_evidence_producers() {
        let workflow = manifest(".github/workflows/live_market_gates.yml");
        assert!(
            workflow.contains("live_worker_cancellation_is_prompt")
                && workflow.contains("live_market_gate_hyperliquid.*"),
            "Hyperliquid live workflow must execute and upload its self-recording gate"
        );
        assert!(
            workflow.contains("--bin rithmic_test_smoke")
                && workflow.contains("live_market_gate_rithmic.*"),
            "Rithmic live workflow must execute and upload its self-recording gate"
        );

        let hyperliquid = manifest("crates/market_runtime/src/hyperliquid_realtime.rs");
        assert!(
            hyperliquid.contains("LiveMarketGateRecorder::start(\"hyperliquid\")")
                && hyperliquid.contains("live Hyperliquid worker never connected")
                && hyperliquid.contains("live Hyperliquid worker never published a candle")
                && hyperliquid
                    .contains("connected, published a live candle, and cancelled promptly"),
            "Hyperliquid live gate must bind evidence to a real connection and live payload"
        );

        let rithmic = manifest("crates/adapters/rithmic_protocol/src/bin/rithmic_test_smoke.rs");
        assert!(
            rithmic.contains("LiveMarketGateRecorder::start(\"rithmic\")")
                && rithmic.contains(
                    "live trades, quotes, depth, history, heartbeat, and reconnect passed"
                ),
            "Rithmic default smoke must retain candidate-bound evidence recording"
        );
    }

    #[test]
    fn desktop_transition_capture_command_matches_tool_contract() {
        assert!(
            !repository_root()
                .join("apps/desktop/src/transition_capture.rs")
                .exists(),
            "retired transition-capture producer returned"
        );
        assert!(
            !repository_root()
                .join("tools/run_native_transition_capture.ps1")
                .exists(),
            "retired engine-era transition runner returned"
        );
        assert!(!manifest("apps/desktop/src/desktop.rs").contains("--capture-native-transitions"));
    }
    #[test]
    fn launcher_uninstall_relocates_outside_install_root() {
        let launcher = manifest("crates/platform_runtime/src/bin/aeris_launcher.rs");
        // The launcher runs from inside the tree uninstall deletes, and
        // Windows refuses to delete a running executable: uninstall must
        // rename the image to a sibling staging dir first so the original
        // tree can be removed synchronously with an honest exit code.
        for marker in [
            "relocate_running_binary",
            "remove_relocated_binary",
            "uninstall-stage",
        ] {
            assert!(
                launcher.contains(marker),
                "launcher uninstall lost its out-of-tree relocation {marker}"
            );
        }
    }

    #[test]
    fn rithmic_logins_stay_in_the_native_vault() {
        // Every Rithmic login, Test or production, is read from and written to
        // the one native-vault entry; none may arrive through the environment,
        // arguments, CI or source.
        let readers = [
            "crates/market_runtime/src/rithmic_realtime.rs",
            "crates/market_runtime/src/rithmic_history.rs",
            "crates/market_runtime/src/market_service/rithmic_login.rs",
            "crates/adapters/rithmic_protocol/src/bin/rithmic_test_smoke.rs",
            "crates/adapters/rithmic_protocol/src/bin/provision_rithmic_test.rs",
        ];
        for path in readers {
            assert!(
                manifest(path).contains("RITHMIC_LOGIN_VAULT_KEY"),
                "{path} lost the native-vault Rithmic login boundary"
            );
        }
        for path in readers
            .into_iter()
            .chain(["apps/desktop/src/components/rithmic_login.rs"])
            .chain([".github/workflows/live_market_gates.yml"])
        {
            let content = manifest(path);
            for forbidden in [
                "RITHMIC_PROD",
                "rithmic-prod",
                "RITHMIC_USER",
                "RITHMIC_PASSWORD",
                "production password",
                "prod password",
            ] {
                assert!(
                    !content.contains(forbidden),
                    "{path} takes Rithmic credentials outside the native vault ({forbidden})"
                );
            }
        }
        // The desktop hands a login to the market runtime and never opens the
        // vault or the adapter itself.
        let desktop = manifest("apps/desktop/src/components/rithmic_login.rs");
        for forbidden in ["NativeCredentialVault", "aeris_rithmic_protocol_adapter"] {
            assert!(
                !desktop.contains(forbidden),
                "the desktop Rithmic login reaches past the market runtime ({forbidden})"
            );
        }
    }
    #[test]
    fn live_market_gates_stay_on_self_hosted_runners() {
        let workflow = manifest(".github/workflows/live_market_gates.yml");
        for runner in [
            "runs-on: [self-hosted, axiusflow, linux]",
            "runs-on: [self-hosted, rithmic-credentials]",
        ] {
            assert!(
                workflow.contains(runner),
                "live-market gates lost the required gate {runner}"
            );
        }
        assert!(
            workflow.contains("AXIUSFLOW_RITHMIC_KIT_ROOT")
                && workflow.contains("AERIS_RITHMIC_KIT_ROOT")
                && workflow.contains("C:\\axiusflow-deps\\provider-kit"),
            "live-market gates must keep the provisioned Rithmic kit location and accept both env names"
        );
        for hosted in ["ubuntu-latest", "windows-latest", "macos-latest"] {
            assert!(
                !workflow.contains(&format!("runs-on: {hosted}")),
                "live-market gates must stay on zero-cost self-hosted runners, found {hosted}"
            );
        }
    }

    #[test]
    fn rithmic_application_name_has_one_canonical_source() {
        let adapter = manifest("crates/adapters/rithmic_protocol/src/lib.rs");
        assert!(adapter.contains("pub const RITHMIC_APPLICATION_NAME: &str = \"Aeris\";"));
        for relative in [
            "crates/adapters/rithmic_protocol/src/provider_session.rs",
            "crates/adapters/rithmic_protocol/src/protocol.rs",
            "crates/adapters/rithmic_protocol/src/session_tests.rs",
            "crates/adapters/rithmic_protocol/src/bin/rithmic_test_smoke.rs",
            "crates/market_runtime/src/rithmic_realtime.rs",
            "crates/market_runtime/src/rithmic_history.rs",
        ] {
            let source = manifest(relative);
            assert!(
                !source.contains("\"Aeris\""),
                "{relative} reintroduced non-canonical identity"
            );
            assert!(
                source.contains("RITHMIC_APPLICATION_NAME"),
                "{relative} must use shared Rithmic application name"
            );
        }
    }
    #[test]
    fn phase_five_account_surface_remains_bounded() {
        const ACCOUNT_ALLOWED_PREFIXES: &[&str] = &[
            "crates/domain/account/src/",
            "crates/account_runtime/src/",
            "crates/contracts/src/account.rs",
            "crates/contracts/src/lib.rs",
            "crates/contracts/src/messages.rs",
            "crates/contracts/tests/",
            "crates/platform_runtime/src/browser.rs",
            "apps/desktop/src/account.rs",
            "apps/desktop/src/desktop.rs",
            // Trading command adapters may construct provider-neutral trading account IDs;
            // they do not own SaaS identity or account-runtime state.
            "apps/desktop/src/trading.rs",
            // Trading accounts are a separate provider-neutral surface owned by
            // trading_runtime; do not confuse TradingAccountId with SaaS identity.
            "crates/domain/trading/src/",
            "crates/trading_runtime/src/",
        ];
        const ACCOUNT_IDENTIFIERS: &[&str] = &[
            "AccountId",
            "PlanId",
            "FeatureId",
            "FeatureSet",
            "BeginLogin",
            "AccountView",
            "LoginAuthorization",
            "AccountSessionState",
        ];
        for path in production_rust_sources() {
            let relative = relative_string(&path);
            let contents = fs::read_to_string(&path).expect("source");
            let production = production_prefix(&contents);
            let allowed = ACCOUNT_ALLOWED_PREFIXES
                .iter()
                .any(|prefix| relative.starts_with(prefix));
            if !allowed {
                for identifier in ACCOUNT_IDENTIFIERS {
                    assert!(
                        !contains_identifier(&production, identifier),
                        "{relative} owns account surface {identifier} outside its boundary"
                    );
                }
                for identifier in ["pkce", "PKCE", "openid", "oidc", "OIDC"] {
                    assert!(
                        !production.contains(identifier),
                        "{relative} owns account surface {identifier} outside its boundary"
                    );
                }
            }
            for forbidden in [
                "better_auth",
                "better-auth",
                "passkey",
                "Passkey",
                "webauthn",
                "Stripe",
                "stripe::",
                "Dodo",
                "dodo::",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "{relative} adds cloud identity/billing surface {forbidden}"
                );
            }
        }
        let coordinator = manifest("crates/market_runtime/src/market_service/coordinator.rs");
        for identifier in ["BeginLogin", "AccountView", "LoginAuthorization"] {
            assert!(
                !production_prefix(&coordinator).contains(identifier),
                "market coordinator must not own account state {identifier}"
            );
        }
        assert!(manifest("apps/desktop/Cargo.toml").contains("aeris_account_runtime"));
        assert!(!manifest("crates/ui/chart_integration/Cargo.toml").contains("aeris_account"));
    }
    #[test]
    fn trading_owner_and_store_remain_single_process_and_single_owner() {
        let workspace = manifest("Cargo.toml");
        for member in ["crates/domain/trading", "crates/trading_runtime"] {
            assert!(workspace.contains(member), "workspace lost {member}");
        }

        let domain = manifest("crates/domain/trading/Cargo.toml");
        for forbidden in [
            "rusqlite",
            "aeris_market_runtime",
            "aeris_account_runtime",
            "gpui",
        ] {
            assert!(
                !domain.contains(forbidden),
                "canonical trading domain depends on {forbidden}"
            );
        }

        let runtime_manifest = manifest("crates/trading_runtime/Cargo.toml");
        for required in ["aeris_trading", "aeris_instruments", "rusqlite.workspace"] {
            assert!(
                runtime_manifest.contains(required),
                "trading owner lost {required}"
            );
        }
        for forbidden in ["aeris_market_runtime", "aeris_account_runtime", "gpui"] {
            assert!(
                !runtime_manifest.contains(forbidden),
                "trading owner depends in the wrong direction on {forbidden}"
            );
        }

        let runtime = manifest("crates/trading_runtime/src/core/mod.rs");
        for contract in [
            "pub struct TradingService",
            "mpsc::sync_channel(COMMAND_CAPACITY)",
            "MAXIMUM_OPEN_ORDERS",
            "MAXIMUM_SNAPSHOT_ITEMS",
            "AccountEnvironment::Simulated",
        ] {
            assert!(runtime.contains(contract), "trading owner lost {contract}");
        }
        let store = [
            "schema.rs",
            "mod.rs",
            "orders.rs",
            "fills.rs",
            "ledger.rs",
            "groups.rs",
            "risk.rs",
            "user_records.rs",
            "retention.rs",
            "export.rs",
        ]
        .into_iter()
        .map(|file| manifest(&format!("crates/trading_runtime/src/store/{file}")))
        .collect::<String>();
        for contract in [
            "journal_mode\", \"WAL",
            "user_version",
            "CREATE TABLE order_events",
            "CREATE TABLE fills",
            "CREATE TABLE positions",
            "CREATE TABLE user_records",
            "maximum_user_records_per_kind",
            "executions.csv",
            "user_records.json",
        ] {
            assert!(store.contains(contract), "trading store lost {contract}");
        }

        for path in production_rust_sources() {
            let relative = relative_string(&path);
            let source = fs::read_to_string(&path).expect("source");
            if source.contains("rusqlite") {
                assert!(
                    relative.starts_with("crates/trading_runtime/src/"),
                    "{relative} introduced a second embedded-store owner"
                );
            }
        }

        for manifest_path in [
            "crates/market_runtime/Cargo.toml",
            "crates/account_runtime/Cargo.toml",
            "crates/ui/chart_integration/Cargo.toml",
            "crates/ui/terminal_ui/Cargo.toml",
        ] {
            assert!(
                !manifest(manifest_path).contains("aeris_trading_runtime"),
                "{manifest_path} bypasses the desktop trading command boundary"
            );
        }
        let desktop = manifest("apps/desktop/src/desktop.rs");
        assert!(desktop.contains("start_trading_service"));
        assert!(desktop.contains("DesktopServices::new(market, trading.clone(), account_service)"));
        assert!(
            manifest("apps/desktop/src/desktop/lifecycle.rs")
                .contains("trading.shutdown(Duration::from_secs(2))")
        );
        assert!(
            manifest("AGENTS.md")
                .contains("`trading_runtime` is the single in-process owner of broker accounts")
        );
    }

    #[test]
    fn injected_desktop_trading_accepts_instance_handles_and_test_only_globals() {
        assert!(!super::has_process_global_desktop_trading(
            "fn app(services: &DesktopServices) { services.trading.status(); }\n\
             #[cfg(test)] mod tests { static TRADING_SERVICE: u8 = 0; }\n"
        ));
    }

    #[test]
    fn injected_desktop_trading_rejects_global_handles_after_test_modules() {
        assert!(super::has_process_global_desktop_trading(
            "fn before() {}\n#[cfg(test)] mod tests { fn hidden() {} }\n\
             fn after() { aeris_desktop::trading::handle(); }\n"
        ));
        assert!(super::has_process_global_desktop_trading(
            "static TRADING_SERVICE: OnceLock<TradingService> = OnceLock::new();"
        ));
    }

    #[test]
    fn injected_desktop_feedback_accepts_instance_status_and_test_only_global() {
        assert!(!super::has_process_global_desktop_trading(
            "fn status(services: &DesktopServices) { services.trading_feedback.latest(); }\n\
             #[cfg(test)] mod tests { static TRADING_FEEDBACK: u8 = 0; }\n"
        ));
    }

    #[test]
    fn injected_desktop_feedback_rejects_process_global_after_test_module() {
        assert!(super::has_process_global_desktop_trading(
            "#[cfg(test)] mod tests { fn hidden() {} }\n\
             fn after() { aeris_desktop::trading::latest_feedback(); }\n"
        ));
        assert!(super::has_process_global_desktop_trading(
            "static TRADING_FEEDBACK: OnceLock<Mutex<TradingFeedbackState>> = OnceLock::new();"
        ));
    }

    #[test]
    fn desktop_trading_service_has_no_process_global_lookup() {
        for path in production_sources_under("apps/desktop/src") {
            let source = fs::read_to_string(&path).expect("desktop source");
            assert!(
                !super::has_process_global_desktop_trading(&source),
                "{} bypasses injected desktop trading",
                relative_string(&path)
            );
        }
    }

    #[test]
    fn injected_desktop_account_accepts_instance_handles_and_test_only_globals() {
        assert!(!super::has_process_global_desktop_account(
            "fn account(services: &DesktopServices) { services.account_service.account_status(); }\n\
             #[cfg(test)] mod tests { static INSTALLED_ACCOUNT: u8 = 0; }\n"
        ));
    }

    #[test]
    fn injected_desktop_account_rejects_globals_after_test_modules() {
        assert!(super::has_process_global_desktop_account(
            "#[cfg(test)] mod tests { fn hidden() {} }\n\
             fn after() { DesktopAccount::shared(); }\n"
        ));
        assert!(super::has_process_global_desktop_account(
            "static SERVICE: OnceLock<AccountService> = OnceLock::new();"
        ));
    }

    #[test]
    fn desktop_account_service_has_no_process_global_lookup() {
        for path in production_sources_under("apps/desktop/src") {
            let source = fs::read_to_string(&path).expect("desktop source");
            assert!(
                !super::has_process_global_desktop_account(&source),
                "{} bypasses injected desktop account",
                relative_string(&path)
            );
        }
    }

    #[test]
    fn injected_desktop_market_accepts_instance_owner_and_test_only_global() {
        assert!(!super::has_process_global_desktop_market(
            "fn app(services: &DesktopServices) { services.market.wake_bridges(); }\n\
             #[cfg(test)] mod tests { static MARKET_RUNTIME: u8 = 0; }\n"
        ));
    }

    #[test]
    fn injected_desktop_market_rejects_global_owner_after_test_module() {
        assert!(super::has_process_global_desktop_market(
            "#[cfg(test)] mod tests { fn hidden() {} }\n\
             fn after() { shared_market_runtime(); }\n"
        ));
        assert!(super::has_process_global_desktop_market(
            "static MARKET_RUNTIME: OnceLock<MarketService> = OnceLock::new();"
        ));
    }

    #[test]
    fn desktop_market_has_no_process_global_lookup() {
        for path in production_sources_under("apps/desktop/src") {
            let source = fs::read_to_string(&path).expect("desktop source");
            assert!(
                !super::has_process_global_desktop_market(&source),
                "{} bypasses injected desktop market",
                relative_string(&path)
            );
        }
    }
    #[test]
    fn trading_core_owner_shell_has_one_command_and_deadline_boundary() {
        let runtime = manifest("crates/trading_runtime/src/core/mod.rs");
        for contract in [
            "struct TradingCore",
            "fn handle(&mut self, command: Command, now: i64)",
            "fn on_market(&mut self, batch: Vec<SimulatedMarketObservation>, now: i64)",
            "fn on_timer(&mut self, now: i64)",
        ] {
            assert!(runtime.contains(contract), "trading core lost {contract}");
        }
        let service = manifest("crates/trading_runtime/src/service/mod.rs");
        assert!(service.contains("pub(crate) enum Command"));
        let owner = manifest("crates/trading_runtime/src/service/owner.rs");
        for contract in [
            "aeris-trading-owner",
            "recv_timeout",
            "core.handle(",
            "core.on_timer(",
        ] {
            assert!(
                owner.contains(contract),
                "trading owner shell lost {contract}"
            );
        }
    }
    #[test]
    fn context_data_remains_one_bounded_runtime_owned_boundary() {
        assert!(manifest("Cargo.toml").contains("crates/context_runtime"));
        let runtime_manifest = manifest("crates/context_runtime/Cargo.toml");
        for required in [
            "aeris_platform_runtime",
            "ureq.workspace",
            "zeroize.workspace",
        ] {
            assert!(
                runtime_manifest.contains(required),
                "context owner lost {required}"
            );
        }
        for forbidden in [
            "aeris_market_runtime",
            "aeris_trading_runtime",
            "aeris_account_runtime",
            "gpui",
        ] {
            assert!(
                !runtime_manifest.contains(forbidden),
                "context owner depends in the wrong direction on {forbidden}"
            );
        }
        let runtime = manifest("crates/context_runtime/src/lib.rs");
        for contract in [
            "pub struct ContextService",
            "mpsc::sync_channel(COMMAND_CAPACITY)",
            "aeris-context-runtime",
            "MAXIMUM_CONTEXT_ITEMS",
            "NativeContextCredentialStore",
            "Zeroizing",
        ] {
            assert!(runtime.contains(contract), "context owner lost {contract}");
        }
        let desktop_manifest = manifest("apps/desktop/Cargo.toml");
        assert!(desktop_manifest.contains("aeris_context_runtime"));
        assert!(!desktop_manifest.contains("ureq"));
        assert!(
            manifest("apps/desktop/src/desktop/lifecycle.rs")
                .contains("context.shutdown(Duration::from_secs(2))")
        );
        let desktop = manifest("apps/desktop/src/desktop.rs");
        for endpoint in [
            "api.eia.gov",
            "api.stlouisfed.org",
            "api.weather.gov",
            "publicreporting.cftc.gov",
            "api.nass.usda.gov",
            "api.fas.usda.gov",
        ] {
            assert!(
                !desktop.contains(endpoint),
                "desktop bypasses context ownership through {endpoint}"
            );
        }
    }
    #[test]
    fn account_contracts_remain_plain_bounded_values() {
        let account = manifest("crates/contracts/src/account.rs");
        for contract in [
            "pub struct BeginLogin",
            "pub struct CancelLogin",
            "pub struct GetAccountStatus",
            "pub struct LoginAuthorization",
            "pub struct AccountView",
            "pub struct SignOut",
            "pub enum AccountSessionState",
        ] {
            assert!(
                account.contains(contract),
                "account DTO boundary lost {contract}"
            );
        }
        assert!(!account.contains("prost::Message"));
        assert!(!account.contains("#[prost"));

        for secret in [
            "refresh_token",
            "access_token",
            "id_token",
            "Bearer",
            "customer_id",
            "payment",
            "Stripe",
            "Dodo",
        ] {
            assert!(
                !account.contains(secret),
                "sanitized account DTO exposes {secret}"
            );
        }
        let messages = manifest("crates/contracts/src/messages.rs");
        let runtime_market = messages
            .split("/// Requests one bounded exact provider-instrument search")
            .nth(1)
            .expect("runtime market contract section exists");
        assert!(!runtime_market.contains("prost::Message"));
        assert!(!runtime_market.contains("#[prost"));
        assert!(messages.contains("Tags 6, 7, and 9-13 are permanently retired"));
        assert!(!messages.contains("pub struct Envelope"));
    }

    #[test]
    fn gpui_base_retains_only_the_desktop_behavior_surface() {
        let source = manifest("third_party/gpui_base/src/lib.rs");
        for retained in [
            "mod button;",
            "pub mod input;",
            "mod scrollbar;",
            "mod styled;",
            "mod switch;",
            "mod theme;",
            "pub fn init(cx: &mut App)",
        ] {
            assert!(
                source.contains(retained),
                "gpui_base lost retained item {retained}"
            );
        }
        for removed in [
            "mod dock;",
            "mod calendar;",
            "mod color_picker;",
            "mod date_picker;",
            "mod sheet;",
            "mod table;",
            "mod tree;",
            "mod text_selection;",
        ] {
            assert!(
                !source.contains(removed),
                "gpui_base retained pruned module {removed}"
            );
        }
        let manifest = manifest("third_party/gpui_base/Cargo.toml");
        for removed in [
            "chrono =",
            "futures =",
            "markdown =",
            "html5ever =",
            "syntect =",
        ] {
            assert!(
                !manifest.contains(removed),
                "gpui_base retained unused dependency {removed}"
            );
        }
    }
}
