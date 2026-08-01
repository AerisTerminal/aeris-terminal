mod tests {
    use crate::{
        PROFILE, PortableSocketConfig, PortableSocketDriver, PortableSocketError,
        UDP_MAXIMUM_PAYLOAD_BYTES, fixture_driver,
    };
    use axiusflow_transport::{
        ActiveIngestMode, DriverLifecycle, IngestDriver, IngestProfile, QueueBinding,
    };
    use std::{net::SocketAddr, num::NonZeroUsize, time::Duration};

    fn limit(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test limit is non-zero")
    }

    fn loopback_address() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    #[test]
    fn profile_is_portable_socket() {
        assert_eq!(PROFILE, IngestProfile::PortableSocket);
    }

    #[test]
    fn loopback_defaults_validate() {
        let config = PortableSocketConfig::loopback().expect("built-in bounds validate");
        assert!(config.maximum_frame_bytes.get() <= UDP_MAXIMUM_PAYLOAD_BYTES);
    }

    #[test]
    fn zero_poll_timeout_is_rejected() {
        let error = PortableSocketConfig::try_new(
            loopback_address(),
            Duration::ZERO,
            Duration::from_millis(1),
            limit(8),
            limit(2_048),
        )
        .expect_err("a zero poll timeout must be rejected");
        assert!(matches!(error, PortableSocketError::ZeroPollTimeout));
    }

    #[test]
    fn idle_interval_above_poll_timeout_is_rejected() {
        let error = PortableSocketConfig::try_new(
            loopback_address(),
            Duration::from_millis(10),
            Duration::from_millis(50),
            limit(8),
            limit(2_048),
        )
        .expect_err("an idle interval above the poll timeout must be rejected");
        assert!(matches!(
            error,
            PortableSocketError::InvalidIdlePollInterval
        ));
    }

    #[test]
    fn frame_limit_above_udp_payload_maximum_is_rejected() {
        let oversized = UDP_MAXIMUM_PAYLOAD_BYTES + 1;
        let error = PortableSocketConfig::try_new(
            loopback_address(),
            Duration::from_millis(100),
            Duration::from_millis(1),
            limit(8),
            limit(oversized),
        )
        .expect_err("a frame limit above the UDP payload maximum must be rejected");
        assert!(matches!(
            error,
            PortableSocketError::FrameLimitUnsupported(value) if value == oversized
        ));
    }

    #[test]
    fn receive_before_start_fails_on_lifecycle_rather_than_returning_frames() {
        let config = PortableSocketConfig::loopback().expect("built-in bounds validate");
        let mut driver =
            PortableSocketDriver::try_new(config).expect("the portable driver is authorized");
        driver
            .bind_queue(QueueBinding {
                queue_id: 0,
                maximum_batch_items: limit(2),
                maximum_frame_bytes: limit(2_048),
            })
            .expect("queue 0 binds");

        let error = driver
            .receive_batch()
            .expect_err("receiving before start must fail");
        assert!(
            matches!(
                error,
                PortableSocketError::NotStarted
                    | PortableSocketError::InvalidLifecycle {
                        expected: DriverLifecycle::Running,
                        ..
                    }
            ),
            "unexpected pre-start receive error: {error:?}"
        );
    }

    #[test]
    fn unbound_queue_above_capability_is_rejected() {
        let config = PortableSocketConfig::loopback().expect("built-in bounds validate");
        let mut driver =
            PortableSocketDriver::try_new(config).expect("the portable driver is authorized");
        let queues = driver.capabilities().receive_queues;
        let error = driver
            .bind_queue(QueueBinding {
                queue_id: queues,
                maximum_batch_items: limit(2),
                maximum_frame_bytes: limit(2_048),
            })
            .expect_err("a queue beyond reported capability must be rejected");
        assert!(matches!(
            error,
            PortableSocketError::QueueUnavailable(value) if value == queues
        ));
    }

    #[test]
    fn fixture_driver_is_never_a_native_socket_mode() {
        let driver = fixture_driver(Vec::new()).expect("the software fixture is authorized");
        assert_eq!(
            driver.capabilities().supported_modes,
            vec![ActiveIngestMode::SoftwareFixture],
            "the fixture must not present itself as a native portable socket"
        );
    }
}
