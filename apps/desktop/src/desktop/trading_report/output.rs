//! Report files in the reports folder: one durable write per report, named by its UTC
//! generation time. The folder is the user's (in Downloads), so Aeris never deletes from it.

use sha2::Digest as _;
use std::io;
use std::path::{Path, PathBuf};

const REPORT_PREFIX: &str = "aeris-report-";
const REPORT_EXTENSION: &str = ".html";
/// Names tried when another report was saved in the same millisecond.
const MAXIMUM_NAME_ATTEMPTS: usize = 16;

/// Lowercase hexadecimal SHA-256 of the report content.
pub(super) fn fingerprint(content: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = sha2::Sha256::digest(content.as_bytes());
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Writes `html` as a new report beside the earlier ones, never replacing a file.
pub(super) fn save_report(
    directory: &Path,
    generated_unix_nanos: i64,
    html: &[u8],
) -> io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let stem = report_stem(generated_unix_nanos);
    let path = (0..MAXIMUM_NAME_ATTEMPTS)
        .map(|attempt| {
            directory.join(if attempt == 0 {
                format!("{stem}{REPORT_EXTENSION}")
            } else {
                format!("{stem}-{attempt}{REPORT_EXTENSION}")
            })
        })
        .find(|path| !path.exists())
        .ok_or_else(|| io::Error::new(io::ErrorKind::AlreadyExists, "report name is taken"))?;
    aeris_platform_runtime::fs::write_file_durably(&path, html)?;
    Ok(path)
}

/// `aeris-report-YYYYMMDD-HHMMSS-mmm` in UTC, so names sort by generation time.
fn report_stem(generated_unix_nanos: i64) -> String {
    let millis = generated_unix_nanos.div_euclid(1_000_000);
    let time = chrono::DateTime::from_timestamp_millis(millis).unwrap_or_default();
    format!(
        "{REPORT_PREFIX}{}-{:03}",
        time.format("%Y%m%d-%H%M%S"),
        millis.rem_euclid(1_000)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "aeris-report-output-{label}-{}",
            std::process::id()
        ));
        if directory.exists() {
            std::fs::remove_dir_all(&directory).expect("clear directory");
        }
        directory
    }

    #[test]
    fn report_names_sort_by_time_and_never_overwrite() {
        let directory = temporary_directory("names");
        let at = 1_794_411_000_123_000_000;
        let first = save_report(&directory, at, b"first").expect("first");
        let second = save_report(&directory, at, b"second").expect("second");
        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("aeris-report-20261111-153000-123.html")
        );
        assert_eq!(
            second.file_name().and_then(|name| name.to_str()),
            Some("aeris-report-20261111-153000-123-1.html")
        );
        assert_eq!(std::fs::read(&first).expect("read"), b"first");
        std::fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn the_fingerprint_is_the_sha256_of_the_content() {
        assert_eq!(
            fingerprint("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
