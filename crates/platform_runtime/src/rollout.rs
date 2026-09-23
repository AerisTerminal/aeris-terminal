//! Local, deterministic staged-release eligibility.
//!
//! The signed release manifest owns the rollout namespace and percentage. The
//! launcher contributes only its stable per-user installation root. No account,
//! credential, or telemetry identifier is required or emitted.

use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::lifecycle::RolloutMetadata;

const INSTALLATION_DOMAIN: &[u8] = b"asceify-rollout-installation-v1\0";
const COHORT_DOMAIN: &[u8] = b"asceify-rollout-cohort-v1\0";

/// Returns whether this installation is eligible for one signed rollout.
///
/// `0%` is an explicit hold and `100%` is universally eligible. Intermediate
/// percentages use a deterministic, local-only bucket keyed by the installation
/// root and signed cohort namespace. Increasing the percentage for the same
/// cohort only adds installations; it never removes an already-eligible one.
#[must_use]
pub fn rollout_eligible(metadata: &RolloutMetadata, install_root: &Path) -> bool {
    let percentage = metadata.percentage;
    if percentage == 0 || percentage > 100 {
        return false;
    }
    if percentage == 100 {
        return true;
    }

    let sample = rollout_sample(metadata, install_root);
    let threshold = (1_u128 << 64) * u128::from(percentage) / 100;
    u128::from(sample) < threshold
}

fn rollout_sample(metadata: &RolloutMetadata, install_root: &Path) -> u64 {
    let mut installation = Sha256::new();
    installation.update(INSTALLATION_DOMAIN);
    installation.update(install_root.to_string_lossy().as_bytes());
    let installation = installation.finalize();

    let mut cohort = Sha256::new();
    cohort.update(COHORT_DOMAIN);
    cohort.update(installation);
    cohort.update(metadata.cohort.as_bytes());
    let digest = cohort.finalize();

    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollout(cohort: &str, percentage: u8) -> RolloutMetadata {
        RolloutMetadata {
            cohort: cohort.to_string(),
            percentage,
        }
    }

    #[test]
    fn zero_percent_is_a_hold_and_full_rollout_needs_no_bucket_exception() {
        let root = Path::new("/users/example/asceify");
        assert!(!rollout_eligible(&rollout("stable", 0), root));
        assert!(rollout_eligible(&rollout("stable", 100), root));
        assert!(!rollout_eligible(&rollout("stable", 101), root));
    }

    #[test]
    fn bucket_is_deterministic_and_namespaced_by_signed_cohort() {
        let root = Path::new("/users/example/asceify");
        let stable = rollout("stable", 50);
        let canary = rollout("canary", 50);

        assert_eq!(rollout_sample(&stable, root), rollout_sample(&stable, root));
        assert_ne!(rollout_sample(&stable, root), rollout_sample(&canary, root));
        assert_ne!(
            rollout_sample(&stable, root),
            rollout_sample(&stable, Path::new("/users/other/asceify"))
        );
    }

    #[test]
    fn increasing_percentage_for_one_ring_is_monotonic() {
        let root = Path::new("/users/example/asceify");
        let mut seen_eligible = false;
        for percentage in 0..=100 {
            let eligible = rollout_eligible(&rollout("stable", percentage), root);
            if seen_eligible {
                assert!(eligible, "eligibility regressed at {percentage}%");
            }
            seen_eligible |= eligible;
        }
        assert!(seen_eligible);
    }

    #[test]
    fn deterministic_vector_fences_bucket_algorithm_changes() {
        let sample = rollout_sample(&rollout("canary", 25), Path::new("/users/example/asceify"));
        assert_eq!(sample, 16_128_428_820_566_202_637);
    }
}
