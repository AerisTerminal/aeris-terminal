use serde::Serialize;
use std::{error::Error, fs, path::Path};

#[derive(Serialize)]
pub(super) struct WorkloadShape {
    pub workspace_updates: usize,
    pub catalog_entries: usize,
    pub order_intents: usize,
}

#[derive(Serialize)]
pub(super) struct LatencySummary {
    pub samples: usize,
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    pub p99_nanos: u64,
    pub maximum_nanos: u64,
}

impl LatencySummary {
    pub fn from_samples(mut samples: Vec<u64>) -> Result<Self, Box<dyn Error>> {
        if samples.is_empty() {
            return Err("latency summary requires at least one sample".into());
        }
        samples.sort_unstable();
        Ok(Self {
            samples: samples.len(),
            p50_nanos: percentile(&samples, 50, 100),
            p95_nanos: percentile(&samples, 95, 100),
            p99_nanos: percentile(&samples, 99, 100),
            maximum_nanos: *samples.last().ok_or("missing maximum sample")?,
        })
    }
}

fn percentile(samples: &[u64], numerator: usize, denominator: usize) -> u64 {
    let rank = samples
        .len()
        .saturating_mul(numerator)
        .saturating_add(denominator.saturating_sub(1))
        / denominator;
    samples[rank.saturating_sub(1).min(samples.len().saturating_sub(1))]
}

#[derive(Serialize)]
pub(super) struct BackendEvidence {
    pub backend: &'static str,
    pub workspace_total_latency: LatencySummary,
    pub catalog_total_latency: LatencySummary,
    pub order_commit_latency: LatencySummary,
    pub reopen_latency: LatencySummary,
    pub file_bytes_p50: u64,
    pub file_bytes_maximum: u64,
    pub migration: &'static str,
    pub committed_reopen: &'static str,
    pub uncommitted_rollback: &'static str,
    pub abrupt_process_recovery: &'static str,
    pub corruption_detection: &'static str,
    pub disk_full_failure: &'static str,
}

#[derive(Serialize)]
pub(super) struct EmbeddedStoreReport {
    pub schema_version: u32,
    pub evidence_scope: &'static str,
    pub source_revision: String,
    pub benchmark_root: String,
    pub benchmark_filesystem: String,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub build_profile: &'static str,
    pub runs: usize,
    pub workloads: WorkloadShape,
    pub sqlite_version: String,
    pub expected_sqlite_version: &'static str,
    pub rusqlite_version: &'static str,
    pub redb_version: &'static str,
    pub sqlite: BackendEvidence,
    pub redb: BackendEvidence,
    pub selection: &'static str,
    pub selection_scope: &'static str,
    pub history_payload_decision: &'static str,
    pub limitations: [&'static str; 4],
}

pub(super) fn write(path: &Path, report: &EmbeddedStoreReport) -> Result<(), Box<dyn Error>> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut encoded = serde_json::to_vec_pretty(report)?;
    encoded.push(b'\n');
    fs::write(path, encoded)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LatencySummary;

    #[test]
    fn percentile_summary_uses_nearest_rank_without_interpolation() {
        let summary = LatencySummary::from_samples((1..=100).collect())
            .expect("non-empty samples produce a summary");
        assert_eq!(summary.p50_nanos, 50);
        assert_eq!(summary.p95_nanos, 95);
        assert_eq!(summary.p99_nanos, 99);
        assert_eq!(summary.maximum_nanos, 100);
    }

    #[test]
    fn percentile_summary_rejects_missing_evidence() {
        assert!(LatencySummary::from_samples(Vec::new()).is_err());
    }
}
