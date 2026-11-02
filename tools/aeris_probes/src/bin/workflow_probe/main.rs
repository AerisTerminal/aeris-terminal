//! Trading-workflow probe: drives accepted workflows through the real trading owner against
//! the live public Hyperliquid feed on an isolated validation root. A price workflow enters
//! a bracketed long when the bid crosses its level, a time-of-day workflow closes it, and a
//! trading restart proves neither fires again. Records an `aeris.probe.v1` envelope and
//! exits non-zero on the first failed check. Non-shipping validation tooling.

mod scenario;

use std::{error::Error, path::PathBuf};

use aeris_probes::{Envelope, Evidence, ProbeError, validate_paths};

struct Options {
    root: PathBuf,
    out: PathBuf,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, &'static str> {
        let mut root = None;
        let mut out = None;
        let mut arguments = arguments.into_iter();
        while let Some(flag) = arguments.next() {
            let slot = match flag.as_str() {
                "--validation-root" if root.is_none() => &mut root,
                "--out" if out.is_none() => &mut out,
                _ => return Err("unknown, duplicate or unsafe probe option"),
            };
            let value = arguments.next().ok_or("missing probe option value")?;
            *slot = Some(PathBuf::from(value));
        }
        let root = root.ok_or("--validation-root is required")?;
        let out = out.ok_or("--out is required")?;
        validate_paths(&root, &out)?;
        Ok(Self { root, out })
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
    let envelope = Envelope::collect("workflows", options.root.clone(), options.out.clone())?;
    let started_at = aeris_probes::utc_now().to_rfc3339();
    let mut evidence = Evidence::new();
    let result = scenario::run(root, &mut evidence);
    let passed = envelope.finish(&started_at, &mut evidence, &result)?;
    if !passed {
        return Err(Box::new(ProbeError::Invariant(
            "workflow probe failed; see JSON evidence".to_string(),
        )));
    }
    println!("Workflow probe passed: {}", options.out.display());
    Ok(())
}
