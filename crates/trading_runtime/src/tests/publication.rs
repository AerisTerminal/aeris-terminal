//! M5.11 (VAL-TRD-066/067): bounded working set, paged read-only history and change-driven,
//! coalesced, lag-safe publication.

use super::*;
use crate::store::create_schema_at;
use crate::{
    HistoryQuery, MAXIMUM_HISTORY_PAGE, MAXIMUM_SUBSCRIPTIONS, SUBSCRIPTION_QUEUE_CAPACITY,
    TradingEventCause, WORKING_SET_FILL_LIMIT,
};
use std::collections::BTreeSet;

const LOAD_TERMINAL_ORDERS: usize = 50_000;
const LOAD_OPEN_ORDERS: usize = 100;
const LOAD_FILLS: usize = 601;
/// The provenance every fixture row carries, in the store's encoded shape.
const FIXTURE_PROVENANCE: &str = "{\"venue_id\":\"aeris-sim\",\"provider_id\":\"fixture\",\
    \"session_generation\":1,\"source_sequence\":1,\"observed_unix_nanos\":1}";

/// Builds a file-backed store at the current schema with `terminal` terminal orders, `open`
/// working GTC orders (the last two Inactive held legs), `fills` alternating fills netting one
/// long contract, and the matching open position. Returns after the first service boot has
/// migrated/backfilled the account and registered the fixture instrument.
fn write_load_fixture(database_path: &std::path::Path, terminal: usize, open: usize, fills: usize) {
    let path = database_path.to_path_buf();
    std::fs::create_dir_all(path.parent().expect("store directory")).expect("fixture directory");
    {
        let connection = rusqlite::Connection::open(&path).expect("fixture database");
        create_schema_at(&connection, SCHEMA_VERSION);
        connection
            .execute_batch(
                "INSERT INTO accounts(id, display_name, environment, currency, currency_scale,
                    starting_equity_units, starting_equity_scale)
                 VALUES ('aeris-sim-1', 'SIM load', 'simulated', 'USD', 2, 10_000_000, 2);",
            )
            .expect("fixture account");
    }
    let configuration = TradingServiceConfig {
        database_path: path.clone(),
        retention: TradingRetention::default(),
        clock: std::sync::Arc::new(crate::clock::SystemClock::default()),
        execution: ExecutionPolicy::default(),
    };
    let service = start_service_with_config(configuration);
    service
        .register_instrument(instrument())
        .expect("fixture instrument");
    service
        .shutdown(Duration::from_secs(2))
        .expect("boot stops");
    let instrument_id = instrument().instrument_id.as_str().to_string();
    let connection = rusqlite::Connection::open(&path).expect("fixture database");
    let transaction = connection
        .unchecked_transaction()
        .expect("fixture transaction");
    write_load_rows(&transaction, &instrument_id, terminal, open, fills);
    transaction.commit().expect("fixture commit");
}

/// Writes the VAL-TRD-068 desktop fixture: a current-schema store with more than 10,000
/// terminal orders, one open position and one working limit (plus two inactive held legs).
#[test]
#[ignore = "writes the documented load fixture path for desktop validation"]
fn generate_gui_load_fixture_for_validation() {
    let path = std::env::var_os("AERIS_LOAD_FIXTURE_OUT")
        .map(std::path::PathBuf::from)
        .expect("set AERIS_LOAD_FIXTURE_OUT to the output trading.sqlite3 path");
    write_load_fixture(&path, 10_001, 3, LOAD_FILLS);
    println!("generated GUI load fixture at {}", path.display());
}

/// Writes the order, event, fill and position rows for the load fixture in one transaction.
fn write_load_rows(
    transaction: &rusqlite::Transaction<'_>,
    instrument_id: &str,
    terminal: usize,
    open: usize,
    fills: usize,
) {
    let mut sequence = 1_i64;
    {
        let (
            mut order_statement,
            mut event_statement,
            mut fill_statement,
            mut accounting_statement,
        ) = load_fixture_statements(transaction);
        for index in 0..(terminal + open) {
            let is_open = index >= terminal;
            let has_fill = index < fills;
            let status = if is_open && index >= terminal + open - 2 {
                "inactive"
            } else if is_open {
                "working"
            } else if has_fill {
                "filled"
            } else {
                "cancelled"
            };
            let submitted = i64::try_from(index).expect("index") * 10 + 10;
            let order_id = format!("load-order-{index}");
            order_statement
                .execute(rusqlite::params![
                    order_id,
                    format!("load-client-{index}"),
                    instrument_id,
                    if is_open { "gtc" } else { "day" },
                    status,
                    submitted,
                    i64::from(has_fill),
                    if is_open { None } else { Some(submitted + 1) },
                    FIXTURE_PROVENANCE,
                ])
                .expect("fixture order");
            event_statement
                .execute(rusqlite::params![
                    format!("load-event-{index}"),
                    order_id,
                    sequence,
                    submitted,
                    FIXTURE_PROVENANCE,
                ])
                .expect("fixture event");
            sequence += 1;
            if has_fill {
                let fill_id = format!("load-fill-{index}");
                fill_statement
                    .execute(rusqlite::params![
                        fill_id,
                        order_id,
                        instrument_id,
                        if index % 2 == 0 { "buy" } else { "sell" },
                        submitted + 5,
                        FIXTURE_PROVENANCE,
                    ])
                    .expect("fixture fill");
                accounting_statement
                    .execute(rusqlite::params![fill_id, instrument_id])
                    .expect("fixture accounting");
            }
        }
        // The fills alternate buy/sell from an even count start, netting one long contract at
        // 50.00 with zero realized P&L.
        assert_eq!(fills % 2, 1, "the fixture nets exactly one long contract");
        let last_fill_at = i64::try_from(fills - 1).expect("fill index") * 10 + 15;
        transaction
            .execute(
                "INSERT INTO positions(account_id, instrument_id, net_units, net_scale,
                    average_units, average_scale, realized_units, realized_scale,
                    unrealized_units, unrealized_scale, last_fill_unix_nanos)
                 VALUES ('aeris-sim-1', ?1, 1, 0, 5000, 2, 0, 2, 0, 2, ?2)",
                rusqlite::params![instrument_id, last_fill_at],
            )
            .expect("fixture position");
        transaction
            .execute(
                "UPDATE metadata SET value = ?1 WHERE key = 'next_sequence'",
                [sequence],
            )
            .expect("fixture sequence");
    }
}

/// Prepares the four insert statements the load fixture reuses across 50,100 rows.
fn load_fixture_statements<'a>(
    transaction: &'a rusqlite::Transaction<'a>,
) -> (
    rusqlite::Statement<'a>,
    rusqlite::Statement<'a>,
    rusqlite::Statement<'a>,
    rusqlite::Statement<'a>,
) {
    let order = transaction
        .prepare(
            "INSERT INTO orders(id, client_order_id, account_id, instrument_id, side,
                order_type, time_in_force, quantity_units, quantity_scale, limit_units,
                limit_scale, status, submitted_unix_nanos, provenance_json, filled_units,
                filled_scale, terminal_unix_nanos, accepted_unix_nanos, updated_unix_nanos,
                eligible_after_unix_nanos)
             VALUES (?1, ?2, 'aeris-sim-1', ?3, 'buy', 'limit', ?4, 1, 0, 5000, 2, ?5,
                ?6, ?9, ?7, 0, ?8, ?6, ?6, ?6)",
        )
        .expect("order statement");
    let event = transaction
        .prepare(
            "INSERT INTO order_events(id, order_id, sequence, kind, event_unix_nanos,
                detail, provenance_json)
             VALUES (?1, ?2, ?3, 'accepted', ?4, NULL, ?5)",
        )
        .expect("event statement");
    let fill = transaction
        .prepare(
            "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                price_scale, quantity_units, quantity_scale, execution_unix_nanos,
                provenance_json)
             VALUES (?1, ?2, 'aeris-sim-1', ?3, ?4, 5000, 2, 1, 0, ?5, ?6)",
        )
        .expect("fill statement");
    let accounting = transaction
        .prepare(
            "INSERT INTO fill_accounting(fill_id, account_id, instrument_id)
             VALUES (?1, 'aeris-sim-1', ?2)",
        )
        .expect("accounting statement");
    (order, event, fill, accounting)
}

fn load_service(directory: &TestDirectory) -> TradingService {
    let mut configuration = config(directory);
    configuration.retention = TradingRetention::default();
    start_service_with_config(configuration)
}

#[test]
fn working_set_and_snapshot_stay_bounded_at_50000_orders() {
    // VAL-TRD-066: the working set carries only open orders, active groups, positions and the
    // newest 500 fills; the compatibility snapshot succeeds far beyond 10,000 orders.
    let directory = TestDirectory::new("publication-load");
    write_load_fixture(
        &config(&directory).database_path,
        LOAD_TERMINAL_ORDERS,
        LOAD_OPEN_ORDERS,
        LOAD_FILLS,
    );
    let service = load_service(&directory);
    let started = std::time::Instant::now();
    let subscription = service.subscribe().expect("subscribe");
    let initial = subscription
        .recv_timeout(Duration::from_secs(10))
        .expect("initial working set");
    let build = started.elapsed();
    println!("working-set build at 50,100 stored orders: {build:?}");
    assert!(
        build < Duration::from_secs(5),
        "working-set build exceeds its bound: {build:?}"
    );
    assert_eq!(initial.event.cause, TradingEventCause::Snapshot);
    assert!(!initial.lagged);
    let working_set = &initial.event.working_set;
    assert_eq!(working_set.orders.len(), LOAD_OPEN_ORDERS);
    assert!(
        working_set
            .orders
            .iter()
            .all(|order| order.status.is_open())
    );
    assert_eq!(
        working_set
            .orders
            .iter()
            .filter(|order| order.status == OrderStatus::Inactive)
            .count(),
        2,
        "inactive held legs are part of the working set"
    );
    assert_eq!(working_set.fills.len(), WORKING_SET_FILL_LIMIT);
    assert_eq!(working_set.positions.len(), 1);
    assert_eq!(working_set.accounts.len(), 1);
    let snapshot = service
        .snapshot()
        .expect("snapshot succeeds beyond 10,000 orders");
    assert_eq!(
        snapshot.orders.len(),
        LOAD_TERMINAL_ORDERS + LOAD_OPEN_ORDERS
    );
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
}

#[test]
fn history_pages_return_every_order_exactly_once_in_order() {
    // VAL-TRD-066: paged history holds at most 500 rows per page, covers all 50,100 orders
    // exactly once in insertion order, and the reader connection rejects writes.
    let directory = TestDirectory::new("publication-history");
    write_load_fixture(
        &config(&directory).database_path,
        LOAD_TERMINAL_ORDERS,
        LOAD_OPEN_ORDERS,
        LOAD_FILLS,
    );
    let service = load_service(&directory);
    let total = LOAD_TERMINAL_ORDERS + LOAD_OPEN_ORDERS;
    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0_usize;
    loop {
        let page = service
            .history(HistoryQuery {
                account_id: None,
                after,
                limit: None,
            })
            .expect("history page");
        assert!(page.orders.len() <= MAXIMUM_HISTORY_PAGE);
        pages += 1;
        after = page.next;
        seen.extend(page.orders);
        if after.is_none() {
            break;
        }
    }
    assert_eq!(pages, total.div_ceil(MAXIMUM_HISTORY_PAGE));
    assert_eq!(seen.len(), total, "every order paged exactly once");
    let unique = seen
        .iter()
        .map(|order| order.client_order_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), total);
    assert!(
        seen.windows(2)
            .all(|pair| pair[0].submitted_unix_nanos < pair[1].submitted_unix_nanos),
        "pages follow insertion order"
    );
    let clamped = service
        .history(HistoryQuery {
            account_id: None,
            after: None,
            limit: Some(MAXIMUM_HISTORY_PAGE + 700),
        })
        .expect("clamped page");
    assert_eq!(clamped.orders.len(), MAXIMUM_HISTORY_PAGE);
    let connection =
        crate::store::open_read_only(&config(&directory).database_path).expect("read-only open");
    assert!(
        connection
            .execute(
                "INSERT INTO metadata(key, value) VALUES ('write-probe', 1)",
                []
            )
            .is_err(),
        "the reader connection is read-only"
    );
    service.shutdown(Duration::from_secs(5)).expect("shutdown");
}

#[test]
fn an_idle_runtime_publishes_nothing() {
    // VAL-TRD-067: no commands and no quote changes over minutes of timer ticks means zero
    // publications after the initial snapshot.
    let directory = TestDirectory::new("publication-idle");
    let (service, clock, _feed) = live_feed_service(&directory);
    let subscription = service.subscribe().expect("subscribe");
    let initial = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("initial working set");
    assert_eq!(initial.event.cause, TradingEventCause::Snapshot);
    for _ in 0..4 {
        clock.advance(Duration::from_secs(61));
        service.status().expect("timer tick");
        service.status().expect("timer barrier");
    }
    assert!(
        subscription
            .recv_timeout(Duration::from_millis(200))
            .is_none(),
        "an idle runtime publishes nothing"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn mark_publications_coalesce_and_carry_the_latest_unrealized() {
    // VAL-TRD-067: 1,000 mark-only quotes inside one coalescing window yield an immediate plus
    // one trailing publication, and the last carries the exact latest unrealized P&L.
    let directory = TestDirectory::new("publication-marks");
    let (service, clock, feed) = live_feed_service(&directory);
    let instrument = instrument();
    service
        .register_instrument(instrument.clone())
        .expect("instrument");
    bootstrap_live_feed(&feed, &instrument, market_feed::route());
    service.status().expect("feed drains");
    service
        .place_order(market_order("mark-entry", OrderSide::Buy, 1, 1_000))
        .expect("entry");
    push_live_quote(&feed, &instrument, 2, 9_975, 10_000, 2);
    service.status().expect("entry fills");
    let subscription = service.subscribe().expect("subscribe");
    let initial = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("initial working set");
    assert_eq!(initial.event.cause, TradingEventCause::Snapshot);
    for index in 0..1_000_u64 {
        // Each mark differs, so every quote changes the position's unrealized P&L in memory.
        push_live_quote(
            &feed,
            &instrument,
            3 + index,
            10_001 + i64::try_from(index).expect("mark"),
            11_000,
            2,
        );
        service.status().expect("quote drains");
    }
    let first = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("the first mark publishes immediately");
    assert_eq!(first.event.cause, TradingEventCause::Mark);
    clock.advance(Duration::from_millis(150));
    service.status().expect("flush tick");
    let last = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("the trailing flush publishes the latest marks");
    assert_eq!(last.event.cause, TradingEventCause::Mark);
    assert!(
        subscription
            .recv_timeout(Duration::from_millis(200))
            .is_none(),
        "1,000 coalesced marks stay under ten publications"
    );
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(
        last.event.working_set.positions[0].unrealized_pnl, snapshot.positions[0].unrealized_pnl,
        "the last publication carries the exact latest unrealized P&L"
    );
    assert_ne!(
        first.event.working_set.positions[0].unrealized_pnl,
        last.event.working_set.positions[0].unrealized_pnl,
        "the burst moved the mark"
    );
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn every_revision_changing_step_is_published() {
    // VAL-TRD-067: each revision-changing command publishes one typed event whose working set
    // reflects the change.
    let directory = TestDirectory::new("publication-steps");
    let (service, _clock, feed) = live_feed_service(&directory);
    let instrument = instrument();
    service
        .register_instrument(instrument.clone())
        .expect("instrument");
    bootstrap_live_feed(&feed, &instrument, market_feed::route());
    push_live_quote(&feed, &instrument, 1, 9_975, 10_000, 2);
    service.status().expect("feed drains");
    let subscription = service.subscribe().expect("subscribe");
    let initial = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("initial working set");
    let mut revision = initial.event.revision;
    let mut resting = market_order("step-order", OrderSide::Buy, 2, 2_000);
    resting.order_type = OrderType::Limit;
    resting.time_in_force = TimeInForce::GoodTillCancelled;
    resting.limit_price = Some(FixedPoint::try_new(9_800, 2).expect("limit"));
    service.place_order(resting).expect("resting order");
    let placed = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("placement publishes");
    assert_eq!(placed.event.cause, TradingEventCause::Command);
    assert!(placed.event.revision > revision);
    revision = placed.event.revision;
    assert_eq!(placed.event.working_set.orders.len(), 1);
    service
        .cancel_order(ClientOrderId::try_new("step-order").expect("client id"))
        .expect("cancel");
    let cancelled = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("cancellation publishes");
    assert_eq!(cancelled.event.cause, TradingEventCause::Command);
    assert!(cancelled.event.revision > revision);
    assert_eq!(cancelled.event.working_set.orders.len(), 0);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn a_lagging_subscriber_resynchronizes_with_an_exact_working_set() {
    // VAL-TRD-067: a subscriber that never drains holds at most 1,024 events; the oldest are
    // dropped and the next receive reports lagged with the exact current working set.
    let directory = TestDirectory::new("publication-lag");
    let (service, clock, _feed) = live_feed_service(&directory);
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let subscription = service.subscribe().expect("subscribe");
    let wall = clock.wall_unix_nanos();
    let steps = SUBSCRIPTION_QUEUE_CAPACITY + 76;
    for index in 0..steps {
        let mut profile = risk_profile_for(&account, wall);
        profile.version = u32::try_from(index + 1).expect("version");
        profile.daily_loss_limit =
            FixedPoint::try_new(1_000 + i64::try_from(index).expect("index"), 2).expect("limit");
        service
            .register_risk_profile(profile)
            .expect("profile step");
    }
    let resync = subscription
        .recv_timeout(Duration::from_secs(10))
        .expect("lagged resync");
    assert!(resync.lagged, "the overflow reports lagged");
    assert_eq!(resync.event.cause, TradingEventCause::Resync);
    let snapshot = service.snapshot().expect("snapshot");
    assert_eq!(
        resync.event.revision, snapshot.revision,
        "the resync working set is exact at the current revision"
    );
    assert_eq!(resync.event.working_set.accounts, snapshot.accounts);
    let mut profile = risk_profile_for(&account, wall);
    profile.version = u32::try_from(steps + 1).expect("version");
    profile.daily_loss_limit = FixedPoint::try_new(9_999, 2).expect("limit");
    service
        .register_risk_profile(profile)
        .expect("post-resync step");
    let next = subscription
        .recv_timeout(Duration::from_secs(5))
        .expect("publication resumes after resync");
    assert!(!next.lagged);
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}

#[test]
fn a_ninth_subscription_is_refused() {
    // VAL-TRD-067: at most eight subscribers; the ninth is refused and a dropped
    // subscription frees its slot.
    let directory = TestDirectory::new("publication-limit");
    let (service, _clock, _feed) = live_feed_service(&directory);
    let subscriptions = (0..MAXIMUM_SUBSCRIPTIONS)
        .map(|_| service.subscribe().expect("subscription"))
        .collect::<Vec<_>>();
    let refused = service
        .subscribe()
        .expect_err("the ninth subscription is refused");
    assert!(
        refused.contains("subscription limit"),
        "typed refusal: {refused}"
    );
    drop(subscriptions);
    service.status().expect("unsubscribe drains");
    service.subscribe().expect("a freed slot subscribes again");
    service.shutdown(Duration::from_secs(2)).expect("shutdown");
}
