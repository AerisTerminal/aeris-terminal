//! `aeris_release_packager` packages one signed release, points the stable channel at a
//! packaged release, or prints the verifying key of a signing key file. It writes only below
//! `--output` and publishes nothing.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};

use aeris_release_lifecycle::RolloutMetadata;
use aeris_release_packager::{
    BlockPlanSummary, ChannelRequest, InnoSetupRequest, ReleaseRequest, inno_setup_arguments,
    package_release, read_signing_key, verifying_key_text, write_channel_pointer,
};

const USAGE: &str = "usage:
  aeris_release_packager release --signing-key <file> --payload <dir> --output <dir>
      --base-url <https-url> --release-identity <id> --generation <n>
      [--previous <release dir>] [--rollout-cohort <name>] [--rollout-percentage <0-100>]
      [--inno-script <aeris_setup.iss> --icon <aeris.ico> --version <version>]
  aeris_release_packager channel --output <dir> --base-url <https-url>
      --release-identity <id> --generation <n> --version <version>
      --published-at <YYYY-MM-DDTHH:MM:SSZ>
  aeris_release_packager verifying-key --signing-key <file>";

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("aeris_release_packager: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut arguments: impl Iterator<Item = OsString>) -> Result<serde_json::Value, String> {
    let command = arguments.next().ok_or_else(|| USAGE.to_string())?;
    match command.to_str() {
        Some("release") => release(Options::parse(
            arguments,
            &[
                "--signing-key",
                "--payload",
                "--output",
                "--base-url",
                "--release-identity",
                "--generation",
                "--previous",
                "--rollout-cohort",
                "--rollout-percentage",
                "--inno-script",
                "--icon",
                "--version",
            ],
        )?),
        Some("channel") => channel(Options::parse(
            arguments,
            &[
                "--output",
                "--base-url",
                "--release-identity",
                "--generation",
                "--version",
                "--published-at",
            ],
        )?),
        Some("verifying-key") => {
            let mut options = Options::parse(arguments, &["--signing-key"])?;
            let key = read_signing_key(&options.path("--signing-key")?)?;
            Ok(serde_json::json!({ "verifying_key": verifying_key_text(&key) }))
        }
        _ => Err(USAGE.to_string()),
    }
}

fn release(mut options: Options) -> Result<serde_json::Value, String> {
    let key = read_signing_key(&options.path("--signing-key")?)?;
    let payload = options.path("--payload")?;
    let output = options.path("--output")?;
    let base_url = options.text("--base-url")?;
    let release_identity = options.text("--release-identity")?;
    let install_generation = options.number("--generation")?;
    let previous = options.optional_path("--previous")?;
    let rollout = RolloutMetadata {
        cohort: options
            .optional_text("--rollout-cohort")?
            .unwrap_or_else(|| "stable".to_string()),
        percentage: options
            .optional_number("--rollout-percentage")?
            .unwrap_or(100),
    };
    let inno = match (
        options.optional_path("--inno-script")?,
        options.optional_path("--icon")?,
        options.optional_text("--version")?,
    ) {
        (Some(script), Some(icon), Some(version)) => Some((script, icon, version)),
        (None, None, None) => None,
        _ => return Err("--inno-script, --icon and --version go together".to_string()),
    };
    let packaged = package_release(
        &ReleaseRequest {
            payload: &payload,
            output: &output,
            base_url: &base_url,
            release_identity: &release_identity,
            install_generation,
            rollout,
            // The packager is built from the payload's commit, so this is the desktop's schema.
            max_store_schema: aeris_trading_runtime::SCHEMA_VERSION,
            previous: previous.as_deref(),
        },
        &key,
    )?;
    let block_plan = match packaged.block_plan {
        BlockPlanSummary::NotRequested => serde_json::json!("not_requested"),
        BlockPlanSummary::NoSharedBlocks => serde_json::json!("no_shared_blocks"),
        BlockPlanSummary::Written {
            files,
            download_blocks,
        } => serde_json::json!({ "files": files, "download_blocks": download_blocks }),
    };
    let mut report = serde_json::json!({
        "release_directory": packaged.directory,
        "release_identity": packaged.manifest.manifest().release_identity,
        "install_generation": packaged.manifest.manifest().install_generation,
        "block_plan": block_plan,
    });
    if let Some((script, icon, version)) = inno {
        report["inno_setup_arguments"] =
            serde_json::json!(inno_setup_arguments(&InnoSetupRequest {
                script: &script,
                icon: &icon,
                version: &version,
                release_directory: &packaged.directory,
            }));
    }
    Ok(report)
}

fn channel(mut options: Options) -> Result<serde_json::Value, String> {
    let output = options.path("--output")?;
    let pointer = write_channel_pointer(&ChannelRequest {
        output: &output,
        base_url: &options.text("--base-url")?,
        release_identity: &options.text("--release-identity")?,
        install_generation: options.number("--generation")?,
        version: &options.text("--version")?,
        published_at: &options.text("--published-at")?,
    })?;
    Ok(serde_json::json!({ "channel_pointer": pointer }))
}

/// `--name value` pairs, each named at most once and only from the command's set.
struct Options(BTreeMap<String, OsString>);

impl Options {
    fn parse(
        mut arguments: impl Iterator<Item = OsString>,
        allowed: &[&str],
    ) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        while let Some(name) = arguments.next() {
            let name = name
                .into_string()
                .ok()
                .filter(|name| allowed.contains(&name.as_str()))
                .ok_or_else(|| USAGE.to_string())?;
            let value = arguments
                .next()
                .ok_or_else(|| format!("{name} needs a value"))?;
            if values.insert(name.clone(), value).is_some() {
                return Err(format!("{name} is given more than once"));
            }
        }
        Ok(Self(values))
    }

    fn optional_text(&mut self, name: &str) -> Result<Option<String>, String> {
        self.0
            .remove(name)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| format!("{name} is not Unicode"))
            })
            .transpose()
    }

    fn text(&mut self, name: &str) -> Result<String, String> {
        self.optional_text(name)?
            .ok_or_else(|| format!("{name} is required"))
    }

    fn optional_number<T: FromStr>(&mut self, name: &str) -> Result<Option<T>, String> {
        self.optional_text(name)?
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| format!("{name} is not a valid number"))
            })
            .transpose()
    }

    fn number<T: FromStr>(&mut self, name: &str) -> Result<T, String> {
        self.optional_number(name)?
            .ok_or_else(|| format!("{name} is required"))
    }

    fn optional_path(&mut self, name: &str) -> Result<Option<PathBuf>, String> {
        self.0
            .remove(name)
            .map(|value| {
                std::path::absolute(Path::new(&value))
                    .map_err(|_| format!("{name} is not a usable path"))
            })
            .transpose()
    }

    fn path(&mut self, name: &str) -> Result<PathBuf, String> {
        self.optional_path(name)?
            .ok_or_else(|| format!("{name} is required"))
    }
}
