//! Executable Stage 1 contract/lifecycle smoke check.
//!
//! This proves native portable loopback semantics on every host and native tuned
//! Linux fixture conformance on Linux. It does not certify accelerated, hardware,
//! provider, renderer-submission, or presented-pixel readiness.

#[cfg(all(target_os = "linux", feature = "af-xdp-copy"))]
mod af_xdp_copy;
#[cfg(all(target_os = "linux", feature = "af-xdp-copy"))]
mod af_xdp_fuzz;
#[cfg(all(target_os = "linux", feature = "af-xdp-copy"))]
mod af_xdp_replay;
mod auth_service_boundary;
mod authorization_boundary;
#[cfg(all(target_os = "linux", feature = "redpanda"))]
mod clickhouse_projection;
mod coinbase_live;
#[cfg(all(target_os = "linux", feature = "dpdk-native"))]
mod dpdk_lifecycle;
mod embedded_store_spike;
mod entitlement_enforcement;
mod evidence_report;
mod feed_profile_matrix;
mod live_data_plane;
mod postgres_persistence;
#[cfg(all(target_os = "linux", feature = "quic"))]
mod quic_prototype;
#[cfg(all(target_os = "linux", feature = "redpanda"))]
mod raw_capture;
#[cfg(all(target_os = "linux", feature = "redpanda"))]
mod redpanda_branch;

use axiusflow_observability::{
    BoundedLatencyRecorder, LatencyBoundary, LatencySample, LatencyTimestampChain,
};
use axiusflow_testing::{
    ConformanceHarnessError, ConformanceOutcome, SnapshotIntegrityOutcome,
    deterministic_ingest_corpus, ingest_outcomes_semantically_equivalent,
    run_binary_market_stream_conformance, run_direct_market_bar_wire_conformance,
    run_ingest_conformance, run_ingest_conformance_after_start,
    run_latest_state_snapshot_conformance, run_realtime_recovery_conformance,
    run_replay_to_gpui_host_benchmark,
};
use axiusflow_transport::{
    ActivationRequest, ActiveIngestMode, DriverLifecycle, FixtureIngestDriver, IngestDriver,
    IngestProfile, QueueBinding, ReadinessManifest, ReadinessState, ReceiveBatch,
    software_fixture_capabilities,
};
use std::{error::Error, net::UdpSocket, num::NonZeroUsize, time::Duration};

fn run_fixture(mut driver: FixtureIngestDriver) -> Result<Vec<ConformanceOutcome>, Box<dyn Error>> {
    let outcomes = run_ingest_conformance(&mut driver)?;
    let health = driver.health();
    if health.lifecycle != DriverLifecycle::Stopped
        || health.released_batches == 0
        || health.abandoned_batches != 0
    {
        return Err("receive batches did not follow startup/release/shutdown lifecycle".into());
    }
    Ok(outcomes)
}

fn verify_abandoned_batch(mut driver: FixtureIngestDriver) -> Result<(), Box<dyn Error>> {
    driver.bind_queue(QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::MIN,
        maximum_frame_bytes: NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
    })?;
    driver.start()?;
    drop(driver.receive_batch()?);
    let health = driver.health();
    if health.abandoned_batches != 1 || health.released_batches != 0 {
        return Err("dropped receive batch was not recorded as abandoned".into());
    }
    driver.shutdown()?;
    Ok(())
}

fn run_portable_loopback() -> Result<Vec<ConformanceOutcome>, Box<dyn Error>> {
    let corpus = deterministic_ingest_corpus();
    let mut driver = axiusflow_portable_network_adapter::PortableSocketDriver::try_new(
        axiusflow_portable_network_adapter::PortableSocketConfig::loopback()?,
    )?;
    if driver.capabilities().profile != IngestProfile::PortableSocket
        || driver.capabilities().supported_modes != vec![ActiveIngestMode::PortableSocket]
        || driver.capabilities().zero_copy_verified
        || driver.capabilities().timestamp_sources
            != vec![axiusflow_transport::TimestampSource::SocketSoftware]
    {
        return Err("portable socket capabilities overclaimed or omitted active behavior".into());
    }
    let outcomes = run_ingest_conformance_after_start(&mut driver, |driver| {
        let destination = driver
            .local_addr()
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        let sender = UdpSocket::bind("127.0.0.1:0")
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        for frame in &corpus {
            let sent = sender
                .send_to(&frame.bytes, destination)
                .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
            if sent != frame.bytes.len() {
                return Err(ConformanceHarnessError::Driver(
                    "loopback sender reported a partial datagram".to_string(),
                ));
            }
        }
        Ok(())
    })?;
    let health = driver.health();
    if health.lifecycle != DriverLifecycle::Stopped
        || health.released_batches == 0
        || health.abandoned_batches != 0
        || health.active_mode != ActiveIngestMode::PortableSocket
    {
        return Err("portable socket lifecycle health did not match conformance".into());
    }
    Ok(outcomes)
}

fn verify_portable_market_bar_packet_to_origin()
-> Result<axiusflow_testing::MarketBarPacketOriginConformance, Box<dyn Error>> {
    let corpus = axiusflow_testing::deterministic_market_bar_packet_corpus()?;
    let mut fixture = axiusflow_portable_network_adapter::fixture_driver(corpus.frames().to_vec())?;
    let fixture_report =
        axiusflow_testing::run_market_bar_packet_to_origin_conformance_after_start(
            &mut fixture,
            &corpus,
            |_, _| Ok(()),
        )?;

    let mut portable = axiusflow_portable_network_adapter::PortableSocketDriver::try_new(
        axiusflow_portable_network_adapter::PortableSocketConfig::loopback()?,
    )?;
    let portable_report =
        axiusflow_testing::run_market_bar_packet_to_origin_conformance_after_start(
            &mut portable,
            &corpus,
            |driver, frames| {
                let destination = driver
                    .local_addr()
                    .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
                let sender = UdpSocket::bind("127.0.0.1:0")
                    .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
                for frame in frames {
                    let sent = sender
                        .send_to(&frame.bytes, destination)
                        .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
                    if sent != frame.bytes.len() {
                        return Err(ConformanceHarnessError::Driver(
                            "market-bar loopback sender reported a partial datagram".to_string(),
                        ));
                    }
                }
                Ok(())
            },
        )?;
    if !fixture_report.semantically_equivalent(&portable_report) {
        return Err(
            "portable market-bar packet-to-Origin state diverged from fixture semantics".into(),
        );
    }
    Ok(portable_report)
}

#[cfg(target_os = "linux")]
fn verify_tuned_market_bar_packet_to_origin(
    portable_report: &axiusflow_testing::MarketBarPacketOriginConformance,
) -> Result<bool, Box<dyn Error>> {
    let corpus = axiusflow_testing::deterministic_market_bar_packet_corpus()?;
    let config = axiusflow_linux_socket_network_adapter::TunedLinuxSocketConfig::loopback()?;
    let mut tuned =
        axiusflow_linux_socket_network_adapter::TunedLinuxSocketDriver::try_new(config)?;
    let tuned_report = axiusflow_testing::run_market_bar_packet_to_origin_conformance_after_start(
        &mut tuned,
        &corpus,
        |driver, frames| {
            let destination = driver
                .local_addr()
                .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
            let sender = UdpSocket::bind("127.0.0.1:0")
                .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
            for frame in frames {
                let sent = sender
                    .send_to(&frame.bytes, destination)
                    .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
                if sent != frame.bytes.len() {
                    return Err(ConformanceHarnessError::Driver(
                        "tuned market-bar loopback sender reported a partial datagram".to_string(),
                    ));
                }
            }
            Ok(())
        },
    )?;
    if tuned.applied_receive_buffer_bytes().is_none()
        || !portable_report.semantically_equivalent(&tuned_report)
    {
        return Err(
            "tuned Linux market-bar packet-to-Origin state diverged from portable semantics".into(),
        );
    }
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
fn verify_tuned_market_bar_packet_to_origin(
    portable_report: &axiusflow_testing::MarketBarPacketOriginConformance,
) -> bool {
    let _ = portable_report;
    false
}

fn verify_portable_lifecycle() -> Result<(), Box<dyn Error>> {
    let binding = QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::MIN,
        maximum_frame_bytes: NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
    };
    let payload = deterministic_ingest_corpus()
        .into_iter()
        .next()
        .ok_or("deterministic corpus was empty")?
        .bytes;
    let mut driver = axiusflow_portable_network_adapter::PortableSocketDriver::try_new(
        axiusflow_portable_network_adapter::PortableSocketConfig::loopback()?,
    )?;
    driver.bind_queue(binding)?;
    driver.start()?;
    let sender = UdpSocket::bind("127.0.0.1:0")?;
    sender.send_to(&payload, driver.local_addr()?)?;
    let abandoned = driver.receive_batch()?;
    if abandoned.frame_count() != 1 {
        return Err("portable abandonment fixture did not receive one frame".into());
    }
    drop(abandoned);
    driver.shutdown()?;

    driver.bind_queue(binding)?;
    driver.start()?;
    sender.send_to(&payload, driver.local_addr()?)?;
    let released = driver.receive_batch()?;
    if released.frame_count() != 1 {
        return Err("portable reconnect fixture did not receive one frame".into());
    }
    released.release();
    driver.shutdown()?;
    let health = driver.health();
    if health.abandoned_batches != 1
        || health.released_batches != 1
        || health.lifecycle != DriverLifecycle::Stopped
    {
        return Err("portable abandonment/reconnect counters were incorrect".into());
    }

    let overflow_config = axiusflow_portable_network_adapter::PortableSocketConfig::try_new(
        "127.0.0.1:0".parse()?,
        Duration::from_millis(100),
        Duration::from_millis(1),
        NonZeroUsize::MIN,
        NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
    )?;
    let mut overflow_driver =
        axiusflow_portable_network_adapter::PortableSocketDriver::try_new(overflow_config)?;
    overflow_driver.bind_queue(QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::MIN,
        maximum_frame_bytes: NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
    })?;
    overflow_driver.start()?;
    sender.send_to(&[0_u8; 128], overflow_driver.local_addr()?)?;
    let overflow_batch = overflow_driver.receive_batch()?;
    let overflow = overflow_batch.overflow();
    if overflow_batch.frame_count() != 0 || overflow.dropped_frames != 1 {
        return Err("portable oversized datagram was not reported as overflow".into());
    }
    overflow_batch.release();
    overflow_driver.shutdown()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_tuned_profile(fixture_baseline: &[ConformanceOutcome]) -> Result<bool, Box<dyn Error>> {
    let config = axiusflow_linux_socket_network_adapter::TunedLinuxSocketConfig::loopback()?;
    let corpus = deterministic_ingest_corpus();
    let mut driver =
        axiusflow_linux_socket_network_adapter::TunedLinuxSocketDriver::try_new(config)?;
    if driver.permit().readiness() != ReadinessState::FixtureValidated
        || driver.permit().evidence_id() != "tuned_linux_socket_loopback_conformance"
        || driver.capabilities().profile != IngestProfile::TunedLinuxSocket
        || driver.capabilities().supported_modes != vec![ActiveIngestMode::TunedLinuxSocket]
        || driver.capabilities().zero_copy_verified
        || driver.capabilities().timestamp_sources
            != vec![axiusflow_transport::TimestampSource::SocketSoftware]
    {
        return Err("tuned Linux capabilities overclaimed or omitted active behavior".into());
    }
    let outcomes = run_ingest_conformance_after_start(&mut driver, |driver| {
        let destination = driver
            .local_addr()
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        let sender = UdpSocket::bind("127.0.0.1:0")
            .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        for frame in &corpus {
            sender
                .send_to(&frame.bytes, destination)
                .map_err(|error| ConformanceHarnessError::Driver(error.to_string()))?;
        }
        Ok(())
    })?;
    if driver.applied_receive_buffer_bytes().is_none()
        || driver.health().lifecycle != DriverLifecycle::Stopped
        || !ingest_outcomes_semantically_equivalent(fixture_baseline, &outcomes)
    {
        return Err("tuned Linux loopback conformance diverged".into());
    }
    verify_tuned_linux_lifecycle(config)?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn verify_tuned_linux_lifecycle(
    config: axiusflow_linux_socket_network_adapter::TunedLinuxSocketConfig,
) -> Result<(), Box<dyn Error>> {
    let binding = QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::MIN,
        maximum_frame_bytes: NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
    };
    let payload = deterministic_ingest_corpus()
        .into_iter()
        .next()
        .ok_or("deterministic corpus was empty")?
        .bytes;
    let mut driver =
        axiusflow_linux_socket_network_adapter::TunedLinuxSocketDriver::try_new(config)?;
    driver.bind_queue(binding)?;
    driver.start()?;
    let sender = UdpSocket::bind("127.0.0.1:0")?;
    sender.send_to(&payload, driver.local_addr()?)?;
    let abandoned = driver.receive_batch()?;
    if abandoned.frame_count() != 1 {
        return Err("tuned Linux abandonment fixture did not receive one frame".into());
    }
    drop(abandoned);
    driver.shutdown()?;
    driver.bind_queue(binding)?;
    driver.start()?;
    sender.send_to(&payload, driver.local_addr()?)?;
    let released = driver.receive_batch()?;
    if released.frame_count() != 1 {
        return Err("tuned Linux reconnect fixture did not receive one frame".into());
    }
    released.release();
    driver.shutdown()?;
    let health = driver.health();
    if health.abandoned_batches != 1
        || health.released_batches != 1
        || health.lifecycle != DriverLifecycle::Stopped
    {
        return Err("tuned Linux abandonment/reconnect counters were incorrect".into());
    }
    verify_tuned_linux_overflow(&sender)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_tuned_linux_overflow(sender: &UdpSocket) -> Result<(), Box<dyn Error>> {
    let config = axiusflow_linux_socket_network_adapter::TunedLinuxSocketConfig::try_new(
        "127.0.0.1:0".parse()?,
        Duration::from_millis(100),
        Duration::from_millis(1),
        NonZeroUsize::MIN,
        NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(65_536).unwrap_or(NonZeroUsize::MIN),
    )?;
    let mut driver =
        axiusflow_linux_socket_network_adapter::TunedLinuxSocketDriver::try_new(config)?;
    driver.bind_queue(QueueBinding {
        queue_id: 0,
        maximum_batch_items: NonZeroUsize::MIN,
        maximum_frame_bytes: NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
    })?;
    driver.start()?;
    sender.send_to(&[0_u8; 128], driver.local_addr()?)?;
    let batch = driver.receive_batch()?;
    let overflow = batch.overflow();
    if batch.frame_count() != 0 || overflow.dropped_frames != 1 {
        return Err("tuned Linux oversized datagram was not reported as overflow".into());
    }
    batch.release();
    driver.shutdown()?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_tuned_profile(fixture_baseline: &[ConformanceOutcome]) -> Result<bool, Box<dyn Error>> {
    let config = axiusflow_linux_socket_network_adapter::TunedLinuxSocketConfig::loopback()?;
    let mut driver =
        axiusflow_linux_socket_network_adapter::TunedLinuxSocketDriver::unavailable(config);
    if !driver.capabilities().supported_modes.is_empty()
        || driver.start().is_ok()
        || driver.health().active_mode != ActiveIngestMode::Unavailable
        || driver.health().lifecycle != DriverLifecycle::Unsupported
    {
        return Err("tuned Linux mode did not reject non-Linux activation".into());
    }
    let _ = fixture_baseline;
    Ok(false)
}

fn verify_accelerated_activation_gates() -> Result<(), Box<dyn Error>> {
    let af_xdp_config = axiusflow_linux_af_xdp_adapter::AfXdpConfig::try_new(
        "lo",
        0,
        NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
    )?;
    let mut af_xdp = axiusflow_linux_af_xdp_adapter::AfXdpDriver::unavailable(&af_xdp_config);
    let af_xdp_prerequisites = af_xdp.prerequisites().clone();
    let expected_af_xdp_dependency = if axiusflow_linux_af_xdp_adapter::NATIVE_DEPENDENCY_SELECTED {
        axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Present
    } else {
        axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::NotSelected
    };
    if af_xdp_prerequisites.native_dependency != expected_af_xdp_dependency
        || af_xdp_prerequisites.copy_mode
            != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::NotExercised
        || af_xdp_prerequisites.zero_copy
            != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Unverified
        || (af_xdp_prerequisites.linux_target
            && (af_xdp_prerequisites.interface
                != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Present
                || af_xdp_prerequisites.receive_queue
                    != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Present))
        || (!af_xdp_prerequisites.linux_target
            && (af_xdp_prerequisites.interface
                != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Missing
                || af_xdp_prerequisites.receive_queue
                    != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Missing
                || af_xdp_prerequisites.kernel_btf
                    != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Missing
                || af_xdp_prerequisites.bpf_filesystem
                    != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Missing
                || af_xdp_prerequisites.xdp_diagnostics
                    != axiusflow_linux_af_xdp_adapter::AfXdpEvidenceStatus::Missing))
        || !af_xdp.capabilities().supported_modes.is_empty()
        || af_xdp.start().is_ok()
        || af_xdp.health().active_mode != ActiveIngestMode::Unavailable
    {
        return Err("AF_XDP activation gate overclaimed native readiness".into());
    }

    let dpdk_config = axiusflow_linux_dpdk_adapter::DpdkConfig::try_new(
        0,
        0,
        NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(2_048).unwrap_or(NonZeroUsize::MIN),
    )?;
    let mut dpdk = axiusflow_linux_dpdk_adapter::DpdkDriver::unavailable(dpdk_config);
    let dpdk_prerequisites = dpdk.prerequisites().clone();
    let expected_dpdk_dependency = if axiusflow_linux_dpdk_adapter::NATIVE_DEPENDENCY_SELECTED {
        axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::Present
    } else {
        axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::NotSelected
    };
    if dpdk_prerequisites.native_dependency != expected_dpdk_dependency
        || dpdk_prerequisites.software_device
            != axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::NotExercised
        || dpdk_prerequisites.poll_mode_driver
            != axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::Unverified
        || dpdk_prerequisites.huge_pages_free > dpdk_prerequisites.huge_pages_total
        || (!dpdk_prerequisites.linux_target
            && (dpdk_prerequisites.huge_pages_total > 0
                || dpdk_prerequisites.huge_pages_free > 0
                || dpdk_prerequisites.vfio_driver
                    != axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::Missing
                || dpdk_prerequisites.vfio_control
                    != axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::Missing
                || dpdk_prerequisites.pkg_config_metadata
                    != axiusflow_linux_dpdk_adapter::DpdkEvidenceStatus::Missing))
        || !dpdk.capabilities().supported_modes.is_empty()
        || dpdk.start().is_ok()
        || dpdk.health().active_mode != ActiveIngestMode::Unavailable
    {
        return Err("DPDK activation gate overclaimed native readiness".into());
    }

    println!(
        "acceleration_prerequisites=observed af_xdp_linux_target={} af_xdp_interface={:?} af_xdp_receive_queue={:?} af_xdp_kernel_btf={:?} af_xdp_bpffs={:?} af_xdp_diagnostics={:?} af_xdp_native_dependency={:?} dpdk_linux_target={} dpdk_huge_pages_total={} dpdk_huge_pages_free={} dpdk_vfio_driver={:?} dpdk_vfio_control={:?} dpdk_pkg_config_metadata={:?} dpdk_native_dependency={:?} privileged_lifecycle_exercised=false",
        af_xdp_prerequisites.linux_target,
        af_xdp_prerequisites.interface,
        af_xdp_prerequisites.receive_queue,
        af_xdp_prerequisites.kernel_btf,
        af_xdp_prerequisites.bpf_filesystem,
        af_xdp_prerequisites.xdp_diagnostics,
        af_xdp_prerequisites.native_dependency,
        dpdk_prerequisites.linux_target,
        dpdk_prerequisites.huge_pages_total,
        dpdk_prerequisites.huge_pages_free,
        dpdk_prerequisites.vfio_driver,
        dpdk_prerequisites.vfio_control,
        dpdk_prerequisites.pkg_config_metadata,
        dpdk_prerequisites.native_dependency,
    );
    Ok(())
}

fn verify_latency_recorder() -> Result<(), Box<dyn Error>> {
    let mut vocabulary = LatencyTimestampChain::new();
    let mut previous_name = None;
    for (index, boundary) in LatencyBoundary::ALL.into_iter().enumerate() {
        let name = boundary.name();
        if name.is_empty() || previous_name == Some(name) {
            return Err("latency boundary vocabulary was incomplete".into());
        }
        vocabulary.set(boundary, i64::try_from(index).unwrap_or(i64::MAX));
        previous_name = Some(name);
    }
    for pair in LatencyBoundary::ALL.windows(2) {
        vocabulary.try_sample(pair[0], pair[1])?;
    }

    let capacity = NonZeroUsize::new(1_000).unwrap_or(NonZeroUsize::MIN);
    let mut recorder = BoundedLatencyRecorder::new(capacity);
    for elapsed in 1_i64..=1_000 {
        let mut chain = LatencyTimestampChain::new();
        chain.set(LatencyBoundary::ClientReceive, 10_000);
        chain.set(
            LatencyBoundary::ModelApply,
            10_000_i64.saturating_add(elapsed),
        );
        recorder.try_record_chain(
            &chain,
            LatencyBoundary::ClientReceive,
            LatencyBoundary::ModelApply,
        )?;
    }
    let overflow = LatencySample::try_new(
        LatencyBoundary::ClientReceive,
        LatencyBoundary::ModelApply,
        0,
        1_001,
    )?;
    if recorder.try_record(overflow).is_ok() {
        return Err("bounded latency recorder accepted an over-capacity sample".into());
    }
    let report = recorder.report(LatencyBoundary::ClientReceive, LatencyBoundary::ModelApply)?;
    if report.sample_count != 1_000
        || report.p50_nanos != 500
        || report.p95_nanos != 950
        || report.p99_nanos != 990
        || report.p99_9_nanos != 999
        || report.maximum_nanos != 1_000
        || recorder.sample_count() != 1_000
        || recorder.rejected_samples() != 1
    {
        return Err("latency percentile or bounded-overflow report diverged".into());
    }
    Ok(())
}

fn verify_stage_2_read_only_contracts() -> Result<(), Box<dyn Error>> {
    if !run_binary_market_stream_conformance()?.is_complete() {
        return Err("binary market stream fixture did not satisfy its contract".into());
    }
    if !run_direct_market_bar_wire_conformance()?.is_complete() {
        return Err("direct canonical market-bar wire fixture did not satisfy its contract".into());
    }
    if !run_latest_state_snapshot_conformance()?.is_complete() {
        return Err("canonical latest-state snapshot fixture did not satisfy its contract".into());
    }
    if !axiusflow_testing::run_snapshot_chunk_conformance()?.is_complete() {
        return Err("snapshot chunk fixture did not satisfy its contract".into());
    }
    if !axiusflow_testing::run_market_bar_client_model_conformance()?.is_complete() {
        return Err("market-bar client model fixture did not satisfy its contract".into());
    }
    if !axiusflow_testing::run_websocket_loopback_conformance()?.is_complete() {
        return Err("bounded WebSocket loopback fixture did not satisfy its contract".into());
    }
    let lifecycle = axiusflow_testing::run_plain_loopback_lifecycle_conformance()
        .map_err(|error| format!("plain-loopback lifecycle fixture failed: {error}"))?;
    if !lifecycle.is_complete() {
        return Err(format!(
            "plain-loopback connection lifecycle did not satisfy its contract: checks={:#05x}",
            lifecycle.passed_checks()
        )
        .into());
    }
    let runtime_chart = axiusflow_testing::run_plain_loopback_runtime_chart_conformance()
        .map_err(|error| format!("plain-loopback runtime chart fixture failed: {error}"))?;
    if !runtime_chart.is_complete() {
        return Err(format!(
            "plain-loopback background runtime chart bridge did not satisfy its contract: checks={:#05x}",
            runtime_chart.passed_checks()
        )
        .into());
    }
    if !axiusflow_chart_integration::run_chart_bridge_recovery_conformance()?.is_complete() {
        return Err("chart bridge recovery lifecycle did not satisfy its contract".into());
    }
    Ok(())
}

fn verify_fixture_target_equivalence(
    fixture_baseline: &[ConformanceOutcome],
) -> Result<(), Box<dyn Error>> {
    let targets: [(&str, Vec<ConformanceOutcome>); 5] = [
        (
            "windows_software_fixture",
            run_fixture(axiusflow_windows_network_adapter::fixture_driver(
                deterministic_ingest_corpus(),
            )?)?,
        ),
        (
            "macos_software_fixture",
            run_fixture(axiusflow_macos_network_adapter::fixture_driver(
                deterministic_ingest_corpus(),
            )?)?,
        ),
        (
            "tuned_linux_software_fixture",
            run_fixture(axiusflow_linux_socket_network_adapter::fixture_driver(
                deterministic_ingest_corpus(),
            )?)?,
        ),
        (
            "linux_af_xdp_software_fixture",
            run_fixture(axiusflow_linux_af_xdp_adapter::fixture_driver(
                deterministic_ingest_corpus(),
            )?)?,
        ),
        (
            "linux_dpdk_software_fixture",
            run_fixture(axiusflow_linux_dpdk_adapter::fixture_driver(
                deterministic_ingest_corpus(),
            )?)?,
        ),
    ];
    for (name, outcomes) in targets {
        if outcomes != fixture_baseline {
            return Err(format!("software contract target {name} was nondeterministic").into());
        }
    }
    Ok(())
}

fn verify_readiness_caps() -> Result<(), Box<dyn Error>> {
    let portable = axiusflow_portable_network_adapter::PortableSocketDriver::try_new(
        axiusflow_portable_network_adapter::PortableSocketConfig::loopback()?,
    )?;
    if portable.permit().readiness() != ReadinessState::FixtureValidated
        || portable.permit().active_mode() != ActiveIngestMode::PortableSocket
        || portable.permit().evidence_id()
            != "windows_macos_linux_same_revision_loopback_conformance"
    {
        return Err("portable socket fixture validation was not authorized by reviewed cross-platform evidence".into());
    }

    for profile in [
        IngestProfile::TunedLinuxSocket,
        IngestProfile::LinuxAfXdp,
        IngestProfile::LinuxDpdk,
    ] {
        let capabilities = software_fixture_capabilities(profile);
        let evidence_id = if matches!(
            profile,
            IngestProfile::LinuxAfXdp | IngestProfile::LinuxDpdk
        ) {
            "software_fixture_adapter"
        } else {
            "deterministic_packet_corpus"
        };
        if ReadinessManifest::authorize_embedded(&ActivationRequest {
            profile,
            requested: ReadinessState::FixtureValidated,
            active_mode: ActiveIngestMode::SoftwareFixture,
            evidence_id,
            capabilities: &capabilities,
        })
        .is_ok()
        {
            return Err(format!(
                "profile {profile:?} overclaimed fixture validation from a software fixture"
            )
            .into());
        }
    }
    Ok(())
}

fn verify_realtime_recovery() -> Result<(), Box<dyn Error>> {
    let recovery = run_realtime_recovery_conformance()?;
    if !recovery.stale_writer_rejected
        || recovery.direct_outcome != axiusflow_realtime::QueueOutcome::Enqueued
        || recovery.durable_outcome != axiusflow_realtime::QueueOutcome::SnapshotRequired
        || !recovery.durable_gap_visible
        || !recovery.recovered
        || recovery.snapshot_integrity != SnapshotIntegrityOutcome::CorruptionRejected
    {
        return Err("fencing/fanout/recovery conformance did not satisfy its contract".into());
    }
    Ok(())
}

fn print_benchmark(report: &axiusflow_testing::ReplayToGpuiBenchmarkReport) {
    println!(
        "replay_to_gpui_host_benchmark=passed samples={} warmup={} decoder_model_p50_ns={} decoder_model_p95_ns={} decoder_model_p99_ns={} decoder_model_p99_9_ns={} decoder_model_max_ns={} origin_frame_p50_ns={} origin_frame_p95_ns={} origin_frame_p99_ns={} origin_frame_p99_9_ns={} origin_frame_max_ns={} gpui_host_p50_ns={} gpui_host_p95_ns={} gpui_host_p99_ns={} gpui_host_p99_9_ns={} gpui_host_max_ns={} renderer_submission_measured=false presented_pixel=false",
        report.measurement_iterations,
        report.warmup_iterations,
        report.decoder_and_model.p50_nanos,
        report.decoder_and_model.p95_nanos,
        report.decoder_and_model.p99_nanos,
        report.decoder_and_model.p99_9_nanos,
        report.decoder_and_model.maximum_nanos,
        report.origin_frame.p50_nanos,
        report.origin_frame.p95_nanos,
        report.origin_frame.p99_nanos,
        report.origin_frame.p99_9_nanos,
        report.origin_frame.maximum_nanos,
        report.gpui_host.p50_nanos,
        report.gpui_host.p95_nanos,
        report.gpui_host.p99_nanos,
        report.gpui_host.p99_9_nanos,
        report.gpui_host.maximum_nanos,
    );
}

fn print_stage_1_status(
    tuned_linux_native: bool,
    tuned_linux_market_bar_origin: bool,
    outcomes: &[ConformanceOutcome],
) {
    let accepted = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, ConformanceOutcome::Accepted(_)))
        .count();
    println!(
        "stage_1_contract_smoke=passed portable_socket_loopback=passed portable_native_equivalence=true portable_market_bar_packet_to_origin=passed portable_packet_partition_fanout_origin=passed tuned_linux_native_loopback={} tuned_linux_market_bar_packet_to_origin={} tuned_linux_packet_partition_fanout_origin={} af_xdp_activation=explicitly_unavailable dpdk_activation=explicitly_unavailable latency_recorder=passed replay_to_gpui_host_benchmark=passed stage_2_binary_stream_fixture=passed stage_2_direct_canonical_market_bar_wire_fixture=passed stage_2_latest_state_snapshot_fixture=passed stage_2_snapshot_chunk_fixture=passed stage_2_client_model_fixture=passed stage_2_websocket_loopback_fixture=passed stage_2_plain_loopback_lifecycle_fixture=passed stage_2_plain_loopback_runtime_chart_fixture=passed stage_2_neutral_stream_chart_coordinator_fixture=passed actual_native_targets={} software_fixture_targets=6 corpus_outcomes={} canonical_events={} portable_readiness=fixture_validated tuned_linux_readiness=fixture_validated af_xdp_readiness=implemented dpdk_readiness=contract_only accelerated_native_equivalence=false connected_live=false websocket_loopback=true websocket_connection_owner_loopback=true websocket_background_runtime_loopback=true canonical_direct_fanout_market_bar_wire=true canonical_latest_state_fixture=true market_stream_runtime_port_loopback=true chart_stream_coordinator_loopback=true websocket_transport=false tls_certified=false auth_connected=false entitlement_enforced=false redpanda_deployed=false hardware_claims=false provider_claims=false",
        tuned_linux_native,
        tuned_linux_market_bar_origin,
        tuned_linux_market_bar_origin,
        usize::from(tuned_linux_native).saturating_add(1),
        outcomes.len(),
        accepted,
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let evidence_report_path = match evidence_report::requested_command()? {
        evidence_report::EvidenceCommand::Run { report_path } => report_path,
        evidence_report::EvidenceCommand::VerifySet { directory } => {
            evidence_report::verify_set(&directory)?;
            return Ok(());
        }
        other => return run_lane_command(other),
    };
    let fixture_baseline = run_fixture(axiusflow_portable_network_adapter::fixture_driver(
        deterministic_ingest_corpus(),
    )?)?;
    verify_abandoned_batch(axiusflow_portable_network_adapter::fixture_driver(
        deterministic_ingest_corpus(),
    )?)?;
    let portable_baseline = run_portable_loopback()?;
    verify_portable_lifecycle()?;
    let portable_market_bar_origin = verify_portable_market_bar_packet_to_origin()?;
    let tuned_linux_native = verify_tuned_profile(&fixture_baseline)?;
    #[cfg(target_os = "linux")]
    let tuned_linux_market_bar_origin =
        verify_tuned_market_bar_packet_to_origin(&portable_market_bar_origin)?;
    #[cfg(not(target_os = "linux"))]
    let tuned_linux_market_bar_origin =
        verify_tuned_market_bar_packet_to_origin(&portable_market_bar_origin);
    if tuned_linux_market_bar_origin != tuned_linux_native {
        return Err(
            "tuned Linux packet-to-Origin evidence disagreed with native activation".into(),
        );
    }
    verify_accelerated_activation_gates()?;
    verify_latency_recorder()?;
    let replay_to_gpui =
        run_replay_to_gpui_host_benchmark(NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN))?;
    if !replay_to_gpui.is_complete() {
        return Err("replay-to-GPUI host benchmark did not reach every honest boundary".into());
    }
    if !ingest_outcomes_semantically_equivalent(&fixture_baseline, &portable_baseline) {
        return Err("portable socket loopback diverged from deterministic packet semantics".into());
    }

    verify_fixture_target_equivalence(&fixture_baseline)?;
    verify_readiness_caps()?;
    verify_stage_2_read_only_contracts()?;
    verify_realtime_recovery()?;
    print_benchmark(&replay_to_gpui);
    print_stage_1_status(
        tuned_linux_native,
        tuned_linux_market_bar_origin,
        &fixture_baseline,
    );
    evidence_report::write(
        evidence_report_path.as_deref(),
        tuned_linux_native,
        tuned_linux_market_bar_origin,
        &fixture_baseline,
        &portable_market_bar_origin,
        &replay_to_gpui,
    )?;
    Ok(())
}

fn run_lane_command(command: evidence_report::EvidenceCommand) -> Result<(), Box<dyn Error>> {
    match command {
        evidence_report::EvidenceCommand::AfXdpCopy { .. } => run_af_xdp_copy_command(command),
        evidence_report::EvidenceCommand::AfXdpCopyFuzz {
            receive_interface,
            transmit_interface,
            seed,
            rounds,
            report_path,
        } => run_af_xdp_copy_fuzz(
            &receive_interface,
            &transmit_interface,
            seed,
            rounds,
            &report_path,
        ),
        evidence_report::EvidenceCommand::DpdkVdevLifecycle { report_path } => {
            run_dpdk_vdev_lifecycle(&report_path)
        }
        evidence_report::EvidenceCommand::RedpandaDurableBranch {
            brokers,
            report_path,
        } => run_redpanda_durable_branch(&brokers, &report_path),
        evidence_report::EvidenceCommand::S3RawCapture {
            host,
            port,
            access_key,
            secret_key,
            report_path,
        } => run_s3_raw_capture(&host, port, &access_key, &secret_key, &report_path),
        evidence_report::EvidenceCommand::ClickHouseProjections {
            host,
            port,
            report_path,
        } => run_clickhouse_projections(&host, port, &report_path),
        command @ evidence_report::EvidenceCommand::PostgresPersistence { .. } => {
            run_postgres_command(command)
        }
        command @ (evidence_report::EvidenceCommand::EmbeddedStoreSpike { .. }
        | evidence_report::EvidenceCommand::EmbeddedStoreCrashChild { .. }) => {
            run_embedded_store_command(command)
        }
        evidence_report::EvidenceCommand::QuicPrototype { report_path } => {
            run_quic_prototype(&report_path)
        }
        evidence_report::EvidenceCommand::AuthorizationBoundary {
            service_binary,
            report_path,
        } => authorization_boundary::run(&service_binary, &report_path),
        evidence_report::EvidenceCommand::MintJwks { workdir } => {
            entitlement_enforcement::mint_jwks(&workdir)
        }
        evidence_report::EvidenceCommand::EntitlementEnforcement { .. } => {
            run_entitlement_command(command)
        }
        evidence_report::EvidenceCommand::FeedProfileMatrix {
            live_provider,
            report_path,
        } => feed_profile_matrix::run(
            &live_provider,
            axiusflow_transport::FeedTransportClass::TlsTcpStream,
            &report_path,
        ),
        evidence_report::EvidenceCommand::LiveDataPlane {
            plane_address,
            product,
            window_seconds,
            report_path,
        } => live_data_plane::run(&plane_address, &product, window_seconds, &report_path),
        evidence_report::EvidenceCommand::CoinbaseLive {
            products,
            window_seconds,
            report_path,
        } => coinbase_live::run(&products, window_seconds, &report_path),
        evidence_report::EvidenceCommand::AuthService {
            service_binary,
            pg_host,
            pg_port,
            pg_user,
            pg_password,
            pg_database,
            report_path,
        } => auth_service_boundary::run(
            &service_binary,
            &pg_host,
            pg_port,
            &pg_user,
            &pg_password,
            &pg_database,
            &report_path,
        ),
        evidence_report::EvidenceCommand::Run { .. }
        | evidence_report::EvidenceCommand::VerifySet { .. } => {
            Err("lane command dispatch reached a non-lane command".into())
        }
    }
}

fn run_embedded_store_command(
    command: evidence_report::EvidenceCommand,
) -> Result<(), Box<dyn Error>> {
    match command {
        evidence_report::EvidenceCommand::EmbeddedStoreSpike { report_path } => {
            embedded_store_spike::run(&report_path)
        }
        evidence_report::EvidenceCommand::EmbeddedStoreCrashChild { backend, path } => {
            embedded_store_spike::run_crash_child(&backend, &path)
        }
        _ => Err("non-embedded command reached embedded-store dispatcher".into()),
    }
}

fn run_postgres_command(command: evidence_report::EvidenceCommand) -> Result<(), Box<dyn Error>> {
    let evidence_report::EvidenceCommand::PostgresPersistence {
        host,
        port,
        user,
        password,
        database,
        report_path,
    } = command
    else {
        return Err("non-PostgreSQL command reached PostgreSQL dispatcher".into());
    };
    postgres_persistence::run(&host, port, &user, &password, &database, &report_path)
        .map_err(|error| error.to_string().into())
}

#[cfg(all(target_os = "linux", feature = "af-xdp-copy"))]
fn run_af_xdp_copy(
    receive_interface: &str,
    transmit_interface: &str,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    af_xdp_copy::run(receive_interface, transmit_interface, report_path)
}

#[cfg(not(all(target_os = "linux", feature = "af-xdp-copy")))]
fn run_af_xdp_copy(
    receive_interface: &str,
    transmit_interface: &str,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let _ = (receive_interface, transmit_interface, report_path);
    Err("AF_XDP copy conformance requires Linux and the af-xdp-copy feature".into())
}

#[cfg(all(target_os = "linux", feature = "af-xdp-copy"))]
fn run_af_xdp_copy_fuzz(
    receive_interface: &str,
    transmit_interface: &str,
    seed: u64,
    rounds: u32,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    af_xdp_fuzz::run(
        receive_interface,
        transmit_interface,
        seed,
        rounds,
        report_path,
    )
}

#[cfg(not(all(target_os = "linux", feature = "af-xdp-copy")))]
fn run_af_xdp_copy_fuzz(
    receive_interface: &str,
    transmit_interface: &str,
    seed: u64,
    rounds: u32,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let _ = (
        receive_interface,
        transmit_interface,
        seed,
        rounds,
        report_path,
    );
    Err("AF_XDP copy data-path fuzzing requires Linux and the af-xdp-copy feature".into())
}

#[cfg(all(target_os = "linux", feature = "dpdk-native"))]
fn run_dpdk_vdev_lifecycle(report_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    dpdk_lifecycle::run(report_path)
}

#[cfg(not(all(target_os = "linux", feature = "dpdk-native")))]
fn run_dpdk_vdev_lifecycle(report_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let _ = report_path;
    Err("DPDK virtual-device lifecycle requires Linux and the dpdk-native feature".into())
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
fn run_redpanda_durable_branch(
    brokers: &str,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    redpanda_branch::run(brokers, report_path)
}

#[cfg(not(all(target_os = "linux", feature = "redpanda")))]
fn run_redpanda_durable_branch(
    brokers: &str,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let _ = (brokers, report_path);
    Err("Redpanda durable-branch conformance requires Linux and the redpanda feature".into())
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
fn run_s3_raw_capture(
    host: &str,
    port: u16,
    access_key: &str,
    secret_key: &str,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    raw_capture::run(host, port, access_key, secret_key, report_path)
}

#[cfg(not(all(target_os = "linux", feature = "redpanda")))]
fn run_s3_raw_capture(
    host: &str,
    port: u16,
    access_key: &str,
    secret_key: &str,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let _ = (host, port, access_key, secret_key, report_path);
    Err("raw S3 capture conformance requires Linux and the redpanda feature".into())
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
fn run_clickhouse_projections(
    host: &str,
    port: u16,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    clickhouse_projection::run(host, port, report_path)
}

#[cfg(not(all(target_os = "linux", feature = "redpanda")))]
fn run_clickhouse_projections(
    host: &str,
    port: u16,
    report_path: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let _ = (host, port, report_path);
    Err("ClickHouse projection conformance requires Linux and the redpanda feature".into())
}

#[cfg(all(target_os = "linux", feature = "quic"))]
fn run_quic_prototype(report_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    quic_prototype::run(report_path)
}

#[cfg(not(all(target_os = "linux", feature = "quic")))]
fn run_quic_prototype(report_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let _ = report_path;
    Err("QUIC prototype requires Linux and the quic feature".into())
}

fn run_af_xdp_copy_command(
    command: evidence_report::EvidenceCommand,
) -> Result<(), Box<dyn Error>> {
    match command {
        evidence_report::EvidenceCommand::AfXdpCopy {
            receive_interface,
            transmit_interface,
            report_path,
        } => run_af_xdp_copy(&receive_interface, &transmit_interface, &report_path),
        _ => Err("AF_XDP copy command dispatch mismatch".into()),
    }
}

fn run_entitlement_command(
    command: evidence_report::EvidenceCommand,
) -> Result<(), Box<dyn Error>> {
    match command {
        evidence_report::EvidenceCommand::EntitlementEnforcement {
            plane_address,
            workdir,
            resnapshot_seconds,
            report_path,
        } => {
            entitlement_enforcement::run(&plane_address, &workdir, resnapshot_seconds, &report_path)
        }
        _ => Err("entitlement command dispatch mismatch".into()),
    }
}
