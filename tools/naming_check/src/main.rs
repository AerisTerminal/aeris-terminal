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
    "financial-charts",
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
        for dependency in forbidden {
            assert!(
                !contents.contains(dependency),
                "{relative} must not depend on {dependency}"
            );
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
        let ui = [
            "gpui",
            "axiusflow_chart_integration",
            "axiusflow_terminal_ui",
        ];
        for relative in [
            "crates/market_engine/Cargo.toml",
            "crates/domain/instruments/Cargo.toml",
            "crates/domain/market_data/Cargo.toml",
            "crates/local_storage/Cargo.toml",
            "crates/local_history/Cargo.toml",
            "crates/adapters/coinbase_market/Cargo.toml",
            "crates/adapters/rithmic_protocol/Cargo.toml",
        ] {
            assert_excludes(relative, &ui);
        }
        assert_excludes(
            "crates/ui/chart_integration/Cargo.toml",
            &[
                "axiusflow_coinbase_market_adapter",
                "axiusflow_rithmic_protocol_adapter",
            ],
        );
    }

    #[test]
    fn transitional_backend_crate_names_do_not_return() {
        let workspace = manifest("Cargo.toml");
        for (retired, replacement) in [
            ("desktop_history", "local_history"),
            ("desktop_storage", "local_storage"),
            ("local_engine_protocol", "engine_protocol"),
        ] {
            assert!(
                !repository_root().join("crates").join(retired).exists(),
                "retired crate directory crates/{retired} must not return"
            );
            assert!(
                !workspace.contains(retired),
                "workspace must not restore transitional crate {retired}"
            );
            assert!(
                repository_root()
                    .join("crates")
                    .join(replacement)
                    .join("Cargo.toml")
                    .is_file(),
                "replacement crate crates/{replacement} is missing"
            );
            assert!(
                workspace.contains(replacement),
                "workspace must retain replacement crate {replacement}"
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
            "provider_kit must not gain Axiusflow Rust/application source: {:?}",
            application_sources
                .iter()
                .map(|path| relative_string(path))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn provider_wire_and_nucleus_boundaries_remain_isolated() {
        let rithmic_adapter = manifest("crates/adapters/rithmic_protocol/src/lib.rs");
        assert!(
            rithmic_adapter.contains("mod generated {"),
            "Rithmic generated protobuf must remain owned by its adapter"
        );
        assert!(
            !rithmic_adapter.contains("pub mod generated"),
            "Rithmic generated protobuf must not be exported above the adapter boundary"
        );

        let root_manifest = manifest("Cargo.toml");
        let expected_source = "https://github.com/NucleusCharts/financial-charts.git";
        let expected_revision = "a5240184aad486d78801bb0446b66fc951f7f782";
        for dependency in [
            "nucleuscharts_engine",
            "nucleuscharts_render",
            "nucleuscharts_render_gpui",
        ] {
            assert!(
                root_manifest.contains(&format!(
                    "{dependency} = {{ git = \"{expected_source}\", rev = \"{expected_revision}\""
                )),
                "{dependency} must remain pinned to the approved Nucleus Charts revision"
            );
        }

        for retired in [
            concat!("Axiusflow-app/", "Ori", "gin_", "charts"),
            concat!("ori", "gin_", "engine ="),
            concat!("ori", "gin_", "render ="),
            concat!("ori", "gin_", "render_gpui ="),
        ] {
            assert!(
                !root_manifest.contains(retired),
                "retired chart dependency identity returned: {retired}"
            );
        }

        for path in workspace_manifests() {
            let relative = relative_string(&path);
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            if contents.contains("nucleuscharts_engine.workspace")
                || contents.contains("nucleuscharts_render.workspace")
                || contents.contains("nucleuscharts_render_gpui.workspace")
            {
                assert_eq!(
                    relative, "crates/ui/chart_integration/Cargo.toml",
                    "Nucleus crates may be consumed only by chart_integration"
                );
            }
        }

        for path in production_rust_sources() {
            let relative = relative_string(&path);
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let production = production_prefix(&contents);
            for retired in [
                concat!("ori", "gin_", "engine"),
                concat!("ori", "gin_", "render"),
                concat!("Ori", "gin", "ChartView"),
                concat!("Ori", "gin", "Workspace"),
                concat!("ori", "gin_", "bridge"),
            ] {
                assert!(
                    !production.contains(retired),
                    "{relative} contains retired chart identity {retired}"
                );
            }
            if production.contains("nucleuscharts_engine")
                || production.contains("nucleuscharts_render")
                || production.contains("nucleuscharts_render_gpui")
            {
                assert!(
                    relative.starts_with("crates/ui/chart_integration/src/"),
                    "{relative} bypasses the Axiusflow chart integration boundary"
                );
            }
            if !relative.starts_with("crates/adapters/rithmic_protocol/") {
                assert!(
                    !production.contains("rithmic.protobuf"),
                    "{relative} leaks Rithmic vendor protobuf above its adapter"
                );
            }
        }

        for relative in super::REPOSITORY_MARKDOWN_FILES {
            let contents = manifest(relative);
            for retired in [
                concat!("Ori", "gin", " Charts"),
                concat!("Ori", "gin_", "charts"),
                concat!("Ori", "gin", "ChartView"),
                concat!("Ori", "gin", "Workspace"),
            ] {
                assert!(
                    !contents.contains(retired),
                    "{relative} contains retired chart identity {retired}"
                );
            }
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
                    "axiusflow_coinbase_market_adapter",
                    "axiusflow_rithmic_protocol_adapter",
                    "axiusflow_local_history",
                    "axiusflow_local_storage",
                    "axiusflow_market_engine",
                    "axiusflow_provider_history",
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
            "crates/local_storage/tests/history_store_lifecycle.rs",
            "crates/engine_protocol/tests/protocol.rs",
            "apps/engine/tests/handshake.rs",
            "apps/desktop/src/readiness_conformance.rs",
        ] {
            assert!(
                repository_root().join(relative).is_file(),
                "durable migration test boundary {relative} is missing"
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
                "axiusflow_coinbase_market_adapter",
                "axiusflow_desktop_market_runtime",
                "axiusflow_desktop_provider_runtime",
                "axiusflow_rithmic_protocol_adapter",
                "axiusflow_provider_history",
                "axiusflow_local_storage",
                "axiusflow_local_history",
                "axiusflow_market_engine",
            ],
        );
    }

    #[test]
    fn workspace_uses_only_axiusflow_owned_gpui_controls() {
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
    fn market_core_manifests_exclude_ipc_and_runtime_dependencies() {
        assert_dependencies_are(
            "crates/application/Cargo.toml",
            &["axiusflow_instruments", "axiusflow_market_data", "sha2"],
        );
        assert_dependencies_are(
            "crates/market_engine/Cargo.toml",
            &["axiusflow_market_data"],
        );
        assert_dependencies_are("crates/domain/instruments/Cargo.toml", &[]);
        assert_dependencies_are("crates/domain/market_data/Cargo.toml", &[]);
    }

    #[test]
    fn retired_runtime_wrappers_do_not_reenter_the_workspace() {
        for relative in [
            "Cargo.toml",
            "apps/desktop/Cargo.toml",
            "apps/engine/Cargo.toml",
        ] {
            assert_excludes(
                relative,
                &[
                    "axiusflow_desktop_market_runtime",
                    "axiusflow_desktop_provider_runtime",
                ],
            );
        }
        for relative in [
            "crates/desktop_market_runtime",
            "crates/desktop_provider_runtime",
        ] {
            let path = repository_root().join(relative);
            assert!(
                !path.join("Cargo.toml").exists(),
                "retired market authority {relative} must not regain a crate manifest"
            );
            let mut sources = Vec::new();
            if path.exists() {
                collect_files(
                    &path,
                    |source| {
                        source
                            .extension()
                            .is_some_and(|extension| extension == "rs")
                    },
                    &mut sources,
                );
            }
            assert!(sources.is_empty(), "{relative} must not regain Rust source");
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
    fn workspace_has_only_desktop_and_engine_applications() {
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
            ["desktop".to_string(), "engine".to_string()].into(),
            "Axiusflow has exactly one desktop and one resident engine application"
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
        let allowed = BTreeSet::from([
            "crates/adapters/coinbase_market/src/messages.rs",
            "crates/adapters/rithmic_protocol/src/lib.rs",
        ]);

        for path in production_rust_sources() {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            if production_prefix(&contents).contains("#[allow(dead_code") {
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
            "apps/engine/src/market_service.rs::HistorySource".to_string(),
            "apps/engine/src/market_service.rs::RealtimeSource".to_string(),
            "crates/adapters/coinbase_market/src/history.rs::CoinbaseHistoryTransport".to_string(),
            "crates/adapters/rithmic_protocol/src/history_adapter.rs::RithmicHistoryTransport"
                .to_string(),
            "crates/adapters/rithmic_protocol/src/provider_runtime.rs::ProviderSessionDriver"
                .to_string(),
            "crates/local_storage/src/model.rs::KeyRevocationEvidence".to_string(),
            "crates/platform_runtime/src/credential_vault.rs::CredentialVault".to_string(),
            "crates/platform_runtime/src/credential_vault.rs::NativeCredentialBackend".to_string(),
            "crates/provider_history/src/model.rs::ProviderHistoryAdapter".to_string(),
        ]);

        assert_eq!(
            actual, expected,
            "production traits require a provider, platform, security, or test-substitution boundary"
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
    fn local_market_protocol_excludes_trading_execution_commands() {
        let protocol = manifest("crates/engine_protocol/src/messages.rs");
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
                "market-data IPC must not add future execution message {forbidden}"
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
    fn production_excludes_shared_memory_ipc() {
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
                    "{} must not introduce shared-memory IPC primitive {forbidden}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn local_ipc_manifests_exclude_public_server_dependencies() {
        for relative in [
            "apps/engine/Cargo.toml",
            "crates/local_engine_client/Cargo.toml",
            "crates/engine_protocol/Cargo.toml",
        ] {
            assert_excludes(
                relative,
                &["actix", "axum", "hyper", "rocket", "tonic", "warp"],
            );
        }
        for relative in [
            "apps/engine/Cargo.toml",
            "crates/local_engine_client/Cargo.toml",
        ] {
            let contents = manifest(relative);
            assert!(
                contents.contains("interprocess"),
                "{relative} must use platform-local IPC"
            );
            assert!(
                contents.contains("axiusflow_engine_protocol"),
                "{relative} must use the versioned local protocol"
            );
        }
    }

    #[test]
    fn provider_adapters_exclude_storage_and_ui_from_production_dependencies() {
        for relative in [
            "crates/adapters/coinbase_market/Cargo.toml",
            "crates/adapters/rithmic_protocol/Cargo.toml",
        ] {
            let dependencies = production_dependencies(relative);
            for forbidden in [
                "axiusflow_local_storage",
                "axiusflow_local_history",
                "axiusflow_chart_integration",
                "axiusflow_terminal_ui",
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
        let contents = manifest("apps/engine/Cargo.toml");
        for dependency in [
            "axiusflow_coinbase_market_adapter",
            "axiusflow_rithmic_protocol_adapter",
            "axiusflow_local_history",
            "axiusflow_market_engine",
            "axiusflow_provider_history",
            "axiusflow_platform_runtime",
            "axiusflow_engine_protocol",
        ] {
            assert!(
                contents.contains(dependency),
                "apps/engine/Cargo.toml must compose {dependency}"
            );
        }
        for forbidden in [
            "gpui",
            "axiusflow_local_storage",
            "axiusflow_chart_integration",
            "axiusflow_terminal_ui",
        ] {
            assert!(
                !contents.contains(forbidden),
                "apps/engine/Cargo.toml must not depend on {forbidden}"
            );
        }
    }

    #[test]
    fn local_history_is_the_engine_consumed_storage_boundary() {
        let local_history = manifest("crates/local_history/Cargo.toml");
        for dependency in [
            "axiusflow_local_storage",
            "axiusflow_market_data",
            "axiusflow_platform_runtime",
        ] {
            assert!(
                local_history.contains(dependency),
                "local_history must compose {dependency}"
            );
        }
        for forbidden in [
            "axiusflow_coinbase_market_adapter",
            "axiusflow_rithmic_protocol_adapter",
            "axiusflow_provider_history",
            "gpui",
        ] {
            assert!(
                !production_dependencies("crates/local_history/Cargo.toml").contains(forbidden),
                "local_history must not own {forbidden}"
            );
        }
        assert!(
            repository_root()
                .join("crates/local_history/src/store.rs")
                .is_file(),
            "local_history must own production immutable segment mechanics"
        );
        assert!(
            !repository_root()
                .join("apps/engine/src/local_history.rs")
                .exists(),
            "the engine app must not duplicate local-history storage mechanics"
        );
    }

    #[test]
    fn resident_market_contracts_remain_explicit_and_provider_neutral() {
        let demand = manifest("crates/market_engine/src/demand.rs");
        assert!(
            demand.contains("pub(crate) fn set_viewport")
                && demand.contains("if generation != current")
                && demand.contains("EngineError::StaleConsumerGeneration"),
            "viewport demand must remain explicit and generation fenced"
        );

        let provider_manager = manifest("crates/market_engine/src/provider_manager.rs");
        for contract in [
            "pub struct ProviderCapabilities",
            "historical_bars: bool",
            "realtime_bars: bool",
            "streams: StreamRequirements",
            "verify_request",
            "verify_streams",
        ] {
            assert!(
                provider_manager.contains(contract),
                "provider capability contract lost {contract}"
            );
        }

        let engine_core = manifest("crates/market_engine/src/lib.rs");
        for regression in [
            "provider_and_viewport_generations_are_exactly_fenced",
            "provider_configuration_routes_only_supported_generation_fenced_requests",
            "shared_subscriptions_ref_count_streams_and_release_only_the_last_consumer",
        ] {
            assert!(
                engine_core.contains(regression),
                "resident market contract lost regression {regression}"
            );
        }

        let coordinator = manifest("apps/engine/src/market_service.rs");
        for regression in [
            "timeframe_switch_waits_for_its_own_current_provider_history",
            "newer_demand_cancels_history_without_waiting_for_cleanup",
            "symbol_and_interval_switch_reuses_the_shared_realtime_session",
        ] {
            assert!(
                coordinator.contains(regression),
                "viewport/history lifecycle lost regression {regression}"
            );
        }
    }

    #[test]
    fn local_clients_versions_and_workspace_writes_remain_fenced() {
        let engine_core = manifest("crates/market_engine/src/lib.rs");
        assert!(
            engine_core.contains("disconnect_removes_only_the_owning_clients_consumers"),
            "multiple desktop clients must remain client scoped"
        );

        let engine_ipc = manifest("apps/engine/src/lib.rs");
        for regression in [
            "authenticated_connection_drop_retires_detached_client_consumers",
            "dormant_market_client_does_not_starve_another_clients_control",
        ] {
            assert!(
                engine_ipc.contains(regression),
                "authenticated multi-client behavior lost regression {regression}"
            );
        }
        let coordinator = manifest("apps/engine/src/market_service.rs");
        assert!(
            coordinator.contains("later_consumers_reuse_one_engine_history_fetch")
                && coordinator.contains("another client's consumer is rejected"),
            "multiple clients must share upstream work without sharing consumer authority"
        );
        assert!(
            engine_ipc.contains("workspace revision is stale")
                && engine_ipc.contains("stale_workspace_fault"),
            "workspace writes must reject stale desktop revisions explicitly"
        );

        let protocol = manifest("crates/engine_protocol/src/lib.rs");
        assert!(
            protocol.contains("pub const PROTOCOL_VERSION: u32 = 14"),
            "incompatible IPC revisions require a deliberate protocol-version change"
        );
        let codec = manifest("crates/engine_protocol/src/codec.rs");
        assert!(
            codec.contains("ProtocolError::VersionMismatch"),
            "IPC decoding must fail closed on incompatible protocol versions"
        );
        let client = manifest("crates/local_engine_client/src/lib.rs");
        for contract in [
            "protocol_socket_name_tracks_the_active_version",
            "reached_endpoint_is_retried_without_spawning_another_engine",
        ] {
            assert!(
                client.contains(contract),
                "engine startup/version fencing lost {contract}"
            );
        }

        let handshake = manifest("apps/engine/tests/handshake.rs");
        for regression in [
            "workspace_selection_is_durable_across_engine_restart",
            "chart_viewport_is_generation_fenced_and_persisted_independently",
            "shutdown_flush_preserves_the_latest_hot_set_and_fences_late_mutation",
        ] {
            assert!(
                handshake.contains(regression),
                "workspace revision authority lost regression {regression}"
            );
        }
    }
}
