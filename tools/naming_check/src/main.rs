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
];
const REPOSITORY_MARKDOWN_FILES: &[&str] = &["AGENTS.md"];
const PLATFORM_FILE_EXCEPTIONS: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "Dockerfile",
    "Readme",
    "rust-toolchain.toml",
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

    if violations.is_empty() {
        println!("snake_case naming check passed");
        return ExitCode::SUCCESS;
    }

    eprintln!("non-snake_case platform-owned paths:");
    for violation in violations {
        eprintln!("  {}", violation.display());
    }
    ExitCode::FAILURE
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
        } else if file_type.is_file() && !is_valid_file_name(&name) {
            violations.push(relative_to(root, &path));
        }
    }
    Ok(())
}

fn should_skip_directory(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || SKIPPED_DIRECTORIES.contains(&name.as_ref())
}

fn is_valid_file_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    if name.starts_with('.')
        || PLATFORM_FILE_EXCEPTIONS.contains(&name.as_ref())
        || REPOSITORY_MARKDOWN_FILES.contains(&name.as_ref())
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
    use std::{
        collections::BTreeSet,
        fs,
        path::{Path, PathBuf},
    };

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

    fn production_prefix(contents: &str) -> &str {
        if contents.starts_with("#![cfg(test)]") {
            return "";
        }
        for (index, _) in contents.match_indices("#[cfg(test)]") {
            let after_attribute = &contents[index + "#[cfg(test)]".len()..];
            let declaration = after_attribute
                .trim_start_matches(['\r', '\n'])
                .lines()
                .next()
                .unwrap_or_default();
            if declaration.starts_with("mod ") && declaration.ends_with(" {") {
                return &contents[..index];
            }
        }
        contents
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
        ] {
            assert_excludes(relative, &ui);
        }
        assert_excludes(
            "crates/ui/chart_integration/Cargo.toml",
            &[
                "aeris_rithmic_protocol_adapter",
                "aeris_hyperliquid_market_adapter",
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
    fn provider_wire_and_nucleus_boundaries_remain_isolated() {
        let rithmic_adapter = manifest("crates/adapters/rithmic_protocol/src/lib.rs");
        assert!(rithmic_adapter.contains("mod generated {"));
        assert!(!rithmic_adapter.contains("pub mod generated"));

        let root_manifest = manifest("Cargo.toml");
        let expected_source = "https://github.com/AerisTerminal/aeris-charts.git";
        let expected_revision = "1f75243a45bacdf54fe411c85006f603a5a7aa3e";
        for dependency in [
            "aeris_charts_engine",
            "aeris_charts_indicators",
            "aeris_charts_render",
            "aeris_charts_render_gpui",
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
            {
                assert_eq!(relative, "crates/ui/chart_integration/Cargo.toml");
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
            "crates/provider_history/tests/provider_history_conformance.rs",
            "crates/provider_history/tests/handoff_conformance.rs",
            "crates/contracts/tests/contracts.rs",
            "crates/market_runtime/src/market_service/tests.rs",
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
        assert!(main.contains("desktop::run()"));
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

    #[test]
    fn ci_cargo_packages_and_targets_exist_in_the_workspace() {
        // Validate executable targets as well as artifact names: an obsolete package
        // must fail here before a self-hosted lane reaches its performance step.
        let manifests = workspace_manifests();
        for workflow in [
            ".github/workflows/ci.yml",
            ".github/workflows/live_market_gates.yml",
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
        assert_dependencies_are("crates/market_engine/Cargo.toml", &["aeris_market_data"]);
        assert_dependencies_are("crates/domain/instruments/Cargo.toml", &[]);
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
    fn dead_code_suppressions_remain_at_external_decode_boundaries() {
        let allowed = BTreeSet::from(["crates/adapters/rithmic_protocol/src/lib.rs"]);

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
            "crates/market_runtime/src/market_service/mod.rs::HistorySource".to_string(),
            "crates/adapters/rithmic_protocol/src/history_adapter.rs::RithmicHistoryTransport"
                .to_string(),
            "crates/adapters/rithmic_protocol/src/provider_runtime.rs::ProviderSessionDriver"
                .to_string(),
            "crates/platform_runtime/src/credential_vault.rs::CredentialVault".to_string(),
            "crates/platform_runtime/src/credential_vault.rs::NativeCredentialBackend".to_string(),
            "crates/platform_runtime/src/lifecycle.rs::LifecycleHooks".to_string(),
            "crates/provider_history/src/model.rs::ProviderHistoryAdapter".to_string(),
        ]);
        assert_eq!(
            actual, expected,
            "production traits require an intentional provider/platform/security boundary"
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
    fn desktop_startup_catalog_resolution_stays_event_driven() {
        let source = manifest("apps/desktop/src/engine_market_worker/selection_commands.rs");
        for required in [
            "StartupResolution::Searching",
            "handle_startup_catalog_event",
            "market.search_provider_instruments",
            "market.select_provider_instrument",
        ] {
            assert!(
                source.contains(required),
                "startup catalog path lost {required}"
            );
        }
        for forbidden in [
            "STARTUP_CATALOG_RESOLUTION_TIMEOUT",
            "poll_market_event_until",
            "recv_timeout",
            "thread::sleep",
        ] {
            assert!(
                !source.contains(forbidden),
                "startup catalog path must not block the shared workspace worker through {forbidden}"
            );
        }
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
                for boundary in ["hyperliquid_market_adapter::", "rithmic_protocol_adapter::"] {
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
        for (path, contract) in [
            ("history.rs", "fn history_completed("),
            ("instrument_selection.rs", "fn handle_catalog_selection("),
            ("publication.rs", "fn recover_overflowed_series_queues("),
            ("realtime.rs", "fn rithmic_trade("),
            ("realtime.rs", "fn hyperliquid_candle("),
            ("runtime.rs", "impl ProviderRuntimeRegistry"),
        ] {
            assert!(
                manifest(&format!("{root}/{path}")).contains(contract),
                "market-service owner {path} lost {contract}"
            );
        }
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
            prepare.contains("aeris_desktop::account::begin_refresh_quiesce()")
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
            lifecycle.contains("aeris_desktop::account::begin_refresh_quiesce()")
                && lifecycle.contains("quiesce.wait()")
                && lifecycle.contains("quiesce.retain_until_process_exit()")
                && lifecycle.contains("blocks_exit: true")
                && lifecycle.contains("quit_after_shutdown_attempt(cx, false)")
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
            .find("pub(super) fn run()")
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
        assert!(!sources.is_empty());
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
    #[test]
    fn supported_os_deterministic_gates_remain_required() {
        let workflow = manifest(".github/workflows/ci.yml");
        for runner in [
            "runs-on: [self-hosted, axiusflow, linux]",
            "runs-on: [self-hosted, axiusflow, windows]",
        ] {
            assert!(
                workflow.contains(runner),
                "CI lost the required native gate {runner}"
            );
        }
        // The account carries no paid Actions quota: a GitHub-hosted runner
        // fails every lane at startup, so the matrix must stay self-hosted.
        for hosted in [
            "runs-on: ubuntu-latest",
            "runs-on: windows-latest",
            "runs-on: macos-latest",
        ] {
            assert!(
                !workflow.contains(hosted),
                "CI must stay on zero-cost self-hosted runners, found {hosted}"
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
        for gate in [
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings",
            "cargo build --workspace --all-targets --all-features --locked",
            "cargo test --workspace --all-features --locked",
        ] {
            assert_eq!(
                workflow.matches(gate).count(),
                2,
                "every native OS lane must run {gate}"
            );
        }
        for evidence_test in [
            "./tools/test_evidence_verifiers.ps1",
            "./tools/test_desktop_endurance_workflow.ps1",
        ] {
            assert_eq!(
                workflow.matches(evidence_test).count(),
                1,
                "the Windows lane must keep the current evidence-schema regression {evidence_test}"
            );
        }
        for artifact in [
            "market-data-performance-linux",
            "market-data-performance-windows",
        ] {
            assert!(
                workflow.contains(artifact),
                "CI lost provenance-bound market-data evidence {artifact}"
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
            !workflow.contains("workspace-macos")
                && !workflow.contains("runs-on: [self-hosted, axiusflow, macos]"),
            "deferred macOS qualification must not leave an unserviceable required lane queued"
        );
        assert!(
            !workflow.contains("continue-on-error"),
            "CI must keep platform-specific failures visible"
        );
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
    fn rithmic_live_surface_stays_on_test_credentials() {
        for (path, marker) in [
            (
                "crates/market_runtime/src/rithmic_realtime.rs",
                "RITHMIC_TEST_VAULT_KEY",
            ),
            (
                "crates/market_runtime/src/rithmic_history.rs",
                "RITHMIC_TEST_VAULT_KEY",
            ),
            (
                "crates/adapters/rithmic_protocol/src/bin/rithmic_test_smoke.rs",
                "RITHMIC_TEST_VAULT_KEY",
            ),
            (
                "crates/adapters/rithmic_protocol/src/bin/provision_rithmic_test.rs",
                "RITHMIC_TEST_VAULT_KEY",
            ),
        ] {
            assert!(
                manifest(path).contains(marker),
                "{path} lost test credential scope"
            );
        }
        for path in [
            "crates/market_runtime/src/rithmic_realtime.rs",
            "crates/market_runtime/src/rithmic_history.rs",
            "crates/adapters/rithmic_protocol/src/bin/rithmic_test_smoke.rs",
            ".github/workflows/live_market_gates.yml",
        ] {
            let content = manifest(path);
            for forbidden in [
                "RITHMIC_PROD",
                "rithmic-prod",
                "production password",
                "prod password",
            ] {
                assert!(
                    !content.contains(forbidden),
                    "{path} references production credentials ({forbidden})"
                );
            }
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
                for identifier in ACCOUNT_IDENTIFIERS
                    .iter()
                    .chain(["pkce", "PKCE", "openid", "oidc", "OIDC"].iter())
                {
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
}
