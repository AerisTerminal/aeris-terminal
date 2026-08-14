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
    use std::{fs, path::Path};

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
            .unwrap_or_else(|| panic!("{relative} must contain a dependencies table"))
            .to_string()
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
                "axiusflow_rithmic_protocol_adapter",
                "axiusflow_provider_history",
                "axiusflow_desktop_storage",
                "axiusflow_desktop_history",
                "axiusflow_market_engine",
            ],
        );
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
