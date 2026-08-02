//! Privileged-free DPDK EAL and virtual-device lifecycle evidence.
//!
//! This lane proves EAL startup, `net_ring` virtual-device enumeration, runtime
//! version binding to the exact reviewed release, and clean teardown. It does not
//! exercise queue or mbuf lifecycle, so DPDK readiness stays `contract_only` and no
//! poll-mode receive path is claimed.

use axiusflow_linux_dpdk_adapter::{EXPECTED_DPDK_VERSION, NativeEalLifecycle};
use serde::Serialize;
use std::{env, error::Error, fs, path::Path};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_1_dpdk_vdev_lifecycle";

#[derive(Serialize)]
struct LifecycleEvidence {
    eal_init: &'static str,
    runtime_version_bound: &'static str,
    virtual_device_enumerated: &'static str,
    eal_cleanup: &'static str,
    queue_mbuf_lifecycle: &'static str,
    poll_mode_receive: &'static str,
}

#[derive(Serialize)]
struct ClaimEvidence {
    poll_mode_active: &'static str,
    zero_copy: &'static str,
    hardware: &'static str,
    provider: &'static str,
    production: &'static str,
}

#[derive(Serialize)]
struct DpdkVdevLifecycleReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    profile: &'static str,
    readiness: &'static str,
    expected_dpdk_version: &'static str,
    runtime_version: String,
    ports_available: u16,
    lifecycle: LifecycleEvidence,
    claims: ClaimEvidence,
    limitations: [&'static str; 3],
}

/// Runs the native EAL and `net_ring` lifecycle and writes one evidence artifact.
pub fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for DPDK lifecycle evidence")?;
    let lifecycle = NativeEalLifecycle::run_net_ring_lifecycle()
        .map_err(|error| format!("DPDK net_ring EAL lifecycle failed: {error}"))?;
    let report = DpdkVdevLifecycleReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        profile: "linux_dpdk",
        readiness: "contract_only",
        expected_dpdk_version: EXPECTED_DPDK_VERSION,
        runtime_version: lifecycle.runtime_version.clone(),
        ports_available: lifecycle.ports_available,
        lifecycle: LifecycleEvidence {
            eal_init: "passed",
            runtime_version_bound: "passed",
            virtual_device_enumerated: "passed",
            eal_cleanup: "passed",
            queue_mbuf_lifecycle: "not_exercised",
            poll_mode_receive: "not_exercised",
        },
        claims: ClaimEvidence {
            poll_mode_active: "not_claimed",
            zero_copy: "not_claimed",
            hardware: "not_claimed",
            provider: "not_claimed",
            production: "not_claimed",
        },
        limitations: [
            "queue_mbuf_lifecycle_unexercised",
            "no_huge_pages_no_pci_virtual_device_only",
            "qualified_nic_driver_unverified",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "dpdk_vdev_lifecycle=passed readiness=contract_only runtime_version={} ports={} cleanup=true poll_mode=not_exercised report={}",
        lifecycle.runtime_version,
        lifecycle.ports_available,
        report_path.display()
    );
    Ok(())
}
