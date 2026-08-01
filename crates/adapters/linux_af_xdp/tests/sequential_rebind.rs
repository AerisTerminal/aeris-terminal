//! Privileged regression test for sequential `AF_XDP` rebinding of one queue.
//!
//! `xsk_socket__delete` deliberately leaves the file descriptor open when the socket
//! shares it with its UMEM, which is the case for the first socket on a UMEM. Only
//! `xsk_umem__delete` closes that descriptor, and it refuses with `EBUSY` while the
//! socket refcount is non-zero. Teardown order therefore decides whether the kernel
//! releases the queue binding, and a regression reappears as `EBUSY` on the next open.
//!
//! The test is skipped unless `AXIUSFLOW_AF_XDP_TEST_INTERFACE` names an interface,
//! because it needs a real queue and elevated privileges.
#![cfg(all(target_os = "linux", feature = "native-copy"))]

use axiusflow_linux_af_xdp_adapter::{AfXdpConfig, AfXdpCopyDriver};
use axiusflow_transport::{DriverLifecycle, IngestDriver, QueueBinding};
use std::{env, num::NonZeroUsize};

const FRAME_COUNT: usize = 64;
const MAXIMUM_FRAME_BYTES: usize = 2_048;
const LIFECYCLES: usize = 3;

fn limit(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test limit is non-zero")
}

/// Resolves the privileged test interface, or `None` when the lane is not requested.
///
/// A missing interface normally skips, because these tests need a real queue and elevated
/// privileges. Setting `AXIUSFLOW_AF_XDP_REQUIRE_PRIVILEGED=1` converts that skip into a
/// failure so a privileged lane cannot silently degrade into a passing no-op.
fn test_interface() -> Option<String> {
    match env::var("AXIUSFLOW_AF_XDP_TEST_INTERFACE") {
        Ok(interface) if !interface.trim().is_empty() => Some(interface),
        _ => {
            assert!(
                env::var("AXIUSFLOW_AF_XDP_REQUIRE_PRIVILEGED").as_deref() != Ok("1"),
                "AXIUSFLOW_AF_XDP_REQUIRE_PRIVILEGED=1 demands a privileged run, but \
                 AXIUSFLOW_AF_XDP_TEST_INTERFACE is unset or empty"
            );
            eprintln!("skipped: AXIUSFLOW_AF_XDP_TEST_INTERFACE is not set");
            None
        }
    }
}

fn binding() -> QueueBinding {
    QueueBinding {
        queue_id: 0,
        maximum_batch_items: limit(2),
        maximum_frame_bytes: limit(MAXIMUM_FRAME_BYTES),
    }
}

/// Proves a released queue can be bound again without an `EBUSY` regression.
#[test]
fn queue_can_be_rebound_after_shutdown() {
    let Some(interface) = test_interface() else {
        return;
    };

    for lifecycle in 1..=LIFECYCLES {
        let config = AfXdpConfig::try_new(
            &interface,
            0,
            limit(FRAME_COUNT),
            limit(MAXIMUM_FRAME_BYTES),
        )
        .expect("a bounded AF_XDP configuration is accepted");
        let mut driver = AfXdpCopyDriver::try_new(config)
            .unwrap_or_else(|error| panic!("lifecycle {lifecycle} authorization failed: {error}"));

        driver
            .bind_queue(binding())
            .unwrap_or_else(|error| panic!("lifecycle {lifecycle} queue bind failed: {error}"));
        driver.start().unwrap_or_else(|error| {
            panic!(
                "lifecycle {lifecycle} activation failed, which indicates the previous lifecycle \
                 leaked its queue binding: {error}"
            )
        });
        assert_eq!(driver.health().lifecycle, DriverLifecycle::Running);

        driver
            .shutdown()
            .unwrap_or_else(|error| panic!("lifecycle {lifecycle} shutdown failed: {error}"));
        assert_eq!(driver.health().lifecycle, DriverLifecycle::Stopped);
        assert_eq!(
            driver.health().queued_frames,
            0,
            "lifecycle {lifecycle} must own no descriptors after shutdown"
        );
    }
}

/// Proves an abandoned batch still releases the queue for the next lifecycle.
#[test]
fn queue_can_be_rebound_after_abandoned_batch() {
    let Some(interface) = test_interface() else {
        return;
    };

    let config = AfXdpConfig::try_new(
        &interface,
        0,
        limit(FRAME_COUNT),
        limit(MAXIMUM_FRAME_BYTES),
    )
    .expect("a bounded AF_XDP configuration is accepted");
    let mut driver = AfXdpCopyDriver::try_new(config).expect("authorization succeeds");
    driver.bind_queue(binding()).expect("queue 0 binds");
    driver.start().expect("activation succeeds");

    let batch = driver
        .receive_batch()
        .expect("a receive poll returns a batch");
    drop(batch);
    driver
        .shutdown()
        .expect("shutdown succeeds after abandonment");
    assert_eq!(driver.health().abandoned_batches, 1);
    drop(driver);

    let config = AfXdpConfig::try_new(
        &interface,
        0,
        limit(FRAME_COUNT),
        limit(MAXIMUM_FRAME_BYTES),
    )
    .expect("a bounded AF_XDP configuration is accepted");
    let mut next = AfXdpCopyDriver::try_new(config).expect("authorization succeeds again");
    next.bind_queue(binding()).expect("queue 0 binds again");
    next.start().expect(
        "activation after an abandoned batch must succeed; EBUSY here means the abandoned \
         lifecycle leaked its queue binding",
    );
    next.shutdown().expect("shutdown succeeds");
}

/// Localizes an `EBUSY` regression to either a leaked descriptor or kernel queue state.
///
/// `xsk_umem__delete` closes the descriptor that the first socket shares with its UMEM,
/// but it refuses with `EBUSY` while the socket refcount is non-zero. A descriptor that
/// survives shutdown therefore proves the UMEM was still referenced, which is the
/// difference between a process-local ownership defect and persistent kernel state.
#[test]
fn shutdown_releases_every_descriptor_it_opened() {
    let Some(interface) = test_interface() else {
        return;
    };

    let open_descriptors = || {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd is readable on Linux")
            .count()
    };

    let describe_descriptors = || -> std::collections::BTreeMap<String, String> {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc/self/fd is readable on Linux")
            .filter_map(Result::ok)
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let target = std::fs::read_link(entry.path()).map_or_else(
                    |error| format!("<unreadable: {error}>"),
                    |path| path.to_string_lossy().into_owned(),
                );
                let metadata = std::fs::read_to_string(format!("/proc/self/fdinfo/{name}"))
                    .unwrap_or_default()
                    .lines()
                    .filter(|line| {
                        line.starts_with("map_type:")
                            || line.starts_with("map_id:")
                            || line.starts_with("map_name:")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let description = if metadata.is_empty() {
                    target
                } else {
                    format!("{target} ({metadata})")
                };
                (name, description)
            })
            .collect()
    };

    let baseline = open_descriptors();
    let before = describe_descriptors();

    let config = AfXdpConfig::try_new(
        &interface,
        0,
        limit(FRAME_COUNT),
        limit(MAXIMUM_FRAME_BYTES),
    )
    .expect("a bounded AF_XDP configuration is accepted");
    let mut driver = AfXdpCopyDriver::try_new(config).expect("authorization succeeds");
    driver.bind_queue(binding()).expect("queue 0 binds");
    driver.start().expect("activation succeeds");

    let active = open_descriptors();
    assert!(
        active > baseline,
        "an active AF_XDP socket must hold at least one descriptor"
    );

    driver.shutdown().expect("shutdown succeeds");
    drop(driver);

    let settled = open_descriptors();
    if settled != baseline {
        let after = describe_descriptors();
        for (name, target) in &after {
            if !before.contains_key(name) {
                eprintln!("leaked descriptor {name} -> {target}");
            }
        }
    }
    assert_eq!(
        settled,
        baseline,
        "shutdown leaked {} descriptor(s); xsk_umem__delete only closes the shared descriptor \
         when the socket refcount has already reached zero",
        settled.saturating_sub(baseline)
    );
}
