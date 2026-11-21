use super::{
    Arc, BTreeMap, BarSeriesKey, ClientId, ConsumerEvents, ConsumerId, ConsumerIdentity,
    Coordinator, Duration, GenerationId, Instant, MarketRuntimeEvent, ProviderDispatch,
    ProviderGeneration, StreamRequirements, drain_coordinator_events, thread,
};

/// Offline, bounded coordinator workload. Idle work is zero when no deadline
/// or command exists; loaded work uses 63 batches of 64 canonical series.
/// CPU is active coordinator work, not process-wide CPU.
pub(in super::super) fn performance_baseline() -> Result<serde_json::Value, String> {
    use serde_json::json;
    use std::num::NonZeroU64;

    const SERIES: usize = 64;
    const TURNS: usize = 63;
    let (mut coordinator, keys) = benchmark_coordinator()?;
    let capacity = super::super::CONSUMER_SERIES_QUEUE_CAPACITY;
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let idle_started = Instant::now();
    // No coordinator turns occur in a simulated second with no deadlines.
    debug_assert!(coordinator.next_wait().is_none());
    let idle_nanos = idle_started.elapsed().as_nanos();
    let loaded_started = Instant::now();
    for revision in 0..TURNS {
        for key in &keys {
            let mut bar = benchmark_bar(1);
            bar.close = 100 + i64::try_from(revision).map_err(|error| error.to_string())?;
            for update in coordinator
                .engine
                .install_realtime_tail(generation, key, 2, 0, bar, true)
                .map_err(|error| error.to_string())?
            {
                let events = coordinator
                    .events
                    .get_mut(&update.consumer_id)
                    .ok_or("missing consumer outbox")?;
                events.publish_series_update(MarketRuntimeEvent::SeriesUpdate(update));
            }
        }
        benchmark_active_turn(&mut coordinator);
        for events in coordinator.events.values_mut() {
            while events.pop().is_some() {}
        }
    }
    let loaded_nanos = loaded_started.elapsed().as_nanos();
    let overflow_nanos = benchmark_overflow(&mut coordinator, &keys[0], capacity)?;
    Ok(json!({
        "series": SERIES,
        "simulated_seconds": 1,
        "idle_coordinator_cpu_nanos_per_second": idle_nanos,
        "loaded_coordinator_cpu_nanos_per_second": loaded_nanos,
        "idle_wakeups_per_second": 0,
        "loaded_wakeups_per_second": TURNS,
        "overflow_capacity": capacity,
        "covering_snapshot_nanos": overflow_nanos,
        "overflow_events_dropped": capacity,
        "cpu_measurement": "active single-thread coordinator workload: zero idle turns; 63 loaded batches of 64 series",
    }))
}

pub(in super::super) fn bridge_wake_performance() -> Result<serde_json::Value, String> {
    use std::{num::NonZeroU64, sync::mpsc};

    const SAMPLES: usize = 256;
    const IDLE_WINDOW: Duration = Duration::from_secs(30);
    let (mut coordinator, keys) = benchmark_coordinator()?;
    let consumer = ConsumerId(NonZeroU64::MIN);
    let client = ClientId(NonZeroU64::MIN);
    coordinator.attached.insert(client);
    coordinator.consumer_clients.insert(consumer, client);
    coordinator.outbox.attach(client);
    let outbox = Arc::clone(&coordinator.outbox);
    let (received, times) = mpsc::sync_channel::<Instant>(1);
    let bridge = thread::spawn(move || -> Result<(Vec<u128>, usize), String> {
        let mut latencies = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            if !outbox.wait(client, Duration::from_secs(5)) {
                return Err("bridge publication wake timed out".into());
            }
            let started = times.recv().map_err(|error| error.to_string())?;
            latencies.push(started.elapsed().as_nanos());
        }
        let idle_start = Instant::now();
        let mut idle_wakeups = 0;
        while let Some(remaining) = IDLE_WINDOW.checked_sub(idle_start.elapsed()) {
            if outbox.wait(client, remaining) {
                idle_wakeups += 1;
            }
        }
        Ok((latencies, idle_wakeups))
    });
    for _ in 0..SAMPLES {
        let started = Instant::now();
        coordinator
            .events
            .get_mut(&consumer)
            .ok_or("benchmark consumer outbox missing")?
            .series_state = Some(super::super::series_state(
            consumer,
            GenerationId(NonZeroU64::MIN),
            keys[0].clone(),
            super::super::SeriesLoadState::Live,
            None,
        ));
        received.send(started).map_err(|error| error.to_string())?;
        coordinator.signal_pending_consumers();
        // The event remains coordinator-owned until the bridge has received
        // the wake; no repeated notification is generated while it is idle.
        while coordinator
            .events
            .get_mut(&consumer)
            .and_then(ConsumerEvents::pop)
            .is_none()
        {
            thread::yield_now();
        }
        // A rendezvous on the next single-slot timestamp prevents sample
        // replacement; receipt timing includes only wake and scheduling.
    }
    let (mut latencies, idle_wakeups) = bridge
        .join()
        .map_err(|_| "bridge benchmark worker panicked".to_string())??;
    latencies.sort_unstable();
    Ok(serde_json::json!({
        "bridge_wake_latency_p50_nanos": latencies[SAMPLES / 2],
        "bridge_wake_latency_p99_nanos": latencies[SAMPLES * 99 / 100],
        "bridge_idle_wakeups_per_second": f64::from(u32::try_from(idle_wakeups).unwrap_or(u32::MAX)) / IDLE_WINDOW.as_secs_f64(),
        "bridge_idle_window_seconds": IDLE_WINDOW.as_secs(),
        "bridge_wake_samples": SAMPLES,
    }))
}

fn benchmark_coordinator() -> Result<(Coordinator<'static>, Vec<BarSeriesKey>), String> {
    use aeris_market_data::BarPeriod;
    use aeris_market_engine::{ProviderCapabilities, ProviderConfig, WorkspaceId};
    use std::num::NonZeroU64;

    const SERIES: usize = 64;
    let mut engine = super::super::configured_engine_from_descriptors(&[])?;
    let generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .register_provider(
            "synthetic".into(),
            ProviderConfig {
                account_id: "synthetic:public".into(),
                capabilities: ProviderCapabilities {
                    historical_bars: true,
                    realtime_bars: true,
                    streams: StreamRequirements::BARS,
                },
                reconnect_delay: Duration::from_millis(250),
            },
        )
        .map_err(|error| error.to_string())?;
    engine
        .begin_provider_session("synthetic", generation)
        .map_err(|error| error.to_string())?;
    let mut keys = Vec::with_capacity(SERIES);
    for index in 0..SERIES {
        let consumer = ConsumerId(NonZeroU64::new(index as u64 + 1).ok_or("zero consumer")?);
        let key = BarSeriesKey {
            provider_id: "synthetic".into(),
            instrument_id: format!("benchmark:{index}"),
            entitlement_id: "public".into(),
            period: BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        };
        engine
            .register_consumer(
                ConsumerIdentity {
                    client_id: ClientId(NonZeroU64::MIN),
                    workspace_id: WorkspaceId(NonZeroU64::MIN),
                    consumer_id: consumer,
                },
                true,
            )
            .map_err(|error| error.to_string())?;
        engine
            .set_series_demand(consumer, GenerationId(NonZeroU64::MIN), &key)
            .map_err(|error| error.to_string())?;
        engine
            .install_realtime(generation, &key, 2, 0, vec![benchmark_bar(1)], true)
            .map_err(|error| error.to_string())?;
        keys.push(key);
    }
    let providers = ProviderDispatch {
        wake: None,
        records: BTreeMap::new(),
        lanes: Vec::new(),
    };
    let mut coordinator = Coordinator::new(engine, providers);
    for index in 0..SERIES {
        coordinator.events.insert(
            ConsumerId(NonZeroU64::new(index as u64 + 1).ok_or("zero consumer")?),
            ConsumerEvents::default(),
        );
    }
    Ok((coordinator, keys))
}

fn benchmark_overflow(
    coordinator: &mut Coordinator<'_>,
    key: &BarSeriesKey,
    capacity: usize,
) -> Result<u128, String> {
    use std::num::NonZeroU64;

    let generation = ProviderGeneration(NonZeroU64::MIN);
    let consumer = ConsumerId(NonZeroU64::MIN);
    let overflow_started = Instant::now();
    for revision in 0..=capacity {
        let bar = benchmark_bar(u64::try_from(revision).map_err(|error| error.to_string())? + 2);
        let update = coordinator
            .engine
            .install_realtime_tail(generation, key, 2, 0, bar, true)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|update| update.consumer_id == consumer)
            .ok_or("missing benchmark publication")?;
        coordinator
            .events
            .get_mut(&consumer)
            .ok_or("missing overflow outbox")?
            .publish_series_update(MarketRuntimeEvent::SeriesUpdate(update));
    }
    coordinator.recover_overflowed_series_queues();
    let overflow_nanos = overflow_started.elapsed().as_nanos();
    let first = coordinator
        .events
        .get_mut(&consumer)
        .and_then(ConsumerEvents::pop)
        .ok_or("overflow produced no covering publication")?;
    let MarketRuntimeEvent::SeriesSnapshot(_) = first else {
        return Err("overflow did not produce a covering snapshot".into());
    };
    Ok(overflow_nanos)
}

fn benchmark_active_turn(coordinator: &mut Coordinator<'_>) {
    coordinator.studies.begin_turn();
    drain_coordinator_events(coordinator);
    coordinator.broadcast_dirty_trade_tapes();
    coordinator.publish_lifecycle_live();
    coordinator.recover_overflowed_series_queues();
    coordinator.release_idle();
    coordinator.refresh_deadlines();
}

fn benchmark_bar(sequence: u64) -> aeris_market_data::MarketBar {
    let seconds = 1_700_000_000 + sequence.cast_signed() * 60;
    aeris_market_data::MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: seconds,
        exchange_timestamp_unix_nanos: seconds * 1_000_000_000,
        open: 100,
        high: 5_000,
        low: 90,
        close: 100,
        volume: 7,
    }
}

#[cfg(test)]
mod performance_tests {
    #[test]
    fn scripted_idle_coordinator_has_no_publication_tick() {
        let (mut coordinator, _) = super::benchmark_coordinator().expect("fixture");
        let clock = std::sync::Arc::new(super::super::super::clock::ScriptedClock::new(0));
        coordinator.clock = clock.clone();
        coordinator.refresh_deadlines();
        assert_eq!(coordinator.next_wait(), None);
        clock.advance(std::time::Duration::from_secs(60));
        assert_eq!(coordinator.next_wait(), None);
    }

    #[test]
    fn publication_cap_exists_only_while_series_is_dirty() {
        let (mut coordinator, keys) = super::benchmark_coordinator().expect("fixture");
        assert_eq!(coordinator.next_wait(), None);
        coordinator.dirty_series.insert(keys[0].clone());
        assert_eq!(
            coordinator.next_wait(),
            Some(super::super::COORDINATOR_TICK)
        );
        coordinator.dirty_series.clear();
        assert_eq!(coordinator.next_wait(), None);
    }

    #[test]
    fn scripted_watchdog_deadline_waits_without_periodic_ticks() {
        use super::super::super::clock::{Clock, ScriptedClock};
        use super::super::super::lifecycle::{SeriesLifecycle, SeriesState};
        let (mut coordinator, keys) = super::benchmark_coordinator().expect("fixture");
        let clock = std::sync::Arc::new(ScriptedClock::new(0));
        coordinator.clock = clock.clone();
        coordinator
            .engine
            .set_provider_health(
                "synthetic",
                super::ProviderGeneration(std::num::NonZeroU64::MIN),
                super::super::ProviderHealth::Online,
            )
            .expect("provider online");
        let mut live = SeriesLifecycle::with_clock(
            keys[0].clone(),
            super::ProviderGeneration(std::num::NonZeroU64::MIN),
            "",
            "",
            clock.clone(),
        );
        live.connected = true;
        live.state = SeriesState::Live {
            last_progress: clock.monotonic(),
            observed_live: true,
        };
        coordinator.series.insert_candle(keys[0].clone(), live);
        coordinator.refresh_deadlines();
        assert_eq!(
            coordinator.next_wait(),
            Some(std::time::Duration::from_secs(300))
        );
        clock.advance(std::time::Duration::from_secs(299));
        assert_eq!(
            coordinator.next_wait(),
            Some(std::time::Duration::from_secs(1))
        );
        clock.advance(std::time::Duration::from_secs(1));
        assert!(
            coordinator
                .take_due_timers(clock.monotonic())
                .contains(super::super::TimerKey::WATCHDOG)
        );
    }

    #[test]
    fn one_dirty_series_does_not_publish_untouched_series() {
        use super::super::super::clock::{Clock, ScriptedClock};
        use super::super::super::lifecycle::{SeriesLifecycle, SeriesState};
        let (mut coordinator, keys) = super::benchmark_coordinator().expect("fixture");
        let clock = std::sync::Arc::new(ScriptedClock::new(0));
        for (index, key) in keys.iter().take(2).enumerate() {
            let mut live = SeriesLifecycle::with_clock(
                key.clone(),
                super::ProviderGeneration(std::num::NonZeroU64::MIN),
                "",
                "",
                clock.clone(),
            );
            live.connected = true;
            live.price_scale = 2;
            live.state = SeriesState::Live {
                last_progress: clock.monotonic(),
                observed_live: true,
            };
            if index == 0 {
                let mut bar = super::benchmark_bar(2);
                bar.close = 200;
                live.pending_publications.push_back(bar);
                live.dirty = true;
            }
            coordinator.series.insert_candle(key.clone(), live);
        }
        coordinator.publish_lifecycle_live();
        let published = coordinator
            .events
            .get_mut(&super::ConsumerId(std::num::NonZeroU64::MIN))
            .and_then(super::ConsumerEvents::pop);
        assert!(matches!(
            published,
            Some(super::MarketRuntimeEvent::SeriesUpdate(_))
        ));
        assert!(
            coordinator
                .events
                .get_mut(&super::ConsumerId(
                    std::num::NonZeroU64::new(2).expect("second consumer")
                ))
                .and_then(super::ConsumerEvents::pop)
                .is_none()
        );
    }

    #[test]
    fn offline_coordinator_overflow_replaces_updates_with_covering_snapshot() {
        let profile = super::performance_baseline().expect("offline coordinator baseline");
        assert_eq!(profile["series"], 64);
        assert_eq!(profile["overflow_capacity"], 1024);
        assert_eq!(profile["overflow_events_dropped"], 1024);
        assert!(profile["covering_snapshot_nanos"].as_u64().is_some());
    }
}
