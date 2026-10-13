// Coordinator command/event characterization. Scripted setup and loop turns
// live in the boundary driver; assertions observe only outbound publications.

use super::*;
use super::boundary::Boundary;

#[test]
fn characterization_reseed_during_live_flow_keeps_one_forming_bar_and_contiguous_history() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series(), 1);
    let initial = boundary.history_request();
    boundary.complete(initial, minute_bars(0, 3), Some(minute_bars(3, 1)[0]));
    boundary.events();

    boundary.trade(1, 3);
    boundary.provider_event(RithmicRealtimeEvent::Connecting(2));
    boundary.provider_event(RithmicRealtimeEvent::Connected(2));
    boundary.trade(2, 4);
    let reseed = boundary.history_request();
    boundary.complete(reseed, minute_bars(0, 4), Some(minute_bars(4, 1)[0]));
    boundary.trade(3, 5);

    let events = boundary.events();
    let snapshots: Vec<_> = events.iter().filter_map(|event| match event {
        MarketRuntimeEvent::SeriesSnapshot(publication) => Some(&publication.snapshot),
        _ => None,
    }).collect();
    let latest = snapshots.last().expect("a covering snapshot after reseed");
    let last_update = events.iter().rev().find_map(|event| match event {
        MarketRuntimeEvent::SeriesUpdate(update) => Some(update),
        _ => None,
    }).expect("live flow publishes after reseed");
    assert!(last_update.forming, "the latest published tail has one forming bar");
    assert!(
        last_update.bar.exchange_timestamp_unix_nanos
            > latest.bars.last().expect("completed history").exchange_timestamp_unix_nanos,
        "the forming bar follows the completed history"
    );
    assert_eq!(
        latest
            .bars
            .windows(2)
            .map(|bars| bars[1].exchange_timestamp_unix_nanos - bars[0].exchange_timestamp_unix_nanos)
            .collect::<Vec<_>>(),
        vec![60_000_000_000; latest.bars.len() - 1]
    );
}

#[test]
fn characterization_outbox_overflow_emits_covering_snapshot() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series(), 1);
    let initial = boundary.history_request();
    boundary.complete(initial, minute_bars(0, 3), Some(minute_bars(3, 1)[0]));
    boundary.events();
    for minute in 4..(4
        + i64::try_from(super::super::super::CONSUMER_SERIES_QUEUE_CAPACITY)
            .expect("bounded consumer queue")
        + 2)
    {
        boundary.trade(u64::try_from(minute).expect("positive fixture minute"), minute);
    }
    boundary.tick();
    assert!(boundary.events().iter().any(|event| match event {
        MarketRuntimeEvent::SeriesSnapshot(publication) => publication.snapshot.bars.len() > 1_024,
        _ => false,
    }));
}

#[test]
fn characterization_stale_generation_history_completion_is_ignored() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series(), 1);
    let request = boundary.history_request();
    boundary.provider_event(RithmicRealtimeEvent::Connecting(2));
    boundary.events();
    boundary.complete(request, minute_bars(0, 3), None);
    assert!(
        !boundary.events().iter().any(|event| {
            matches!(event, MarketRuntimeEvent::SeriesSnapshot(_))
        }),
        "the retired history response cannot publish bars"
    );
}

#[test]
fn characterization_symbol_or_timeframe_change_keeps_provider_session() {
    let mut boundary = Boundary::new();
    boundary.demand(boundary.series(), 1);
    let initial = boundary
        .status()
        .providers
        .into_iter()
        .find(|state| state.provider == "rithmic")
        .expect("provider status");
    let mut other = boundary.series();
    other.period = BarPeriod::time(300).expect("five minute interval");
    boundary.demand(other, 2);
    let mut symbol = instrument();
    symbol.instrument_id = "instrument:rithmic:CME:MESU6".into();
    symbol.provider_symbol = "MESU6".into();
    symbol.display_symbol = "MES Sep 2026".into();
    symbol.entitlement_id = "rithmic-test:CME:MESU6".into();
    boundary.install(symbol.clone());
    let mut second_series = boundary.series();
    second_series.instrument_id = symbol.instrument_id;
    second_series.entitlement_id = symbol.entitlement_id;
    boundary.demand(second_series, 3);
    let after = boundary
        .status()
        .providers
        .into_iter()
        .find(|state| state.provider == "rithmic")
        .expect("provider status");
    assert_eq!(initial.generation, after.generation);
}
