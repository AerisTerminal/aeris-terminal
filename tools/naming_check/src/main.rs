use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "__pycache__",
    "node_modules",
    "origin_charts",
    "target",
    "third_party",
];
const PLATFORM_FILE_EXCEPTIONS: &[&str] = &[
    ".gitignore",
    ".gitmodules",
    "AGENTS.md",
    "Cargo.lock",
    "Cargo.toml",
    "Dockerfile",
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
    if PLATFORM_FILE_EXCEPTIONS.contains(&name.as_ref()) {
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
            "crates/desktop_storage/Cargo.toml",
            "crates/desktop_history/Cargo.toml",
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
    fn desktop_manifest_excludes_backend_implementation_crates() {
        assert_excludes(
            "apps/desktop/Cargo.toml",
            &[
                "axiusflow_coinbase_market_adapter",
                "axiusflow_desktop_market_runtime",
                "axiusflow_desktop_provider_runtime",
                "axiusflow_rithmic_protocol_adapter",
                "axiusflow_provider_history",
                "axiusflow_desktop_storage",
                "axiusflow_desktop_history",
                "axiusflow_market_engine",
            ],
        );
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
            "crates/local_engine_protocol/Cargo.toml",
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
                contents.contains("axiusflow_local_engine_protocol"),
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
                "axiusflow_desktop_storage",
                "axiusflow_desktop_history",
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
            "axiusflow_desktop_storage",
            "axiusflow_market_engine",
            "axiusflow_provider_history",
            "axiusflow_platform_runtime",
            "axiusflow_local_engine_protocol",
        ] {
            assert!(
                contents.contains(dependency),
                "apps/engine/Cargo.toml must compose {dependency}"
            );
        }
        for forbidden in [
            "gpui",
            "axiusflow_chart_integration",
            "axiusflow_terminal_ui",
        ] {
            assert!(
                !contents.contains(forbidden),
                "apps/engine/Cargo.toml must not depend on {forbidden}"
            );
        }
    }
}
