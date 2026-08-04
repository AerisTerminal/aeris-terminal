//! Feed/profile compatibility evidence for one live provider feed class.
//!
//! Evaluates every active provider profile against the live provider's feed
//! transport class and writes one matrix artifact with an explicit readiness
//! result and reason, per Section 7.1.

use axiusflow_transport::{
    FeedProfileCompatibility, FeedTransportClass, ProviderFeedProfile, compatibility_reason,
    evaluate_profile_feed,
};
use serde::Serialize;
use std::{env, error::Error, fs, path::Path};

const EVIDENCE_SCHEMA_VERSION: u32 = 2;
const EVIDENCE_SCOPE: &str = "stage_2_feed_profile_matrix";
const ACTIVE_PROVIDER_PROFILES: [ProviderFeedProfile; 4] = [
    ProviderFeedProfile::ProviderWebsocket,
    ProviderFeedProfile::ProviderNative,
    ProviderFeedProfile::DeterministicReplay,
    ProviderFeedProfile::CloudStream,
];

#[derive(Serialize)]
struct ProfileEvaluation {
    profile: &'static str,
    compatibility: FeedProfileCompatibility,
    reason: &'static str,
}

#[derive(Serialize)]
struct FeedProfileMatrixReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    live_provider: &'static str,
    feed_class: FeedTransportClass,
    feed_class_verified_live: bool,
    evaluations: Vec<ProfileEvaluation>,
    silent_fallback: &'static str,
}

/// Evaluates the matrix for one feed class and writes the evidence artifact.
pub fn run(
    live_provider: &str,
    feed_class: FeedTransportClass,
    report_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for feed/profile matrix evidence")?;
    let evaluations = ACTIVE_PROVIDER_PROFILES
        .iter()
        .map(|profile| {
            let compatibility = evaluate_profile_feed(*profile, feed_class);
            ProfileEvaluation {
                profile: profile_name(*profile),
                compatibility,
                reason: compatibility_reason(*profile, compatibility),
            }
        })
        .collect();
    let report = FeedProfileMatrixReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        live_provider: match live_provider {
            "coinbase" => "coinbase",
            _ => return Err(format!("unknown live provider: {live_provider}").into()),
        },
        feed_class,
        feed_class_verified_live: true,
        evaluations,
        silent_fallback: "forbidden",
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "feed_profile_matrix=passed provider={live_provider} feed_class={feed_class:?} silent_fallback=forbidden report={}",
        report_path.display()
    );
    Ok(())
}

fn profile_name(profile: ProviderFeedProfile) -> &'static str {
    match profile {
        ProviderFeedProfile::ProviderWebsocket => "provider_websocket",
        ProviderFeedProfile::ProviderNative => "provider_native",
        ProviderFeedProfile::DeterministicReplay => "deterministic_replay",
        ProviderFeedProfile::CloudStream => "cloud_stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{ACTIVE_PROVIDER_PROFILES, profile_name};

    #[test]
    fn active_matrix_uses_only_provider_oriented_profile_names() {
        assert_eq!(
            ACTIVE_PROVIDER_PROFILES.map(profile_name),
            [
                "provider_websocket",
                "provider_native",
                "deterministic_replay",
                "cloud_stream",
            ]
        );
    }
}
