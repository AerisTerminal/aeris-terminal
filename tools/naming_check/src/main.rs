use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SKIPPED_DIRECTORIES: &[&str] = &[".git", "node_modules", "origin_charts", "target"];
const PLATFORM_FILE_EXCEPTIONS: &[&str] = &[
    ".gitignore",
    ".gitmodules",
    "Cargo.lock",
    "Cargo.toml",
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
