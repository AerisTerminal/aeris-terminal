use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct Options {
    pub(super) root: PathBuf,
    pub(super) out: PathBuf,
    pub(super) instrument: String,
    pub(super) duration: Duration,
}

impl Options {
    pub(super) fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut root = None;
        let mut out = None;
        let mut instrument = "BTC".to_string();
        let mut duration = Duration::from_secs(120);
        let mut arguments = arguments.into_iter();
        while let Some(flag) = arguments.next() {
            let value = arguments.next().ok_or("missing probe option value")?;
            match flag.as_str() {
                "--validation-root" if root.is_none() => root = Some(PathBuf::from(value)),
                "--out" if out.is_none() => out = Some(PathBuf::from(value)),
                "--instrument" if matches!(value.as_str(), "BTC" | "ETH" | "SOL") => {
                    instrument = value;
                }
                "--duration-secs" => {
                    let seconds = value.parse::<u64>().map_err(|_| "invalid probe duration")?;
                    if !(120..=600).contains(&seconds) {
                        return Err("probe duration must be between 120 and 600 seconds");
                    }
                    duration = Duration::from_secs(seconds);
                }
                _ => return Err("unknown, duplicate or unsafe probe option"),
            }
        }
        let root = root.ok_or("--validation-root is required")?;
        let out = out.ok_or("--out is required")?;
        Self::validate_paths(&root, &out)?;
        Ok(Self {
            root,
            out,
            instrument,
            duration,
        })
    }

    fn validate_paths(root: &Path, out: &Path) -> Result<(), &'static str> {
        aeris_probes::validate_paths(root, out)
    }
}

#[cfg(test)]
mod tests {
    use super::Options;

    #[test]
    fn probe_refuses_production_and_traversal_before_any_io() {
        let production = aeris_platform_runtime::data_root::native_production_data_root()
            .expect("production root path");
        let arguments = |root: &std::path::Path| {
            vec![
                "--validation-root".into(),
                root.to_string_lossy().into_owned(),
                "--out".into(),
                root.join("probe.json").to_string_lossy().into_owned(),
            ]
        };
        assert!(Options::parse(arguments(&production)).is_err());
        let traversal = std::env::temp_dir()
            .join("aeris_validation")
            .join("..")
            .join("bad");
        assert!(Options::parse(arguments(&traversal)).is_err());
    }

    #[test]
    fn probe_requires_bounded_duration_and_output_under_root() {
        let root = std::env::temp_dir()
            .join("aeris_validation")
            .join("probe-unit");
        let arguments = |out: &std::path::Path, seconds: &str| {
            vec![
                "--validation-root".into(),
                root.to_string_lossy().into_owned(),
                "--out".into(),
                out.to_string_lossy().into_owned(),
                "--duration-secs".into(),
                seconds.into(),
            ]
        };
        assert!(Options::parse(arguments(&root.join("probe.json"), "120")).is_ok());
        assert!(Options::parse(arguments(&root.join("probe.json"), "0")).is_err());
        assert!(
            Options::parse(arguments(&std::env::temp_dir().join("outside.json"), "120")).is_err()
        );
    }
}
