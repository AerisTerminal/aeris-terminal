//! M5.12 paper-trading probe: drives the full simulated-trading path against the live
//! public Hyperliquid feed on an isolated validation root. Records an
//! `aeris.probe.v1` envelope and exits non-zero on the first failed check.
//! Non-shipping validation tooling.

mod scenario;

use std::{error::Error, path::PathBuf};

use aeris_probes::{Envelope, Evidence, ProbeError, validate_paths};

pub struct Options {
    pub root: PathBuf,
    pub out: PathBuf,
    pub no_fees: bool,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut root = None;
        let mut out = None;
        let mut no_fees = false;
        let mut arguments = arguments.into_iter();
        while let Some(flag) = arguments.next() {
            match flag.as_str() {
                "--validation-root" if root.is_none() => {
                    let value = arguments.next().ok_or("missing probe option value")?;
                    root = Some(PathBuf::from(value));
                }
                "--out" if out.is_none() => {
                    let value = arguments.next().ok_or("missing probe option value")?;
                    out = Some(PathBuf::from(value));
                }
                "--no-fees" if !no_fees => no_fees = true,
                _ => return Err("unknown, duplicate or unsafe probe option"),
            }
        }
        let root = root.ok_or("--validation-root is required")?;
        let out = out.ok_or("--out is required")?;
        validate_paths(&root, &out)?;
        Ok(Self { root, out, no_fees })
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    use aeris_platform_runtime::data_root::{DataRootMode, initialize_data_root};

    let options = Options::parse(std::env::args().skip(1)).map_err(ProbeError::Argument)?;
    // Reject unsafe roots before collecting evidence or attempting to write output.
    let root = initialize_data_root(DataRootMode::Validation {
        root: options.root.clone(),
    })
    .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    // The probe sources are not committed while the milestone is under development;
    // the worktree state stays on the envelope as evidence without gating the outcome.
    let envelope = Envelope::collect_with_policy(
        "paper_trading",
        options.root.clone(),
        options.out.clone(),
        false,
    )?;
    let started_at = aeris_probes::utc_now().to_rfc3339();
    let mut evidence = Evidence::new();
    let result = scenario::run(root, &options, &mut evidence);
    let passed = envelope.finish(&started_at, &mut evidence, &result)?;
    if !passed {
        return Err(Box::new(ProbeError::Invariant(
            "paper trading probe failed; see JSON evidence".to_string(),
        )));
    }
    println!("Paper trading probe passed: {}", options.out.display());
    Ok(())
}
