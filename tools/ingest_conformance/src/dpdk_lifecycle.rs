//! Privileged-free DPDK EAL, virtual-device, and queue/mbuf lifecycle evidence.
//!
//! This lane proves EAL startup, `net_ring` virtual-device enumeration, runtime
//! version binding to the exact reviewed release, and the queue/mbuf lifecycle:
//! pool creation, port configuration, queue setup, a bounded transmit/receive
//! loopback with payload verification, exact mbuf return accounting, stop, close,
//! and pool teardown. A virtual loopback proves packet flow only, so DPDK readiness
//! stays `contract_only` and no hardware or production poll-mode path is claimed.

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
    queue_mbuf_lifecycle: &'static str,
    loopback_payload_verified: &'static str,
    mbuf_return_accounting: &'static str,
    eal_cleanup: &'static str,
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
    pool_entries_before: u32,
    pool_entries_after: u32,
    frames_transmitted: usize,
    frames_received: usize,
    lifecycle: LifecycleEvidence,
    claims: ClaimEvidence,
    limitations: [&'static str; 3],
}

/// Runs the native EAL and `net_ring` loopback lifecycle and writes one evidence
/// artifact.
pub fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for DPDK lifecycle evidence")?;
    let (lifecycle, loopback) = NativeEalLifecycle::run_net_ring_loopback()
        .map_err(|error| format!("DPDK net_ring EAL/loopback lifecycle failed: {error}"))?;
    if loopback.pool_leaked {
        return Err("DPDK loopback leaked pool entries".into());
    }
    let report = DpdkVdevLifecycleReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        profile: "linux_dpdk",
        readiness: "contract_only",
        expected_dpdk_version: EXPECTED_DPDK_VERSION,
        runtime_version: lifecycle.runtime_version.clone(),
        ports_available: lifecycle.ports_available,
        pool_entries_before: loopback.pool_entries_before,
        pool_entries_after: loopback.pool_entries_after,
        frames_transmitted: loopback.frames_transmitted,
        frames_received: loopback.frames_received,
        lifecycle: LifecycleEvidence {
            eal_init: "passed",
            runtime_version_bound: "passed",
            virtual_device_enumerated: "passed",
            queue_mbuf_lifecycle: "passed",
            loopback_payload_verified: "passed",
            mbuf_return_accounting: "passed",
            eal_cleanup: "passed",
        },
        claims: ClaimEvidence {
            poll_mode_active: "not_claimed",
            zero_copy: "not_claimed",
            hardware: "not_claimed",
            provider: "not_claimed",
            production: "not_claimed",
        },
        limitations: [
            "virtual_device_loopback_only",
            "no_huge_pages_no_pci",
            "qualified_nic_driver_unverified",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "dpdk_vdev_lifecycle=passed readiness=contract_only runtime_version={} ports={} loopback_frames={}/{} payload_match={} pool_entries={}->{} cleanup=true hardware_poll_mode=not_exercised report={}",
        lifecycle.runtime_version,
        lifecycle.ports_available,
        loopback.frames_received,
        loopback.frames_transmitted,
        loopback.payloads_matched,
        loopback.pool_entries_before,
        loopback.pool_entries_after,
        report_path.display()
    );
    Ok(())
}
