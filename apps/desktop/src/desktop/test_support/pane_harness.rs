//! GPUI pane fixture exercising the real bounded market mailbox without a provider session.

use super::*;
use aeris_contracts::{ProviderInstrumentKind, ProviderInstrumentSummary};
use aeris_desktop::market_worker::{
    EngineWorkerStartup, LocalCommandOutcome, LocalSessionStatus, MarketProjectionSender,
    MarketWorkerCommand, market_projection_channel,
};
use aeris_desktop::test_support::{FixtureMarketEvents, FixtureMarketWorker, FixtureSeries};
use gpui::{TestAppContext, VisualTestContext};
use std::num::NonZeroUsize;
use std::sync::{atomic::AtomicU64, mpsc};

struct PaneHarness {
    pane: Entity<WorkspaceSurface>,
    window: Entity<PaneWindow>,
    market: FixtureMarketWorker,
    market_events: FixtureMarketEvents,
    publications: MarketProjectionSender,
    commands: Option<mpsc::Receiver<MarketWorkerCommand>>,
}

struct PaneWindow {
    pane: Entity<WorkspaceSurface>,
    toaster: Toaster,
    _trading_store: Entity<trading_store::TradingStore>,
}

impl toaster::ToastHost for PaneWindow {
    fn toaster_mut(&mut self) -> &mut Toaster {
        &mut self.toaster
    }

    fn activate_notice_action(
        &mut self,
        _: notification_center::NoticeAction,
        _: &mut Context<Self>,
    ) {
        panic!("pane notices have no action controls");
    }
}

impl Render for PaneWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .children(self.pane.read(cx).chart.clone())
            .children(toaster::toast_stack(&toaster::ToastLayer {
                host: &cx.entity(),
                toaster: &self.toaster,
                stack: toaster::ToastStack::of(&self.pane),
                price_axis_width: 64.0,
                theme: &AerisTheme::dark(),
                now: Instant::now(),
            }))
    }
}

impl PaneHarness {
    fn adopt_owner_publication(
        &self,
        subscription: &aeris_trading_runtime::TradingSubscription,
        cx: &mut VisualTestContext,
    ) {
        let update = subscription
            .recv_timeout(Duration::from_secs(2))
            .expect("owner publication");
        self.pane.update(cx, |pane, cx| {
            let store = pane
                .lifecycle
                .trading_store()
                .expect("injected empty store");
            store.update(cx, |store, cx| {
                store.apply_test_publication(update);
                cx.notify();
            });
            workspace_layout::apply_trading_snapshot(pane, &store, cx);
        });
    }

    fn fixture_snapshot(
        &mut self,
        product: &InstallProviderInstrument,
        interval: ChartInterval,
        generation: u64,
        bar_count: usize,
    ) -> PaneProjectionEvent {
        let series = engine_market_worker::series_key(product, interval).expect("fixture series");
        let route = FixtureSeries {
            series,
            provider_generation: product.session_generation,
            price_scale: u8::try_from(product.price_scale).expect("price scale"),
            quantity_scale: u8::try_from(product.quantity_scale).expect("quantity scale"),
        };
        let event = self
            .market
            .publish_runtime_snapshot(bar_count, &route, generation, &self.market_events)
            .expect("canonical fixture snapshot");
        PaneProjectionEvent::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection {
            event,
            received_unix_nanos: Some(301_000_000_000),
            recovery_request_id: None,
            selection_generation: None,
        })
    }

    fn fixture_readiness(
        &self,
        generation: u64,
        state: aeris_contracts::SeriesLoadState,
        detail: &str,
    ) -> PaneProjectionEvent {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(
                self.market_events.readiness(generation, state, detail),
            ),
        )
    }

    fn fixture_recovering(&self, generation: u64, detail: &str) -> PaneProjectionEvent {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(
                self.market_events.demand_error(
                    generation,
                    aeris_market_runtime::MarketDemandErrorScope::Series,
                    detail,
                ),
            ),
        )
    }

    fn open(cx: &mut TestAppContext) -> (Self, &mut VisualTestContext) {
        Self::open_with_restore(WorkspaceSurfaceRestore::default(), cx)
    }

    fn open_with_restore(
        restore: WorkspaceSurfaceRestore,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        Self::open_with_restore_and_full_queue(restore, false, cx)
    }

    fn open_with_restore_and_full_queue(
        restore: WorkspaceSurfaceRestore,
        fill_command_queue: bool,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        Self::open_with_startup(restore, fill_command_queue, false, cx)
    }

    fn open_with_startup(
        restore: WorkspaceSurfaceRestore,
        fill_command_queue: bool,
        fixture_futures_startup: bool,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        let product = if fixture_futures_startup {
            aeris_desktop::test_support::fixture_futures_contract("MNQ")
        } else {
            local_state::fixture_workspace().watchlist_entries[0]
                .instrument
                .clone()
                .expect("fixture product")
        };
        Self::open_with_product(
            restore,
            fill_command_queue,
            product,
            ChartInterval::Minute1,
            cx,
        )
    }

    fn open_with_product(
        restore: WorkspaceSurfaceRestore,
        fill_command_queue: bool,
        product: InstallProviderInstrument,
        interval: ChartInterval,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        Self::open_with_product_and_owner(restore, fill_command_queue, product, interval, None, cx)
    }

    fn open_with_product_and_owner(
        restore: WorkspaceSurfaceRestore,
        fill_command_queue: bool,
        product: InstallProviderInstrument,
        interval: ChartInterval,
        owner: Option<aeris_trading_runtime::TradingService>,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        let (commands, requests) = mpsc::sync_channel(8);
        let (publications, messages) = market_projection_channel(NonZeroUsize::new(8).unwrap());
        let (_shutdown_sender, shutdown) = mpsc::sync_channel(1);
        let worker = MarketDataWorker::from_channels(
            commands,
            messages,
            shutdown,
            None,
            Some(Arc::new(AtomicU64::new(1))),
        );
        if fill_command_queue {
            for _ in 0..8 {
                worker
                    .try_set_order_book_detail(OrderBookDetail::Depth)
                    .expect("fill bounded command queue before restore");
            }
        }
        let (view, cx) = cx.add_window_view(move |window, cx| {
            gpui_base::init(cx);
            let lifecycle = owner.map_or_else(DesktopLifecycle::new_for_test, |owner| {
                DesktopLifecycle::new_for_test_with_trading(owner)
            });
            let store = cx.new(|_| trading_store::TradingStore::default());
            lifecycle.register_trading_store(&store);
            let startup = MarketWorkerStartup::Loading(Box::new(EngineWorkerStartup {
                product,
                interval,
                restored_viewport: None,
                subscription_id: "desktop_fixture_market_bars".into(),
                worker_label: "disconnected fixture".into(),
            }));
            let pane = workspace_surface_entity(
                startup,
                worker,
                &lifecycle,
                chart_chrome::ChartChromePreferences::default(),
                restore,
                window,
                cx,
            );
            cx.observe(&pane, |_, _, cx| cx.notify()).detach();
            cx.subscribe(
                &pane,
                |window: &mut PaneWindow, pane, event: &PaneNoticeEvent, cx| {
                    let now = Instant::now();
                    match event {
                        PaneNoticeEvent::Post(notice) => window.toaster.post_notice(
                            toaster::ToastStack::of(&pane),
                            notice.clone(),
                            now,
                        ),
                        PaneNoticeEvent::Clear(key) => window.toaster.clear_notice(key, now),
                    }
                    cx.notify();
                },
            )
            .detach();
            pane.update(cx, |pane, cx| {
                if let Some(descriptor) = pane.descriptor.as_mut()
                    && descriptor.asset_class == aeris_instruments::InstrumentClass::Other
                {
                    descriptor.asset_class = aeris_instruments::InstrumentClass::Future;
                    descriptor.currency = Some("USDC".into());
                }
                pane.emit_initial_price_alert_restore_notice(cx);
                pane.emit_initial_chart_failure_notice(cx);
                pane.apply_initial_chart_settings(cx);
            });
            PaneWindow {
                pane,
                toaster: Toaster::default(),
                _trading_store: store,
            }
        });
        let pane = cx.read(|cx| view.read(cx).pane.clone());
        (
            Self {
                pane,
                window: view,
                market: FixtureMarketWorker::try_new().expect("fixture market"),
                market_events: FixtureMarketEvents::default(),
                publications,
                commands: Some(requests),
            },
            cx,
        )
    }

    fn publish_snapshot(&mut self, bar_count: usize, cx: &mut VisualTestContext) {
        let product = cx.read(|cx| self.pane.read(cx).product.clone().expect("product"));
        let snapshot = self.fixture_snapshot(&product, ChartInterval::Minute1, 1, bar_count);
        self.publications
            .send(snapshot)
            .expect("bounded fixture mailbox accepts publication");
        self.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        self.publications
            .send(PaneProjectionEvent::RuntimeProjection(
                aeris_desktop::market_worker::RuntimeProjection::event(
                    self.market_events.readiness(
                        1,
                        aeris_contracts::SeriesLoadState::Live,
                        "disconnected fixture ready",
                    ),
                ),
            ))
            .expect("fixture readiness enters mailbox");
        self.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }

    fn reinstall_snapshot(&mut self, bar_count: usize, cx: &mut VisualTestContext) {
        let snapshot = self
            .market
            .publish_snapshot(bar_count)
            .expect("fixture snapshot");
        self.pane.update(cx, |pane, cx| {
            pane.install_snapshot_chart(&snapshot.snapshot, cx);
        });
    }
}

fn place_scope_resting_order(
    owner: &aeris_trading_runtime::TradingService,
    product: &InstallProviderInstrument,
    account: &aeris_trading::TradingAccountId,
    id: &str,
) -> aeris_trading::Order {
    let mut command = aeris_desktop::trading::prepare_product_order(
        1_000_000_000_000,
        product,
        aeris_trading::OrderSide::Buy,
        Some(account.as_str().into()),
        1,
        aeris_trading::OrderType::Limit,
        aeris_trading::TimeInForce::Day,
    )
    .expect("valid product-scoped instruction");
    command.client_order_id = aeris_trading::ClientOrderId::try_new(id).expect("client id");
    command.limit_price = Some(aeris_trading::FixedPoint::try_new(9_900, 2).expect("limit"));
    let order = owner
        .place_order(command, false)
        .expect("open-session owner admission");
    assert_eq!(order.status, aeris_trading::OrderStatus::Working);
    order
}

fn assert_owner_rejects_unpriced_modify(
    owner: &aeris_trading_runtime::TradingService,
    order: &aeris_trading::Order,
) {
    use aeris_trading_runtime::{ModifyOrder, OrderCommandError, RevisionedModifyOrder};
    let error = owner
        .modify_order_checked(RevisionedModifyOrder {
            order: ModifyOrder {
                client_order_id: order.client_order_id.clone(),
                time_in_force: order.time_in_force,
                limit_price: Some(aeris_trading::FixedPoint::try_new(9_800, 2).expect("new limit")),
                stop_price: None,
                modified_unix_nanos: 1_000_000_000_000,
                provenance: order.provenance.clone(),
            },
            expected_revision: Some(order.revision),
        })
        .expect_err("owner rejects missing quote for modify");
    assert!(matches!(error, OrderCommandError::Other(message) if message.contains("quote")));
}

#[gpui::test]
fn pane_harness_chart_buy_intent_fills_through_owner_and_projects_position(
    cx: &mut TestAppContext,
) {
    let (owner, root, account, _, instrument, _) =
        trading_store::real_owner_fill_fixture_for("hyperliquid");
    let subscription = owner.subscribe().expect("owner subscription");
    let mut product = aeris_desktop::test_support::fixture_public_crypto();
    product.instrument_id = instrument.as_str().into();
    product.price_scale = 2;
    product.quantity_scale = 0;
    product.price_increment = Some(1);
    let (mut harness, cx) = PaneHarness::open_with_product_and_owner(
        WorkspaceSurfaceRestore::default(),
        false,
        product,
        ChartInterval::Minute1,
        Some(owner.clone()),
        cx,
    );
    harness.publish_snapshot(120, cx);
    harness.adopt_owner_publication(&subscription, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(
            pane.trading_pnl.order_entry.selected_account_id.as_ref(),
            Some(&account),
            "the pane selects the scripted practice account"
        );
        let chart = pane.chart.as_ref().expect("chart").read(cx);
        let (width, height) = chart.rendered_viewport_size();
        assert!(
            width >= 500.0 && height >= 350.0,
            "chart needs a realistic viewport: {width}x{height}"
        );
        assert!(pane.chart_chrome.chart_trade_buttons.shown());
        assert!(chart.legend_options().symbol, "symbol legend enabled");
        assert!(!chart.legend_title().is_empty(), "symbol legend has title");
    });
    // The chart prepaint first computes the pane geometry used by the next
    // legend layout, then the host Buy control can receive a real GPUI click.
    cx.update(|window, _| window.refresh());
    let buy = cx
        .debug_bounds("chart_trade_buy")
        .expect("host Buy control is painted");
    cx.simulate_click(buy.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let update = subscription
        .recv_timeout(Duration::from_secs(2))
        .expect("owner fill publication after chart Buy click");
    assert_eq!(update.event.working_set.fills.len(), 1);
    assert_eq!(update.event.working_set.positions.len(), 1);
    assert_eq!(update.event.working_set.fills[0].account_id, account);
    assert_eq!(update.event.working_set.fills[0].instrument_id, instrument);
    harness.pane.update(cx, |pane, cx| {
        let store = pane.lifecycle.trading_store().expect("pane TradingStore");
        store.update(cx, |store, cx| {
            store.apply_test_publication(update);
            cx.notify();
        });
        workspace_layout::apply_trading_snapshot(pane, &store, cx);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let store = pane.lifecycle.trading_store().expect("pane TradingStore");
        let store = store.read(cx);
        let view = store.pane_view(&account, &instrument);
        assert_eq!(
            store
                .current()
                .expect("owner projection")
                .fills
                .iter()
                .filter(|fill| fill.account_id == account && fill.instrument_id == instrument)
                .count(),
            1,
            "owner fill in the pane's TradingStore"
        );
        assert_eq!(
            view.positions().count(),
            1,
            "owner position in pane projection"
        );
        assert_eq!(
            pane.chart
                .as_ref()
                .expect("chart")
                .read(cx)
                .trading_snapshot()
                .positions
                .len(),
            1,
            "the projected position is painted on the chart"
        );
    });
    owner
        .shutdown(Duration::from_secs(2))
        .expect("owner stopped");
    std::fs::remove_dir_all(root).expect("remove test root");
}

fn place_owner_protective_target(
    owner: &aeris_trading_runtime::TradingService,
    product: &InstallProviderInstrument,
    account: &aeris_trading::TradingAccountId,
) -> aeris_trading::Order {
    use aeris_trading::{
        ClientOrderId, FixedPoint, OrderSide, OrderStatus, OrderType, TimeInForce,
    };
    use aeris_trading_runtime::{PlaceProtection, ProtectionRole};
    let price = |units| FixedPoint::try_new(units, 2).expect("price");
    let mut entry = aeris_desktop::trading::prepare_product_order(
        1_000_000_000_000,
        product,
        OrderSide::Buy,
        Some(account.as_str().into()),
        1,
        OrderType::Market,
        TimeInForce::Day,
    )
    .expect("entry");
    entry.client_order_id = ClientOrderId::try_new("pane-protected-entry").expect("id");
    let filled_entry = owner.place_order(entry, false).expect("owner entry fill");
    assert_eq!(filled_entry.status, OrderStatus::Filled);
    let mut target = aeris_desktop::trading::prepare_product_order(
        1_000_000_000_000,
        product,
        OrderSide::Sell,
        Some(account.as_str().into()),
        1,
        OrderType::Limit,
        TimeInForce::Day,
    )
    .expect("protection");
    target.client_order_id = ClientOrderId::try_new("pane-protective-target").expect("id");
    target.limit_price = Some(price(17_700));
    let protected = owner
        .place_protective_order(PlaceProtection {
            order: target,
            role: ProtectionRole::TakeProfit,
        })
        .expect("owner protection");
    assert_eq!(protected.status, OrderStatus::Working);
    protected
}

fn fill_owner_protective_target(
    owner: &aeris_trading_runtime::TradingService,
    subscription: &aeris_trading_runtime::TradingSubscription,
    clock: &aeris_trading_runtime::clock::ManualClock,
    instrument: &aeris_instruments::InstrumentId,
    protected: &aeris_trading::Order,
) -> aeris_trading_runtime::SubscriptionUpdate {
    use aeris_trading::market::{
        MarketFeedError, MarketFeedEvent, QuoteInterest, QuoteInterestSink, TradingQuote,
    };
    use aeris_trading::{FixedPoint, OrderStatus};
    use aeris_trading_runtime::{ModifyOrder, OrderCommandError, RevisionedModifyOrder};
    struct Interest;
    impl QuoteInterestSink for Interest {
        fn set_interest(&self, _: QuoteInterest) -> Result<(), MarketFeedError> {
            Ok(())
        }
    }
    let price = |units| FixedPoint::try_new(units, 2).expect("price");
    let feed = owner
        .attach_market_feed(Arc::new(Interest))
        .expect("scripted feed");
    clock.advance(Duration::from_millis(1));
    feed.push(MarketFeedEvent::Quote(TradingQuote {
        instrument_id: instrument.clone(),
        bid: price(17_725),
        ask: price(17_750),
        bid_size: None,
        ask_size: None,
        last: None,
        provider_id: "hyperliquid".into(),
        provider_generation: 1,
        source_sequence: Some(2),
        exchange_unix_nanos: None,
        received_unix_nanos: 1_000_001_000_000,
    }))
    .expect("trigger target");
    owner.status().expect("owner adopted trigger quote");
    let filled = (0..4)
        .map(|_| {
            subscription
                .recv_timeout(Duration::from_secs(2))
                .expect("owner publication held outside pane")
        })
        .find(|publication| {
            publication
                .event
                .working_set
                .fills
                .iter()
                .any(|fill| fill.order_id == protected.id)
        })
        .expect("owner filled protective target");
    assert!(
        filled
            .event
            .working_set
            .fills
            .iter()
            .any(|fill| fill.order_id == protected.id)
    );
    let stale = owner
        .modify_order_checked(RevisionedModifyOrder {
            order: ModifyOrder {
                client_order_id: protected.client_order_id.clone(),
                time_in_force: protected.time_in_force,
                limit_price: Some(price(17_800)),
                stop_price: None,
                modified_unix_nanos: 1_000_000_000_001,
                provenance: protected.provenance.clone(),
            },
            expected_revision: Some(protected.revision),
        })
        .expect_err("owner rejects modification of the filled protection");
    assert!(matches!(stale, OrderCommandError::StaleRevision { current }
        if current.id == protected.id && current.status == OrderStatus::Filled));
    filled
}

fn drag_pane_protection(cx: &mut VisualTestContext, hit: gpui::Point<gpui::Pixels>) {
    let moved = gpui::point(hit.x, hit.y - gpui::px(18.0));
    cx.simulate_mouse_move(hit, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(hit, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(
        moved,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::none(),
    );
    cx.simulate_mouse_up(moved, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
}

fn assert_stale_protection_notice(harness: &PaneHarness, cx: &VisualTestContext) {
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let chart = pane.chart.as_ref().expect("chart").read(cx);
        assert_eq!(chart.trading_snapshot().orders, [], "filled line removed");
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1, "one keyed stale revision notice");
        assert_eq!(notices[0].key.kind, "stale_order_edit");
        assert_eq!(notices[0].title, aeris_desktop::trading::STALE_ORDER_NOTICE);
        assert!(!notices[0].detail.contains("StaleRevision"));
    });
}

#[gpui::test]
fn pane_harness_protective_drag_owner_stale_revision_posts_one_notice_and_restores_line(
    cx: &mut TestAppContext,
) {
    let (owner, root, account, _, instrument, _, clock) =
        trading_store::real_owner_fill_fixture_for_with_clock("hyperliquid");
    let subscription = owner.subscribe().expect("owner subscription");
    let mut product = aeris_desktop::test_support::fixture_public_crypto();
    product.instrument_id = instrument.as_str().into();
    product.price_scale = 2;
    product.quantity_scale = 0;
    product.price_increment = Some(1);
    let (mut harness, cx) = PaneHarness::open_with_product_and_owner(
        WorkspaceSurfaceRestore::default(),
        false,
        product.clone(),
        ChartInterval::Minute1,
        Some(owner.clone()),
        cx,
    );
    harness.publish_snapshot(120, cx);
    harness.adopt_owner_publication(&subscription, cx);
    let protected = place_owner_protective_target(&owner, &product, &account);
    harness.adopt_owner_publication(&subscription, cx);
    harness.adopt_owner_publication(&subscription, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let line = &pane
            .chart
            .as_ref()
            .expect("chart")
            .read(cx)
            .trading_snapshot()
            .orders[0];
        assert_eq!(line.role, ChartOrderRole::TakeProfit);
        assert_eq!(line.id.as_str(), protected.client_order_id.as_str());
        assert_eq!(line.price, 177.0);
    });
    let chart = cx.read(|cx| harness.pane.read(cx).chart.clone().expect("chart"));
    let hit = cx.read(|cx| {
        chart
            .read(cx)
            .diagnostic_trading_order_hit_point(protected.client_order_id.as_str(), 177.0)
            .expect("painted protective line has an engine drag hit")
    });
    // The owner fills the target, but the pane still holds its earlier working publication.
    let filled =
        fill_owner_protective_target(&owner, &subscription, &clock, &instrument, &protected);
    cx.read(|cx| {
        assert_eq!(
            chart.read(cx).trading_snapshot().orders.len(),
            1,
            "pane still shows the stale working target"
        );
    });

    drag_pane_protection(cx, hit);
    assert_stale_protection_notice(&harness, cx);
    // Repaint the same stale publication to prove a second real drag deduplicates the key.
    harness.pane.update(cx, |pane, cx| {
        let store = pane.lifecycle.trading_store().expect("pane TradingStore");
        workspace_layout::apply_trading_snapshot(pane, &store, cx);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(chart.read(cx).trading_snapshot().orders.len(), 1));
    drag_pane_protection(cx, hit);
    assert_stale_protection_notice(&harness, cx);
    harness.pane.update(cx, |pane, cx| {
        let store = pane.lifecycle.trading_store().expect("pane TradingStore");
        store.update(cx, |store, cx| {
            store.apply_test_publication(filled);
            cx.notify();
        });
        workspace_layout::apply_trading_snapshot(pane, &store, cx);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let chart = pane.chart.as_ref().expect("chart").read(cx);
        assert!(
            chart.trading_snapshot().orders.is_empty(),
            "the adopted owner publication repaints the filled state"
        );
        assert!(chart.rendered_viewport_size().0 >= 500.0);
    });
    owner
        .shutdown(Duration::from_secs(2))
        .expect("owner stopped");
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[gpui::test]
fn pane_harness_open_session_owner_cancels_resting_orders_and_rejects_missing_quote(
    cx: &mut TestAppContext,
) {
    use aeris_trading::OrderStatus;
    use aeris_trading::market::{
        FeedReason, FeedState, MarketFeedError, MarketFeedEvent, QuoteInterest, QuoteInterestSink,
    };
    use aeris_trading_runtime::OrderCommandError;
    struct Interest;
    impl QuoteInterestSink for Interest {
        fn set_interest(&self, _: QuoteInterest) -> Result<(), MarketFeedError> {
            Ok(())
        }
    }
    let (owner, root, account, _, instrument, _) =
        trading_store::real_owner_fill_fixture_for("hyperliquid");
    let subscription = owner.subscribe().expect("owner subscription");
    let mut product = aeris_desktop::test_support::fixture_public_crypto();
    product.instrument_id = instrument.as_str().into();
    product.price_scale = 0;
    product.quantity_scale = 0;
    product.price_increment = Some(1);
    let (mut harness, cx) = PaneHarness::open_with_product_and_owner(
        WorkspaceSurfaceRestore::default(),
        false,
        product.clone(),
        ChartInterval::Minute1,
        Some(owner.clone()),
        cx,
    );
    harness.publish_snapshot(12, cx);
    harness.adopt_owner_publication(&subscription, cx);
    let first = place_scope_resting_order(&owner, &product, &account, "pane-scope-cancel");
    harness.adopt_owner_publication(&subscription, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let chart = pane.chart.as_ref().expect("populated chart").read(cx);
        assert_eq!(
            chart.trading_snapshot().orders.len(),
            1,
            "resting owner order is drawn"
        );
        assert_eq!(chart.trading_snapshot().orders[0].price, 99.0);
    });
    let cancelled = owner
        .cancel_all(Some(account.clone()))
        .expect("scoped cancel");
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].id, first.id);
    assert_eq!(cancelled[0].status, OrderStatus::Cancelled);
    harness.adopt_owner_publication(&subscription, cx);
    let second = place_scope_resting_order(&owner, &product, &account, "pane-scope-kill");
    harness.adopt_owner_publication(&subscription, cx);
    let feed = owner
        .attach_market_feed(Arc::new(Interest))
        .expect("scripted feed");
    feed.push(MarketFeedEvent::Feed {
        instrument_id: instrument.clone(),
        provider_generation: 1,
        state: FeedState::Unavailable {
            reason: FeedReason::QuoteMissing,
        },
    })
    .expect("remove owner quote");
    owner.status().expect("quote removal adopted");
    assert_owner_rejects_unpriced_modify(&owner, &second);
    let killed = owner
        .kill_switch(Some(account), "scope fixture".into(), 1_000_000_000_000)
        .expect("kill cancels resting order without quote");
    assert_eq!(killed.cancelled.len(), 1);
    assert_eq!(killed.cancelled[0].id, second.id);
    let stale = owner
        .cancel_order_checked(aeris_trading_runtime::RevisionedCancelOrder {
            client_order_id: second.client_order_id,
            expected_revision: Some(second.revision),
        })
        .expect_err("the owner rejects the retired revision");
    assert!(matches!(stale, OrderCommandError::StaleRevision { current }
        if current.id == second.id && current.status == OrderStatus::Cancelled));
    harness.adopt_owner_publication(&subscription, cx);
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(
            pane.chart
                .as_ref()
                .expect("chart")
                .read(cx)
                .trading_snapshot()
                .orders,
            []
        );
    });
    owner
        .shutdown(Duration::from_secs(2))
        .expect("owner stopped");
    std::fs::remove_dir_all(root).expect("remove test root");
}

#[gpui::test]
fn fresh_default_partial_terms_cover_chart_and_stay_ready_through_live_updates(
    cx: &mut TestAppContext,
) {
    use aeris_contracts::{ProviderContractMetadata, ProviderInstrumentClass};
    let mut product = aeris_desktop::test_support::fixture_public_crypto();
    product.contract_metadata = Some(Box::new(ProviderContractMetadata {
        instrument_class: Some(ProviderInstrumentClass::CryptoPerpetual as i32),
        currency: Some("USDC".into()),
        ..Default::default()
    }));
    assert!(aeris_market_runtime::market_service::instrument_terms_from_install(&product).is_err());
    let descriptor = aeris_market_runtime::InstrumentDescriptor::from_install(
        &product,
        aeris_market_runtime::built_in_provider_presentations()
            .iter()
            .find(|presentation| presentation.id == product.provider)
            .expect("published default provider"),
        true,
    )
    .expect("the provisional default has chart metadata without verified trading terms");

    let (harness, cx) = PaneHarness::open_with_product(
        WorkspaceSurfaceRestore::default(),
        false,
        product.clone(),
        ChartInterval::Minute1,
        cx,
    );
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::BoundDescriptor {
                generation: 1,
                descriptor: Box::new(descriptor),
                provider_display_name: "Hyperliquid".into(),
            },
        ))
        .expect("default descriptor");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::SelectedInstrument {
                generation: 1,
                instrument: product.clone(),
            },
        ))
        .expect("default selection");
    harness
        .pane
        .update(cx, |pane, cx| assert_eq!(pane.poll_market_worker(cx), 2));

    let series = engine_market_worker::series_key(&product, ChartInterval::Minute1)
        .expect("default minute series");
    let events = FixtureMarketEvents::default();
    let bar = fixture_bar();
    harness
        .publications
        .send(runtime_bar_snapshot_message(
            &product, &series, bar, &events, 1, 1,
        ))
        .expect("initial covering snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(
            pane.chart_state,
            ChartState::Error,
            "the initial snapshot must be accepted"
        );
        assert!(pane.chart.is_some(), "the covering bar installs a chart");
    });
    harness
        .publications
        .send(harness.fixture_readiness(
            1,
            aeris_contracts::SeriesLoadState::Live,
            "current coverage is live",
        ))
        .expect("runtime ready");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Ready);
    });
    for revision in 2..=4 {
        harness
            .publications
            .send(default_live_tail_event(
                &product, &series, bar, &events, revision,
            ))
            .expect("live update");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
            assert_eq!(
                pane.chart_state,
                ChartState::Ready,
                "live revision {revision}"
            );
            assert!(pane.chart.is_some());
            assert_eq!(pane.chart_failure, None);
        });
    }
}

fn default_live_tail_event(
    product: &InstallProviderInstrument,
    series: &aeris_market_data::BarSeriesKey,
    bar: aeris_market_data::MarketBar,
    events: &FixtureMarketEvents,
    revision: u64,
) -> PaneProjectionEvent {
    let timestamp = 300 + i64::try_from(revision - 1).expect("small revision") * 60;
    let tail = events.update(
        &FixtureSeries {
            series: series.clone(),
            provider_generation: product.session_generation,
            price_scale: u8::try_from(product.price_scale).expect("price scale"),
            quantity_scale: u8::try_from(product.quantity_scale).expect("quantity scale"),
        },
        1,
        revision,
        aeris_market_runtime::SeriesTailOperation::Append,
        aeris_market_data::MarketBar {
            source_sequence: revision,
            exchange_timestamp_seconds: timestamp,
            exchange_timestamp_unix_nanos: timestamp * 1_000_000_000,
            ..bar
        },
        false,
    );
    PaneProjectionEvent::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection::series(
        tail,
        timestamp * 1_000_000_000,
        None,
    ))
}

#[gpui::test]
fn pane_harness_four_restored_panes_apply_one_initial_reply_each(cx: &mut TestAppContext) {
    let saved_instrument = local_state::fixture_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("fixture product");
    let mut panes = Vec::new();
    let mut dispatches = Vec::new();
    for (index, (symbol, interval)) in [
        ("BTC", ChartInterval::Minute1),
        ("ETH", ChartInterval::Minute5),
        ("SOL", ChartInterval::Hour1),
        ("HYPE", ChartInterval::Day3),
    ]
    .into_iter()
    .enumerate()
    {
        let mut product = saved_instrument.clone();
        product.instrument_id = format!("hyperliquid:perp:{symbol}");
        product.provider_symbol = symbol.into();
        product.display_symbol = format!("{symbol}-USDC");
        product.entitlement_id = format!("saved-entitlement-{index}");
        let consumer_id = index as u64 + 1;
        let request = engine_market_worker::restored_selection_for_pane_harness(
            product.clone(),
            interval,
            consumer_id,
        );
        assert_eq!(request.consumer_id, consumer_id);
        assert_eq!(request.generation, 1);
        assert_eq!(request.interval, interval);
        assert_eq!(
            request.instrument,
            aeris_market_runtime::InstrumentRequest::restored(&product)
        );
        dispatches.push(request);
        let (harness, cx) = PaneHarness::open_with_product(
            WorkspaceSurfaceRestore::default(),
            false,
            product.clone(),
            interval,
            cx,
        );
        cx.read(|cx| {
            let pane = harness.pane.read(cx);
            assert_eq!(pane.product.as_ref(), Some(&product));
            assert_eq!(pane.interval, interval);
            assert!(pane.chart_pane_controller.accept_publication(1));
        });
        panes.push((harness, product, interval));
    }
    assert_eq!(
        dispatches.len(),
        4,
        "four exact restored selection commands"
    );
    assert_eq!(panes.len(), 4);
    for (harness, product, interval) in &mut panes {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                aeris_desktop::market_worker::LocalCommandOutcome::SelectedInstrument {
                    generation: 1,
                    instrument: product.clone(),
                },
            ))
            .expect("restored selection reply enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        let mut duplicate = product.clone();
        duplicate.price_increment = Some(999);
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                aeris_desktop::market_worker::LocalCommandOutcome::SelectedInstrument {
                    generation: 1,
                    instrument: duplicate,
                },
            ))
            .expect("duplicate reply enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        cx.read(|cx| {
            let pane = harness.pane.read(cx);
            assert_eq!(pane.product.as_ref(), Some(&*product));
            assert_eq!(pane.interval, *interval);
            assert!(pane.chart_pane_controller.accept_publication(1));
        });
        assert!(
            harness
                .commands
                .as_ref()
                .expect("command receiver")
                .try_iter()
                .all(|command| !matches!(command, MarketWorkerCommand::EngineSelect(_))),
            "a restored Selected reply cannot dispatch a follow-up selection"
        );
    }
}

#[gpui::test]
fn pane_harness_one_user_selection_command_and_one_applied_reply(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let requests = harness.commands.as_ref().expect("command receiver");
    assert!(
        requests
            .try_iter()
            .all(|command| !matches!(command, MarketWorkerCommand::EngineSelect(_))),
        "startup sends no user selection"
    );
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let selections: Vec<_> = requests
        .try_iter()
        .filter_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request),
            _ => None,
        })
        .collect();
    let [request] = selections.as_slice() else {
        panic!("exactly one selection command");
    };
    assert_eq!(request.interval, ChartInterval::Minute5);
    let sequence = request.sequence;
    for _ in 0..2 {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::EngineSelection {
                    sequence,
                    succeeded: true,
                },
            ))
            .expect("fixture reply enters mailbox");
    }
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 2);
        assert_eq!(pane.interval, ChartInterval::Minute5);
        assert_eq!(pane.chart_pane_controller.pending_selection(), None);
        assert!(pane.chart_pane_controller.accept_publication(sequence));
    });
    assert!(
        requests
            .try_iter()
            .all(|command| !matches!(command, MarketWorkerCommand::EngineSelect(_))),
        "Selected sends no second selection"
    );
}

#[gpui::test]
fn pane_harness_session_status_rejects_retired_selection_after_marker(cx: &mut TestAppContext) {
    use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let old_status = MarketSessionStatus {
        instrument_id: cx.read(|cx| {
            harness
                .pane
                .read(cx)
                .product
                .as_ref()
                .expect("product")
                .instrument_id
                .clone()
        }),
        phase: MarketSessionPhase::Closed,
        source: MarketSessionSource::ProviderCalendar,
        session_start_unix_nanos: None,
        session_end_unix_nanos: None,
        next_open_unix_nanos: None,
    };
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection request");
    harness
        .publications
        .send(PaneProjectionEvent::SessionStatus {
            generation: 1,
            status: LocalSessionStatus::MarketSession(old_status.clone()),
        })
        .expect("old status while pending");
    harness.pane.update(cx, |pane, cx| {
        pane.poll_market_worker(cx);
        assert_eq!(pane.market_session_status.as_ref(), Some(&old_status));
    });
    harness
        .publications
        .send(PaneProjectionEvent::SessionStatus {
            generation: 1,
            status: LocalSessionStatus::MarketSession(MarketSessionStatus {
                instrument_id: "uncommitted instrument".into(),
                ..old_status.clone()
            }),
        })
        .expect("catalog status ahead of selection");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.market_session_status.as_ref(), Some(&old_status));
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker");
    harness
        .publications
        .send(PaneProjectionEvent::SessionStatus {
            generation: 1,
            status: LocalSessionStatus::MarketSession(old_status.clone()),
        })
        .expect("queued stale status");
    let new_status = MarketSessionStatus {
        phase: MarketSessionPhase::Regular,
        ..old_status.clone()
    };
    harness
        .publications
        .send(PaneProjectionEvent::SessionStatus {
            generation: sequence,
            status: LocalSessionStatus::MarketSession(new_status.clone()),
        })
        .expect("new status");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 3);
        assert_eq!(pane.market_session_status.as_ref(), Some(&new_status));
    });
}

#[gpui::test]
fn pane_harness_order_book_rejects_retired_selection_even_with_higher_revision(
    cx: &mut TestAppContext,
) {
    use aeris_market_data::{OrderBookPublication, OrderBookState};

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let product = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .product
            .clone()
            .expect("fixture product")
    });
    let events = FixtureMarketEvents::default();
    let frame = |generation, revision| {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(events.order_book(
                generation,
                OrderBookPublication {
                    provider_id: product.provider.clone(),
                    instrument_id: product.instrument_id.clone(),
                    entitlement_id: product.entitlement_id.clone(),
                    session_generation: product.session_generation,
                    revision,
                    source_watermark: revision,
                    bbo_source_watermark: revision,
                    state: OrderBookState::Ready,
                    bids: vec![],
                    asks: vec![],
                    best_bid: None,
                    best_ask: None,
                    traded_volumes: std::collections::BTreeMap::default(),
                    trade_source_watermark: revision,
                    top_of_book_only: false,
                },
            )),
        )
    };
    harness
        .publications
        .send(frame(1, 1))
        .expect("initial order book");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.order_book.read(cx).frame().map(|f| f.revision),
            Some(1)
        );
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    harness
        .publications
        .send(frame(sequence, 90))
        .expect("early replacement frame");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(
            pane.order_book.read(cx).frame().map(|f| f.revision),
            Some(90)
        );
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("selection marker");
    harness
        .publications
        .send(frame(1, 99))
        .expect("retired higher revision");
    harness
        .publications
        .send(frame(sequence, 2))
        .expect("current frame");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 3);
        let book = pane.order_book.read(cx);
        assert_eq!(book.frame().map(|f| f.selection_generation), Some(sequence));
        assert_eq!(book.frame().map(|f| f.revision), Some(2));
    });
}

#[gpui::test]
fn pane_harness_failed_pending_interval_restores_chart_and_deduplicates_notice(
    cx: &mut TestAppContext,
) {
    use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let (old_chart, old_product) = cx.read(|cx| {
        let pane = harness.pane.read(cx);
        (
            pane.chart.clone().expect("original chart"),
            pane.product.clone(),
        )
    });
    let old_status = MarketSessionStatus {
        instrument_id: old_product
            .as_ref()
            .expect("initial instrument")
            .instrument_id
            .clone(),
        phase: MarketSessionPhase::Regular,
        source: MarketSessionSource::ProviderCalendar,
        session_start_unix_nanos: None,
        session_end_unix_nanos: None,
        next_open_unix_nanos: None,
    };
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            assert!(pane.select_interval(ChartInterval::Minute5, cx));
        });
        let sequence = harness
            .commands
            .as_ref()
            .expect("command receiver")
            .try_iter()
            .find_map(|command| match command {
                MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
                _ => None,
            })
            .expect("selection request");
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::EngineSelection {
                    sequence,
                    succeeded: false,
                },
            ))
            .expect("selection failure enters mailbox");
        harness
            .publications
            .send(PaneProjectionEvent::SessionStatus {
                generation: 1,
                status: LocalSessionStatus::MarketSession(old_status.clone()),
            })
            .expect("previous chart status after rejected selection");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 2);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let pane = harness.pane.read(cx);
            assert_eq!(
                pane.chart.as_ref().map(Entity::entity_id),
                Some(old_chart.entity_id())
            );
            assert_eq!(pane.product, old_product);
            assert_eq!(pane.interval, ChartInterval::Minute1);
            assert_eq!(pane.chart_state, ChartState::Ready);
            assert_eq!(pane.market_session_status.as_ref(), Some(&old_status));
            let toaster = &harness.window.read(cx).toaster;
            assert_eq!(toaster.notices().len(), 1);
            assert_eq!(toaster.notices()[0].title, "Timeframe unavailable");
            assert_eq!(
                toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
                1
            );
        });
    }
}

#[gpui::test]
fn pane_harness_retired_recovery_cannot_fail_the_replacement_chart(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness
        .publications
        .send(harness.fixture_recovering(1, "fixture stream invalid"))
        .expect("stream invalidation");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let request_id = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::Recovery { command, .. } => Some(command.request_id),
            _ => None,
        })
        .expect("recovery request");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("replacement selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("replacement marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let before = cx.read(|cx| harness.pane.read(cx).chart_state);
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            aeris_desktop::market_worker::LocalCommandOutcome::Recovery {
                generation: 1,
                request_id,
                failure: aeris_desktop::market_worker::MarketRecoveryFailure::Failed(
                    "retired private recovery failure".into(),
                ),
            },
        ))
        .expect("retired recovery");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, before);
        assert_eq!(pane.chart_failure, None);
    });
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_recovery_keeps_fenced_snapshot_until_exact_request_projection(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness
        .publications
        .send(harness.fixture_recovering(1, "fixture stream invalid"))
        .expect("recovery state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let request_id = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::Recovery { command, .. } => Some(command.request_id),
            _ => None,
        })
        .expect("recovery request");
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let PaneProjectionEvent::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection {
        event,
        received_unix_nanos: Some(received_unix_nanos),
        ..
    }) = harness.fixture_snapshot(&product, ChartInterval::Minute1, 1, 12)
    else {
        unreachable!("fixture publishes a canonical snapshot");
    };
    let epoch = event.epoch;
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection {
                event,
                received_unix_nanos: Some(received_unix_nanos),
                recovery_request_id: Some(request_id + 1),
                selection_generation: None,
            },
        ))
        .expect("stale request");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Recovering);
    });
    let PaneProjectionEvent::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection {
        event,
        received_unix_nanos: Some(received_unix_nanos),
        ..
    }) = harness.fixture_snapshot(&product, ChartInterval::Minute1, 1, 12)
    else {
        unreachable!("fixture publishes a canonical snapshot");
    };
    assert_eq!(event.epoch, epoch);
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection {
                event,
                received_unix_nanos: Some(received_unix_nanos),
                recovery_request_id: Some(request_id),
                selection_generation: None,
            },
        ))
        .expect("current recovery");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Ready);
        assert_eq!(pane.chart_failure, None);
    });
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_failed_committed_interval_restores_previous_chart_with_one_notice(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let prior_descriptor = harness.pane.update(cx, |pane, _| {
        let descriptor = pane.descriptor.as_mut().expect("bound descriptor");
        descriptor.venue = "Fixture Venue".into();
        descriptor.clone()
    });
    let (old_chart, old_product) = cx.read(|cx| {
        let pane = harness.pane.read(cx);
        (
            pane.chart.clone().expect("original chart"),
            pane.product.clone(),
        )
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection request");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let state_before_retired_failure = cx.read(|cx| harness.pane.read(cx).chart_state);
    harness
        .publications
        .send(harness.fixture_readiness(
            1,
            aeris_contracts::SeriesLoadState::Failed,
            "retired provider failure",
        ))
        .expect("retired failure enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, state_before_retired_failure);
        assert_eq!(pane.chart_failure, None);
    });
    harness
        .publications
        .send(harness.fixture_readiness(
            sequence,
            aeris_contracts::SeriesLoadState::Failed,
            "private provider failure",
        ))
        .expect("failure enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.product, old_product);
        assert_eq!(pane.descriptor.as_ref(), Some(&prior_descriptor));
        assert_eq!(pane.interval, ChartInterval::Minute1);
        assert_eq!(
            pane.chart.as_ref().map(Entity::entity_id),
            Some(old_chart.entity_id())
        );
        assert_eq!(pane.chart_state, ChartState::Ready);
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Timeframe unavailable");
        assert!(!notices[0].detail.contains("private provider failure"));
    });
}

#[gpui::test]
fn pane_harness_failed_committed_instrument_keeps_original_chart_and_deduplicates_notice(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let (old_chart, old_product) = cx.read(|cx| {
        let pane = harness.pane.read(cx);
        (
            pane.chart.clone().expect("original chart"),
            pane.product.clone().expect("original product"),
        )
    });
    let mut replacement = old_product.clone();
    replacement.instrument_id = "different-instrument".into();
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            assert!(pane.select_installed_instrument(&replacement, cx));
        });
        let sequence = harness
            .commands
            .as_ref()
            .expect("command receiver")
            .try_iter()
            .filter_map(|command| match command {
                MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
                _ => None,
            })
            .last()
            .expect("selection request");
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::EngineSelection {
                    sequence,
                    succeeded: true,
                },
            ))
            .expect("marker enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        harness
            .publications
            .send(harness.fixture_readiness(
                sequence,
                aeris_contracts::SeriesLoadState::Failed,
                "private provider failure",
            ))
            .expect("failure enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let pane = harness.pane.read(cx);
            assert_eq!(pane.product.as_ref(), Some(&old_product));
            assert_eq!(pane.interval, ChartInterval::Minute1);
            assert_eq!(
                pane.chart.as_ref().map(Entity::entity_id),
                Some(old_chart.entity_id())
            );
            assert_eq!(pane.chart_state, ChartState::Ready);
            let notices = harness.window.read(cx).toaster.notices();
            assert_eq!(notices.len(), 1);
            assert_eq!(notices[0].title, "Market selection unavailable");
            assert!(!notices[0].detail.contains("private provider failure"));
        });
    }
}

#[gpui::test]
fn pane_harness_interval_switch_covers_chart_until_replacement(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let original = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .chart
            .as_ref()
            .expect("original chart")
            .entity_id()
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("interval selection");
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(
            pane.showing_superseded_series(),
            "pending interval-only switch"
        );
        assert_eq!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(
            pane.showing_superseded_series(),
            "committed interval-only switch"
        );
        assert_eq!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
        assert_eq!(pane.interval, ChartInterval::Minute5);
    });
}

#[gpui::test]
fn pane_harness_bar_publications_reject_retired_selection(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let original = cx
        .read(|cx| harness.pane.read(cx).chart.as_ref().map(Entity::entity_id))
        .expect("original chart");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let early = harness.fixture_snapshot(&product, ChartInterval::Minute5, sequence, 13);
    let retired = harness.fixture_snapshot(&product, ChartInterval::Minute1, 1, 13);
    let current = harness.fixture_snapshot(&product, ChartInterval::Minute5, sequence, 13);
    harness
        .publications
        .send(early)
        .expect("early replacement snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.interval, ChartInterval::Minute1);
        assert_eq!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker");
    harness
        .publications
        .send(retired)
        .expect("retired snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 2);
        assert_eq!(pane.interval, ChartInterval::Minute5);
        assert_eq!(
            pane.chart.as_ref().map(Entity::entity_id),
            Some(original),
            "retired bars must not install into the new selection"
        );
    });
    harness
        .publications
        .send(current)
        .expect("replacement snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
    });
    let current_label = cx.read(|cx| harness.pane.read(cx).replay_label.clone());
    let stale_delta = harness.market_events.update(
        &FixtureSeries {
            series: engine_market_worker::series_key(&product, ChartInterval::Minute1)
                .expect("retired series"),
            provider_generation: product.session_generation,
            price_scale: u8::try_from(product.price_scale).expect("price scale"),
            quantity_scale: u8::try_from(product.quantity_scale).expect("quantity scale"),
        },
        1,
        100,
        aeris_market_runtime::SeriesTailOperation::Append,
        aeris_market_data::MarketBar {
            source_sequence: 14,
            exchange_timestamp_seconds: 840,
            exchange_timestamp_unix_nanos: 840_000_000_000,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 7,
        },
        true,
    );
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection {
                event: stale_delta,
                received_unix_nanos: Some(841_000_000_000),
                recovery_request_id: None,
                selection_generation: None,
            },
        ))
        .expect("retired update");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.replay_label, current_label);
    });
}

fn check_snapshot_switch_order(order: [u8; 3], cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let original = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .chart
            .as_ref()
            .expect("original chart")
            .entity_id()
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("interval selection");
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let events = FixtureMarketEvents::default();
    let mut committed = false;
    let mut replacement_installed = false;
    for event in order {
        let message = match event {
            0 => PaneProjectionEvent::LocalCommandOutcome(LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            }),
            1 | 2 => {
                let interval = if event == 1 {
                    ChartInterval::Minute1
                } else {
                    ChartInterval::Minute5
                };
                let series =
                    engine_market_worker::series_key(&product, interval).expect("fixture series");
                runtime_bar_snapshot_message(
                    &product,
                    &series,
                    fixture_bar(),
                    &events,
                    if event == 1 { 1 } else { sequence },
                    14,
                )
            }
            _ => panic!("unknown switch event"),
        };
        harness.publications.send(message).expect("fixture message");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1, "order {order:?}");
            if event == 0 {
                committed = true;
            } else if event == 2 && committed {
                replacement_installed = true;
            }
            assert_eq!(
                pane.chart.as_ref().map(Entity::entity_id) != Some(original),
                replacement_installed,
                "order {order:?}, event {event}"
            );
            assert_eq!(
                pane.chart_pane_controller.chart_needs_replacement(),
                committed && !replacement_installed,
                "order {order:?}, event {event}"
            );
        });
    }
}

#[gpui::test]
fn pane_harness_snapshot_order_commit_old_new(cx: &mut TestAppContext) {
    check_snapshot_switch_order([0, 1, 2], cx);
}

#[gpui::test]
fn pane_harness_snapshot_order_commit_new_old(cx: &mut TestAppContext) {
    check_snapshot_switch_order([0, 2, 1], cx);
}

#[gpui::test]
fn pane_harness_snapshot_order_old_commit_new(cx: &mut TestAppContext) {
    check_snapshot_switch_order([1, 0, 2], cx);
}

#[gpui::test]
fn pane_harness_snapshot_order_old_new_commit(cx: &mut TestAppContext) {
    check_snapshot_switch_order([1, 2, 0], cx);
}

#[gpui::test]
fn pane_harness_snapshot_order_new_commit_old(cx: &mut TestAppContext) {
    check_snapshot_switch_order([2, 0, 1], cx);
}

#[gpui::test]
fn pane_harness_snapshot_order_new_old_commit(cx: &mut TestAppContext) {
    check_snapshot_switch_order([2, 1, 0], cx);
}

#[gpui::test]
fn pane_harness_rapid_interval_storm_keeps_final_day3_binding(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let requests = harness.commands.as_ref().expect("command receiver");
    let reply = |sequence| {
        PaneProjectionEvent::LocalCommandOutcome(LocalCommandOutcome::EngineSelection {
            sequence,
            succeeded: true,
        })
    };

    // Start on a different interval so the first 1m action is a real request.
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Hour4, cx));
    });
    let initial = requests
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("initial interval request");
    harness
        .publications
        .send(reply(initial))
        .expect("initial selection");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });

    let mut sequences = Vec::new();
    for interval in [
        ChartInterval::Minute1,
        ChartInterval::Hour1,
        ChartInterval::Day1,
        ChartInterval::Day3,
        ChartInterval::Minute5,
        ChartInterval::Day3,
    ] {
        harness.pane.update(cx, |pane, cx| {
            assert!(pane.select_interval(interval, cx), "{interval:?}");
        });
        let sequence = requests
            .try_iter()
            .find_map(|command| match command {
                MarketWorkerCommand::EngineSelect(request) => {
                    assert_eq!(request.interval, interval);
                    Some(request.sequence)
                }
                _ => None,
            })
            .expect("one request per distinct pending interval");
        sequences.push(sequence);
        cx.read(|cx| {
            let pane = harness.pane.read(cx);
            assert_eq!(
                pane.chart_pane_controller.pending_selection(),
                Some(sequence)
            );
            assert_eq!(
                pane.chart_pane_controller.pending_interval(),
                Some(interval)
            );
            assert_eq!(pane.interval, ChartInterval::Hour4);
        });
    }

    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let events = FixtureMarketEvents::default();
    let publication = |generation| {
        let interval = if generation == *sequences.last().expect("final request") {
            ChartInterval::Day3
        } else {
            ChartInterval::Hour4
        };
        let series = engine_market_worker::series_key(&product, interval).expect("fixture series");
        runtime_bar_snapshot_message(&product, &series, fixture_bar(), &events, generation, 14)
    };
    let final_generation = *sequences.last().expect("final request");
    deliver_retired_interval_outcomes(&mut harness, cx, &sequences, publication);
    harness
        .publications
        .send(publication(final_generation))
        .expect("early final publication");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.interval, ChartInterval::Hour4);
    });
    harness
        .publications
        .send(reply(final_generation))
        .expect("final reply");
    harness
        .publications
        .send(publication(final_generation))
        .expect("final publication");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 2);
        assert_eq!(pane.chart_pane_controller.pending_selection(), None);
        assert_eq!(pane.interval, ChartInterval::Day3);
        assert_eq!(pane.interval.label(), "3D");
        assert!(!pane.chart_pane_controller.chart_needs_replacement());
    });
}

fn deliver_retired_interval_outcomes(
    harness: &mut PaneHarness,
    cx: &mut VisualTestContext,
    sequences: &[u64],
    publication: impl Fn(u64) -> PaneProjectionEvent,
) {
    let final_generation = *sequences.last().expect("final request");
    for &generation in sequences[..sequences.len() - 1].iter().rev() {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::EngineSelection {
                    sequence: generation,
                    succeeded: true,
                },
            ))
            .expect("retired reply");
        harness
            .publications
            .send(publication(generation))
            .expect("retired publication");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 2);
            assert_eq!(pane.interval, ChartInterval::Hour4);
            assert_eq!(
                pane.chart_pane_controller.pending_selection(),
                Some(final_generation)
            );
        });
    }
}

fn fixture_bar() -> aeris_market_data::MarketBar {
    aeris_market_data::MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 300,
        exchange_timestamp_unix_nanos: 300_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    }
}

fn runtime_bar_snapshot_message(
    product: &InstallProviderInstrument,
    series: &aeris_market_data::BarSeriesKey,
    bar: aeris_market_data::MarketBar,
    events: &FixtureMarketEvents,
    generation: u64,
    publication_generation: u64,
) -> PaneProjectionEvent {
    PaneProjectionEvent::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection {
        event: events.snapshot(
            &FixtureSeries {
                series: series.clone(),
                provider_generation: product.session_generation,
                price_scale: u8::try_from(product.price_scale).expect("price scale"),
                quantity_scale: u8::try_from(product.quantity_scale).expect("quantity scale"),
            },
            generation,
            publication_generation,
            [bar].as_slice(),
        ),
        received_unix_nanos: Some(301_000_000_000),
        recovery_request_id: None,
        selection_generation: None,
    })
}

fn publish_runtime_bar_tail(
    harness: &mut PaneHarness,
    cx: &mut VisualTestContext,
    series: aeris_market_data::BarSeriesKey,
    bar: aeris_market_data::MarketBar,
    provider_generation: aeris_market_runtime::MarketProviderGeneration,
    events: &FixtureMarketEvents,
    sequence: u64,
) {
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection {
                event: events.update(
                    &FixtureSeries {
                        series,
                        provider_generation: provider_generation.0.get(),
                        price_scale: u8::try_from(product.price_scale).expect("price scale"),
                        quantity_scale: u8::try_from(product.quantity_scale)
                            .expect("quantity scale"),
                    },
                    sequence,
                    2,
                    aeris_market_runtime::SeriesTailOperation::Append,
                    aeris_market_data::MarketBar {
                        source_sequence: 2,
                        exchange_timestamp_seconds: 600,
                        exchange_timestamp_unix_nanos: 600_000_000_000,
                        ..bar
                    },
                    true,
                ),
                received_unix_nanos: Some(601_000_000_000),
                recovery_request_id: None,
                selection_generation: None,
            },
        ))
        .expect("tail update");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.interval, ChartInterval::Minute5);
        assert!(!pane.chart_pane_controller.chart_needs_replacement());
    });
}

#[gpui::test]
fn pane_harness_fenced_runtime_bars_project_only_after_selection_commit(cx: &mut TestAppContext) {
    use aeris_market_data::MarketBar;
    use aeris_market_runtime::MarketProviderGeneration;
    use std::num::NonZeroU64;

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let series = engine_market_worker::series_key(&product, ChartInterval::Minute5)
        .expect("replacement series");
    let bar = MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 300,
        exchange_timestamp_unix_nanos: 300_000_000_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    };
    let provider_generation =
        MarketProviderGeneration(NonZeroU64::new(product.session_generation).expect("provider"));
    let events = FixtureMarketEvents::default();
    let snapshot = |generation, publication_generation| {
        runtime_bar_snapshot_message(
            &product,
            &series,
            bar,
            &events,
            generation,
            publication_generation,
        )
    };
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    harness
        .publications
        .send(snapshot(sequence, 1))
        .expect("early snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.interval, ChartInterval::Minute1);
        assert!(!pane.chart_pane_controller.chart_needs_replacement());
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert!(pane.chart_pane_controller.chart_needs_replacement());
    });
    harness
        .publications
        .send(snapshot(1, 99))
        .expect("retired higher revision");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert!(pane.chart_pane_controller.chart_needs_replacement());
    });
    harness
        .publications
        .send(snapshot(sequence, 1))
        .expect("covering snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert!(!pane.chart_pane_controller.chart_needs_replacement());
        assert_eq!(pane.interval, ChartInterval::Minute5);
    });
    publish_runtime_bar_tail(
        &mut harness,
        cx,
        series,
        bar,
        provider_generation,
        &events,
        sequence,
    );
}

#[gpui::test]
fn pane_harness_fenced_runtime_readiness_projects_only_committed_selection(
    cx: &mut TestAppContext,
) {
    use aeris_contracts::SeriesLoadState;
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let events = FixtureMarketEvents::default();
    let readiness = |generation, state, detail: &str| {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(
                events.readiness(generation, state, detail),
            ),
        )
    };
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    harness
        .publications
        .send(readiness(sequence, SeriesLoadState::Live, "early live"))
        .expect("early runtime state");
    let before = cx.read(|cx| harness.pane.read(cx).chart_state);
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, before);
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    harness
        .publications
        .send(readiness(1, SeriesLoadState::Live, "retired live"))
        .expect("retired runtime state");
    harness.pane.update(cx, |pane, cx| {
        let before = pane.chart_state;
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, before);
    });
    harness
        .publications
        .send(readiness(
            sequence,
            SeriesLoadState::Empty,
            "waiting for trades",
        ))
        .expect("current runtime state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::AwaitingData);
    });
    harness
        .publications
        .send(readiness(sequence, SeriesLoadState::Live, "live handoff"))
        .expect("live runtime state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Ready);
    });
    harness
        .publications
        .send(readiness(
            sequence,
            SeriesLoadState::Partial,
            "background history repair",
        ))
        .expect("live backfill state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.chart_state,
            ChartState::Ready,
            "a backfill after live handoff must not return the pane to loading"
        );
    });
}

#[gpui::test]
fn pane_harness_fenced_runtime_failure_restores_previous_binding(cx: &mut TestAppContext) {
    use aeris_contracts::SeriesLoadState;
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let events = FixtureMarketEvents::default();
    let failed = |generation| {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(events.readiness(
                generation,
                SeriesLoadState::Failed,
                "market unavailable",
            )),
        )
    };
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    harness.publications.send(failed(sequence)).expect("early");
    harness.pane.update(cx, |pane, cx| {
        let previous = pane.chart_state;
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, previous);
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    harness.publications.send(failed(1)).expect("retired");
    harness.pane.update(cx, |pane, cx| {
        let previous = pane.chart_state;
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, previous);
    });
    harness
        .publications
        .send(failed(sequence))
        .expect("current");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        // The replacement failed before its covering snapshot: the pane
        // restores the previous chart rather than failing that displayed chart.
        assert_eq!(pane.chart_state, ChartState::Ready);
        assert!(!pane.chart_pane_controller.accept_series_state(sequence));
    });
}

#[gpui::test]
fn pane_harness_runtime_demand_error_projects_only_current_series(cx: &mut TestAppContext) {
    use aeris_market_runtime::MarketDemandErrorScope;

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let events = FixtureMarketEvents::default();
    let error = |generation, scope| {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(events.demand_error(
                generation,
                scope,
                "coverage unavailable",
            )),
        )
    };
    harness
        .publications
        .send(error(2, MarketDemandErrorScope::Series))
        .expect("future failure");
    harness
        .publications
        .send(error(1, MarketDemandErrorScope::OlderHistory))
        .expect("older history failure");
    harness.pane.update(cx, |pane, cx| {
        let previous = pane.chart_state;
        assert_eq!(pane.poll_market_worker(cx), 2);
        assert_eq!(pane.chart_state, previous);
    });
    harness
        .publications
        .send(error(1, MarketDemandErrorScope::Series))
        .expect("current failure");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Recovering);
    });
}

#[gpui::test]
fn pane_harness_series_state_rejects_early_and_retired_selection(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    let state = |generation, load_state| {
        harness.fixture_readiness(generation, load_state, "fixture series state")
    };
    let pending_state = cx.read(|cx| harness.pane.read(cx).chart_state);
    harness
        .publications
        .send(state(sequence, aeris_contracts::SeriesLoadState::Live))
        .expect("early state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, pending_state);
        assert_eq!(pane.interval, ChartInterval::Minute1);
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let committed_state = cx.read(|cx| harness.pane.read(cx).chart_state);
    harness
        .publications
        .send(state(1, aeris_contracts::SeriesLoadState::Failed))
        .expect("retired state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, committed_state);
    });
    harness
        .publications
        .send(harness.fixture_recovering(sequence, "fixture series recovering"))
        .expect("current state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Recovering);
    });
}

#[gpui::test]
fn pane_harness_history_unavailable_holds_until_the_runtime_clears_it(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness.pane.update(cx, |pane, _| {
        assert_eq!(pane.chart_state, ChartState::Ready);
    });
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(
                harness.market_events.readiness_with_presentation(
                    1,
                    aeris_contracts::SeriesLoadState::Live,
                    "History unavailable; showing live bars",
                    aeris_market_runtime::SeriesPresentationStatus::HistoryUnavailable,
                ),
            ),
        ))
        .expect("live-only status");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::HistoryUnavailable);
    });
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let bars_only = harness.fixture_snapshot(&product, ChartInterval::Minute1, 1, 13);
    harness
        .publications
        .send(bars_only)
        .expect("bar publication enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.chart_state,
            ChartState::HistoryUnavailable,
            "a bar publication alone must not clear the unavailable phase"
        );
    });
    harness
        .publications
        .send(harness.fixture_readiness(
            1,
            aeris_contracts::SeriesLoadState::Live,
            "Market history/live handoff is current",
        ))
        .expect("recovered live status");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Ready);
    });
}

#[gpui::test]
fn pane_harness_old_recovery_failure_during_pending_switch_keeps_request(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness
        .publications
        .send(harness.fixture_recovering(1, "fixture stream invalid"))
        .expect("stream invalidation");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let request_id = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::Recovery { command, .. } => Some(command.request_id),
            _ => None,
        })
        .expect("recovery request");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("replacement selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            aeris_desktop::market_worker::LocalCommandOutcome::Recovery {
                generation: 1,
                request_id,
                failure: aeris_desktop::market_worker::MarketRecoveryFailure::Failed(
                    "private old recovery failure".into(),
                ),
            },
        ))
        .expect("old recovery");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.chart_pane_controller.pending_selection(),
            Some(sequence)
        );
        assert_eq!(pane.chart_failure, None);
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("replacement marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.interval, ChartInterval::Minute5);
        assert_eq!(pane.chart_failure, None);
    });
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_old_series_error_during_pending_switch_does_not_cancel_selection(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    harness
        .publications
        .send(harness.fixture_readiness(
            1,
            aeris_contracts::SeriesLoadState::Failed,
            "retired private series failure",
        ))
        .expect("old series error");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.chart_pane_controller.pending_selection(),
            Some(sequence)
        );
        assert_eq!(pane.chart_state, ChartState::Loading);
    });
    for state in [
        aeris_contracts::SeriesLoadState::Live,
        aeris_contracts::SeriesLoadState::Empty,
    ] {
        harness
            .publications
            .send(harness.fixture_readiness(1, state, "retired series settled"))
            .expect("old series lifecycle");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
            assert_eq!(
                pane.chart_pane_controller.pending_selection(),
                Some(sequence)
            );
            assert_eq!(pane.chart_state, ChartState::Loading);
        });
    }
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.interval, ChartInterval::Minute5);
        assert_eq!(pane.chart_failure, None);
    });
}

#[gpui::test]
fn pane_harness_catalog_request_fences_old_series_before_catalog_reply(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let provider = harness.pane.update(cx, |pane, _| {
        let search = chart_provider_search(pane, "ETH");
        assert!(pane.symbol_browser.apply_results(
            search.provider,
            search.request_id,
            vec![ProviderInstrumentSummary {
                symbol: "ETH".into(),
                display_symbol: "ETH".into(),
                exchange: "TEST".into(),
                ..ProviderInstrumentSummary::default()
            }]
        ));
        search.provider
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_instrument(
            InstrumentMenuSelection::SearchResult { provider, index: 0 },
            SymbolSelectionTarget::Chart,
            cx,
        ));
    });
    let request = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::SelectCatalog(request) => Some(request),
            _ => None,
        })
        .expect("catalog request");
    harness.pane.update(cx, |pane, _| {
        assert_eq!(
            pane.chart_pane_controller.pending_selection(),
            Some(request.sequence),
            "the request must fence lifecycle before its catalog reply"
        );
    });
    harness
        .publications
        .send(harness.fixture_readiness(
            1,
            aeris_contracts::SeriesLoadState::Failed,
            "retired private error",
        ))
        .expect("old series error");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Ready);
        assert_eq!(
            pane.chart_pane_controller.pending_selection(),
            Some(request.sequence)
        );
    });
    let failure = LocalCommandOutcome::CatalogSwitch {
        sequence: request.sequence,
        browser_generation: request.browser_generation,
        result: Err("catalog failed".into()),
    };
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(failure))
        .expect("catalog failure");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_pane_controller.pending_selection(), None);
        assert!(pane.chart_pane_controller.accept_series_state(1));
        assert_eq!(pane.chart_state, ChartState::Ready);
    });
}

#[gpui::test]
fn pane_harness_study_reinitialization_outcome_preserves_failure_and_success(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let study_id = StudyInstanceId::try_from_u64(3).expect("study identity");
    let original = workspace_surface::legacy_runtime_study(3, ChartIndicator::Sma, true)
        .expect("study fixture");
    let mut replacement = original.clone();
    replacement.settings = vec![WorkspaceStudySettingState {
        identifier: "length".to_string(),
        value: Some(workspace_study_setting_state::Value::Integer(42)),
    }];
    harness.pane.update(cx, |pane, _| {
        let product = pane.product.as_ref().expect("selected instrument");
        let series = engine_market_worker::series_key(product, ChartInterval::Minute1)
            .expect("selected series");
        pane.studies.activate(RuntimeStudyState {
            study_id,
            persisted: original.clone(),
            resolved_chart_series: Some(series.clone()),
        });
        pane.studies.begin_reinitialization(
            study_id,
            PendingStudyReinitialization {
                series,
                replacement_settings: Some(replacement.settings.clone()),
            },
        );
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::StudyReinitialization {
                study_id,
                result: Err("private-provider-rejection".into()),
            },
        ))
        .expect("reinitialization failure");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.studies.active()[0].persisted, original);
        assert!(!pane.studies.is_reinitializing(study_id));
        assert!(!pane.studies.suppresses_output(study_id));
    });
    harness.pane.update(cx, |pane, _| {
        let product = pane.product.as_ref().expect("selected instrument");
        let series = engine_market_worker::series_key(product, ChartInterval::Minute1)
            .expect("selected series");
        pane.studies.begin_reinitialization(
            study_id,
            PendingStudyReinitialization {
                series,
                replacement_settings: Some(replacement.settings.clone()),
            },
        );
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::StudyReinitialization {
                study_id,
                result: Ok(()),
            },
        ))
        .expect("reinitialization success");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.studies.active()[0].persisted, replacement);
        assert!(!pane.studies.is_reinitializing(study_id));
        assert!(pane.persistence.is_dirty());
    });
}

/// The SMA periods of the study reinitializations sent since the last call.
fn reinitialized_sma_periods(harness: &PaneHarness) -> Vec<i64> {
    harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .filter_map(|command| match command {
            MarketWorkerCommand::ReinitializeStudy(request) => match request
                .registration
                .settings
                .get(aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING)
            {
                Some(StudySettingValue::Integer(period)) => Some(*period),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn persisted_sma_period(pane: &WorkspaceSurface) -> Option<i64> {
    pane.studies.active()[0]
        .persisted
        .settings
        .iter()
        .find(|setting| setting.identifier == aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING)
        .and_then(|setting| match setting.value {
            Some(workspace_study_setting_state::Value::Integer(period)) => Some(period),
            _ => None,
        })
}

fn complete_study_reinitialization(
    harness: &PaneHarness,
    study_id: StudyInstanceId,
    cx: &mut VisualTestContext,
) {
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::StudyReinitialization {
                study_id,
                result: Ok(()),
            },
        ))
        .expect("reinitialization success");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
}

#[gpui::test]
fn pane_harness_study_dialog_previews_edits_one_request_at_a_time_and_cancel_restores(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let study_id = StudyInstanceId::try_from_u64(3).expect("study identity");
    let original = workspace_surface::legacy_runtime_study(3, ChartIndicator::Sma, true)
        .expect("study fixture");
    harness.pane.update(cx, |pane, _| {
        let product = pane.product.as_ref().expect("selected instrument");
        let series = engine_market_worker::series_key(product, ChartInterval::Minute1)
            .expect("selected series");
        pane.studies.activate(RuntimeStudyState {
            study_id,
            persisted: original.clone(),
            resolved_chart_series: Some(series),
        });
    });
    reinitialized_sma_periods(&harness);
    let original_width = original.line_width;

    let period = cx.update(|window, cx| {
        harness.pane.update(cx, |pane, cx| {
            pane.open_study_settings_dialog(study_id, window, cx);
            pane.study_settings_dialog()
                .expect("study dialog")
                .inputs
                .get(aeris_study_sdk::BUILTIN_SMA_PERIOD_SETTING)
                .cloned()
                .expect("period input")
        })
    });
    let original_period = cx.read(|cx| {
        period
            .read(cx)
            .value()
            .parse::<i64>()
            .expect("the dialog shows the period it opened with")
    });
    cx.update(|_, cx| {
        harness.pane.update(cx, |pane, cx| {
            pane.set_study_settings_line_width(4, cx);
            assert_eq!(
                pane.studies.active()[0].persisted.line_width,
                4,
                "a style edit applies at once"
            );
        });
    });
    assert!(
        reinitialized_sma_periods(&harness).is_empty(),
        "a style edit never reinitializes the study"
    );

    cx.update(|window, cx| {
        period.update(cx, |input, cx| input.replace_all("30", window, cx));
    });
    assert_eq!(reinitialized_sma_periods(&harness), vec![30]);
    cx.update(|window, cx| {
        period.update(cx, |input, cx| input.replace_all("3", window, cx));
        period.update(cx, |input, cx| input.replace_all("40", window, cx));
    });
    assert!(
        reinitialized_sma_periods(&harness).is_empty(),
        "edits made while a reinitialization is in flight wait for it"
    );
    complete_study_reinitialization(&harness, study_id, cx);
    assert_eq!(
        reinitialized_sma_periods(&harness),
        vec![40],
        "only the newest waiting edit is sent"
    );
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(persisted_sma_period(pane), Some(30));
        assert!(
            pane.study_settings_dialog().is_some(),
            "a preview keeps the dialog open"
        );
    });

    cx.update(|_, cx| {
        harness
            .pane
            .update(cx, WorkspaceSurface::close_study_settings_dialog);
    });
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(pane.study_settings_dialog().is_none());
        assert_eq!(
            pane.studies.active()[0].persisted.line_width,
            original_width
        );
    });
    complete_study_reinitialization(&harness, study_id, cx);
    assert_eq!(
        reinitialized_sma_periods(&harness),
        vec![original_period],
        "Cancel restores the settings the dialog opened with"
    );
    complete_study_reinitialization(&harness, study_id, cx);
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.studies.active()[0].persisted, original);
        assert!(!pane.studies.has_reinitializations());
    });
}

#[gpui::test]
fn pane_harness_local_study_removal_failure_renders_one_notice(cx: &mut TestAppContext) {
    let (harness, cx) = PaneHarness::open(cx);
    let study_id = StudyInstanceId::try_from_u64(3).expect("study identity");
    for _ in 0..2 {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::StudyRemovalFailed { study_id },
            ))
            .expect("study removal failure enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Study unavailable");
    });
}

#[gpui::test]
fn pane_harness_retired_study_removal_cannot_delete_current_study(cx: &mut TestAppContext) {
    use aeris_market_runtime::{MarketConsumerId, MarketGenerationId, MarketStudyRemoved};
    use std::num::NonZeroU64;

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let study_id = StudyInstanceId::try_from_u64(3).expect("study identity");
    harness.pane.update(cx, |pane, _| {
        pane.studies.activate(RuntimeStudyState {
            study_id,
            persisted: workspace_surface::legacy_runtime_study(3, ChartIndicator::Sma, true)
                .expect("study fixture"),
            resolved_chart_series: None,
        });
    });
    let events = FixtureMarketEvents::default();
    let removed = |generation| {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(events.study_removed(
                MarketStudyRemoved {
                    consumer_id: MarketConsumerId(NonZeroU64::MIN),
                    generation: Some(MarketGenerationId(
                        NonZeroU64::new(generation).expect("generation"),
                    )),
                    study_ids: vec![study_id],
                },
            )),
        )
    };
    harness
        .publications
        .send(removed(1))
        .expect("retired removal");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.studies.active().len(),
            1,
            "retired removal cannot mutate pane"
        );
    });
    harness
        .publications
        .send(removed(sequence))
        .expect("committed removal");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert!(
            pane.studies.active().is_empty(),
            "current removal is applied"
        );
    });
}

#[gpui::test]
fn pane_harness_instrument_switch_covers_original_through_marker(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let (original, mut replacement) = cx.read(|cx| {
        let pane = harness.pane.read(cx);
        (
            pane.chart.as_ref().expect("original chart").entity_id(),
            pane.product.clone().expect("original instrument"),
        )
    });
    replacement.instrument_id = "different-instrument".into();
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_installed_instrument(&replacement, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("instrument selection");
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(
            pane.showing_superseded_series(),
            "pending instrument switch"
        );
        assert_eq!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(
            pane.showing_superseded_series(),
            "committed instrument switch"
        );
        assert_eq!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
        assert_eq!(pane.product.as_ref(), Some(&replacement));
    });
}

fn check_switch_cover_through_ready(
    interval_only: bool,
    snapshot_first: bool,
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let mut replacement = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .product
            .clone()
            .expect("original instrument")
    });
    let replacement_interval = if interval_only {
        ChartInterval::Minute5
    } else {
        replacement.instrument_id = "hyperliquid:perp:ETH".into();
        replacement.provider_symbol = "ETH".into();
        replacement.display_symbol = "ETH-USDC".into();
        ChartInterval::Minute1
    };
    harness.pane.update(cx, |pane, cx| {
        if interval_only {
            assert!(pane.select_interval(replacement_interval, cx));
        } else {
            assert!(pane.select_installed_instrument(&replacement, cx));
        }
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("replacement selection");
    let mut descriptor = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .descriptor
            .clone()
            .expect("fixture descriptor")
    });
    descriptor.instrument = replacement.clone();
    descriptor.display_symbol = replacement.display_symbol.clone();
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::BoundDescriptor {
                generation: sequence,
                descriptor: Box::new(descriptor),
                provider_display_name: "Hyperliquid".into(),
            },
        ))
        .expect("replacement descriptor");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("selection commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 2);
        assert!(pane.showing_superseded_series());
    });

    let covering = harness.fixture_snapshot(&replacement, replacement_interval, sequence, 13);
    let ready = harness.fixture_readiness(
        sequence,
        aeris_contracts::SeriesLoadState::Live,
        "replacement series ready",
    );
    let events = if snapshot_first {
        [covering, ready]
    } else {
        [ready, covering]
    };
    for (step, event) in events.into_iter().enumerate() {
        harness.publications.send(event).expect("replacement event");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
            assert_eq!(
                pane.showing_superseded_series(),
                step == 0,
                "cover requires both readiness and covering bars; interval_only={interval_only}, snapshot_first={snapshot_first}, step={step}"
            );
            if step == 1 {
                assert_eq!(pane.chart_state, ChartState::Ready);
            }
        });
    }
}

#[gpui::test]
fn pane_harness_instrument_cover_waits_for_snapshot_after_ready(cx: &mut TestAppContext) {
    check_switch_cover_through_ready(false, false, cx);
}

#[gpui::test]
fn pane_harness_instrument_cover_waits_for_ready_after_snapshot(cx: &mut TestAppContext) {
    check_switch_cover_through_ready(false, true, cx);
}

#[gpui::test]
fn pane_harness_interval_cover_waits_for_snapshot_after_ready(cx: &mut TestAppContext) {
    check_switch_cover_through_ready(true, false, cx);
}

#[gpui::test]
fn pane_harness_interval_cover_waits_for_ready_after_snapshot(cx: &mut TestAppContext) {
    check_switch_cover_through_ready(true, true, cx);
}

fn assert_chart_presentation(
    pane: &WorkspaceSurface,
    expected: &ChartAppearanceSettings,
    cx: &App,
) {
    let chart = pane.chart.as_ref().expect("chart").read(cx);
    assert_eq!(chart.chart_type(), ChartType::HollowCandles);
    assert_eq!(&chart.appearance_settings(), expected);
    assert_eq!(
        chart
            .price_axis_menu_state(0, false)
            .expect("price axis")
            .mode,
        1
    );
}

#[gpui::test]
fn pane_harness_interval_switch_keeps_the_chosen_separate_pane_height(cx: &mut TestAppContext) {
    use aeris_chart_integration::{
        ChartPaneContent, ChartPaneHeight, VolumePlacement, VolumeSettings,
    };

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let chosen = vec![ChartPaneHeight {
        content: ChartPaneContent::Volume,
        share: 0.08,
    }];
    let original = harness.pane.update(cx, |pane, cx| {
        assert!(pane.add_indicator(ChartIndicator::Volume, cx));
        let chart = pane.chart.as_ref().expect("original chart").clone();
        chart.update(cx, |chart, _| {
            assert!(chart.set_volume_settings(VolumeSettings {
                placement: VolumePlacement::OwnPane,
                ..VolumeSettings::default()
            }));
            chart.restore_pane_heights(&chosen);
        });
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
        chart.entity_id()
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("interval selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("selection commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let product = cx.read(|cx| harness.pane.read(cx).product.clone().expect("product"));
    let snapshot = harness.fixture_snapshot(&product, ChartInterval::Minute5, sequence, 13);
    harness
        .publications
        .send(snapshot)
        .expect("covering snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let chart = pane.chart.as_ref().expect("replacement chart");
        assert_ne!(
            chart.entity_id(),
            original,
            "the interval switch rebuilt the chart"
        );
        assert_eq!(chart.read(cx).pane_heights(), chosen);
        let saved = pane.workspace_chart_state(cx).expect("saved presentation");
        assert_eq!(saved.pane_heights.len(), 1);
        assert_eq!(saved.pane_heights[0].content, "volume");
        assert_eq!(saved.pane_heights[0].share_basis_points, 800);
    });
}

#[gpui::test]
fn pane_harness_symbol_switch_retains_style_colors_and_scale_mode(cx: &mut TestAppContext) {
    use prost::Message as _;

    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let mut replacement = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .product
            .clone()
            .expect("original instrument")
    });
    replacement.instrument_id = "hyperliquid:perp:ETH".into();
    replacement.provider_symbol = "ETH".into();
    replacement.display_symbol = "ETH-USDC".into();
    let appearance = harness.pane.update(cx, |pane, cx| {
        pane.set_chart_type(ChartType::HollowCandles, cx);
        let chart = pane.chart.as_ref().expect("original chart").clone();
        let mut appearance = chart.read(cx).appearance_settings();
        appearance.grid_visible = !appearance.grid_visible;
        std::mem::swap(&mut appearance.up_color, &mut appearance.down_color);
        pane.set_chart_appearance(&appearance, cx);
        chart.update(cx, |chart, _| {
            assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::SetMode(1),));
        });
        assert!(pane.select_installed_instrument(&replacement, cx));
        appearance
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("instrument selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("selection commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let snapshot = harness.fixture_snapshot(&replacement, ChartInterval::Minute1, sequence, 13);
    harness
        .publications
        .send(snapshot)
        .expect("covering snapshot");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.read(|cx| {
        assert_chart_presentation(harness.pane.read(cx), &appearance, cx);
    });
    let saved = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .workspace_chart_state(cx)
            .expect("saved presentation")
    });
    let decoded = WorkspaceChartState::decode(saved.encode_to_vec().as_slice())
        .expect("pane persistence decodes");
    assert_eq!(decoded, saved, "pane persistence preserves all fields");
    let (mut reopened, cx) = PaneHarness::open_with_restore(
        WorkspaceSurfaceRestore {
            chart: Some(decoded),
            ..WorkspaceSurfaceRestore::default()
        },
        cx,
    );
    reopened.publish_snapshot(12, cx);
    cx.read(|cx| {
        assert_chart_presentation(reopened.pane.read(cx), &appearance, cx);
    });
}

#[gpui::test]
fn pane_harness_retired_initial_descriptor_does_not_rewrite_selected_terms(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let mut stale = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .product
            .clone()
            .expect("initial instrument")
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("interval selection");
    stale.price_increment = Some(999);
    for marker_first in [false, true] {
        if marker_first {
            harness
                .publications
                .send(PaneProjectionEvent::LocalCommandOutcome(
                    LocalCommandOutcome::EngineSelection {
                        sequence,
                        succeeded: true,
                    },
                ))
                .expect("marker enters mailbox");
        }
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                aeris_desktop::market_worker::LocalCommandOutcome::SelectedInstrument {
                    generation: sequence.saturating_sub(1),
                    instrument: stale.clone(),
                },
            ))
            .expect("retired descriptor enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(
                pane.poll_market_worker(cx),
                if marker_first { 2 } else { 1 }
            );
            assert_ne!(
                pane.product.as_ref().and_then(|p| p.price_increment),
                Some(999)
            );
        });
    }
}

#[gpui::test]
fn pane_harness_retired_selection_failure_cannot_cancel_new_request(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let requests = harness.commands.as_ref().expect("command receiver");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let first = requests
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("first selection");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute15, cx));
    });
    let second = requests
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("replacement selection");
    assert_ne!(first, second);
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence: first,
                succeeded: false,
            },
        ))
        .expect("retired failure enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_pane_controller.pending_selection(), Some(second));
        assert_eq!(
            pane.chart_pane_controller.pending_interval(),
            Some(ChartInterval::Minute15)
        );
        assert_eq!(pane.chart_state, ChartState::Loading);
        assert_eq!(harness.window.read(cx).toaster.notices(), []);
    });
}

#[gpui::test]
fn pane_harness_selection_outcome_uses_exact_pending_controller_generation(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let requests = harness.commands.as_ref().expect("command receiver");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = requests
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("pending selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("selection outcome enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_pane_controller.pending_selection(), None);
        assert_eq!(pane.interval, ChartInterval::Minute5);
    });
}

#[gpui::test]
fn pane_harness_failed_pending_instrument_keeps_previous_identity(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let (old_chart, product) = cx.read(|cx| {
        let pane = harness.pane.read(cx);
        (
            pane.chart.clone().expect("original chart"),
            pane.product.clone().expect("instrument"),
        )
    });
    let mut replacement = product.clone();
    replacement.instrument_id = "different-instrument".into();
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_installed_instrument(&replacement, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("command receiver")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("instrument selection");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: false,
            },
        ))
        .expect("failure enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.product.as_ref(), Some(&product));
        assert_eq!(
            pane.chart.as_ref().map(Entity::entity_id),
            Some(old_chart.entity_id())
        );
        assert_eq!(pane.interval, ChartInterval::Minute1);
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Market selection unavailable");
    });
}

#[gpui::test]
fn pane_harness_invalid_restored_chart_zone_renders_one_notice_on_repeat(cx: &mut TestAppContext) {
    let restore = WorkspaceSurfaceRestore {
        chart: Some(WorkspaceChartState {
            time_zone: "Invalid/Restored_Zone".into(),
            ..WorkspaceChartState::default()
        }),
        ..WorkspaceSurfaceRestore::default()
    };
    let (mut harness, cx) = PaneHarness::open_with_restore(restore, cx);
    harness.publish_snapshot(12, cx);
    // A replacement covering snapshot recreates the chart and re-applies the surface zone.
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(
            toaster.notices()[0].title,
            "Chart settings could not be restored"
        );
        assert_eq!(
            toaster.notices()[0].key.scope,
            harness.pane.entity_id().as_u64()
        );
        assert_eq!(
            toaster.notices()[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
        assert_eq!(
            toaster.notices()[0].severity,
            notification_center::NoticeSeverity::Error
        );
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("Invalid/Restored_Zone")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "restore failure rendered a card"
    );
}

#[gpui::test]
fn pane_harness_invalid_saved_presentation_reports_one_notice_for_multiple_fields(
    cx: &mut TestAppContext,
) {
    let private_zone = "private-credential-sentinel/Zone";
    let restore = WorkspaceSurfaceRestore {
        chart: Some(WorkspaceChartState {
            time_zone: private_zone.into(),
            extend_order_lines_left: Some(false),
            appearance: Some(WorkspaceChartAppearanceState {
                grid_style: u32::MAX,
                ..WorkspaceChartAppearanceState::default()
            }),
            order_flow: Some(WorkspaceOrderFlowSettingsState {
                display_mode: u32::MAX,
                ..WorkspaceOrderFlowSettingsState::default()
            }),
            ..WorkspaceChartState::default()
        }),
        ..WorkspaceSurfaceRestore::default()
    };
    let (mut harness, cx) = PaneHarness::open_with_restore(restore, cx);
    harness.publish_snapshot(12, cx);
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            !harness
                .pane
                .read(cx)
                .chart
                .as_ref()
                .expect("snapshot chart")
                .read(cx)
                .order_lines_extend_left(),
            "invalid zone must not skip the other surface setter"
        );
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(
            notices.len(),
            1,
            "all rejected saved fields share one notice"
        );
        assert_eq!(notices[0].title, "Chart settings could not be restored");
        assert!(!notices[0].detail.contains(private_zone));
        assert_eq!(
            notices[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
    });
    harness.pane.update(cx, |pane, cx| {
        let mut appearance = pane
            .chart
            .as_ref()
            .expect("snapshot chart")
            .read(cx)
            .appearance_settings();
        appearance.grid_visible = !appearance.grid_visible;
        pane.set_chart_appearance(&appearance, cx);
    });
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            harness.window.read(cx).toaster.notices().len(),
            1,
            "the untouched order-flow rejection keeps the notice"
        );
    });
    harness.pane.update(cx, |pane, cx| {
        pane.set_chart_order_flow_settings(
            OrderFlowSettings {
                show_cumulative_delta: true,
                ..OrderFlowSettings::default()
            },
            cx,
        );
    });
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            harness.window.read(cx).toaster.notices().len(),
            1,
            "the invalid zone still keeps the notice"
        );
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.set_chart_time_zone("Etc/UTC", cx));
    });
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_startup_chart_setter_failure_dedupes_and_snapshot_recovery_clears(
    cx: &mut TestAppContext,
) {
    let private_zone = "private-credential-sentinel/Zone";
    let restore = WorkspaceSurfaceRestore {
        chart: Some(WorkspaceChartState {
            time_zone: private_zone.into(),
            extend_order_lines_left: Some(false),
            ..WorkspaceChartState::default()
        }),
        ..WorkspaceSurfaceRestore::default()
    };
    let (mut harness, cx) = PaneHarness::open_with_startup(restore, false, true, cx);
    cx.read(|cx| {
        assert_eq!(harness.pane.read(cx).chart_state, ChartState::Loading);
        assert!(harness.pane.read(cx).chart.is_none());
    });
    harness.publish_snapshot(12, cx);
    cx.read(|cx| {
        assert!(
            !harness
                .pane
                .read(cx)
                .chart
                .as_ref()
                .expect("startup chart")
                .read(cx)
                .order_lines_extend_left(),
            "a failed time zone must not skip the order-line setting"
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(harness.window.read(cx).toaster.notices().len(), 1);
    });
    harness.pane.update(cx, |pane, cx| {
        pane.apply_initial_chart_settings(cx);
    });
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            !harness
                .pane
                .read(cx)
                .chart
                .as_ref()
                .expect("replacement chart")
                .read(cx)
                .order_lines_extend_left(),
            "replacement retains the order-line setting after the failed time zone"
        );
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(
            toaster.notices()[0].title,
            "Chart settings could not be restored"
        );
        assert!(!toaster.notices()[0].detail.contains(private_zone));
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "startup failure rendered"
    );
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.set_chart_time_zone("Etc/UTC", cx));
    });
    harness.reinstall_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_secret_bearing_restore_failure_redacts_notice_and_diagnostic(
    cx: &mut TestAppContext,
) {
    const CHILD_FLAG: &str = "AERIS_NOTICE_DIAGNOSTIC_CHILD";
    if std::env::var_os(CHILD_FLAG).is_none() {
        // The production sink is process-wide. Isolate this one GPUI test so
        // concurrent notice tests cannot contribute an identical title line.
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "desktop::pane_harness::pane_harness_secret_bearing_restore_failure_redacts_notice_and_diagnostic",
                "--nocapture",
            ])
            .env(CHILD_FLAG, "1")
            .output()
            .expect("isolated diagnostic test starts");
        assert!(output.status.success(), "isolated diagnostic test failed");
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "aeris-notice-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("test log directory");
    let log_path = root.join("aeris.log");
    let guard = aeris_observability::install_log_sink(&log_path).expect("test diagnostic sink");
    let secret = format!("private-credential-sentinel-{}", std::process::id());
    let restore = WorkspaceSurfaceRestore {
        chart: Some(WorkspaceChartState {
            time_zone: format!("{secret}/Zone"),
            ..WorkspaceChartState::default()
        }),
        ..WorkspaceSurfaceRestore::default()
    };
    let (mut harness, cx) = PaneHarness::open_with_startup(restore, false, true, cx);
    harness.publish_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Chart settings could not be restored");
        assert!(!notices[0].detail.contains(&secret));
        assert_eq!(
            notices[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
    });
    drop(guard); // Drains the real bounded diagnostics sink before inspecting its output.
    let log = std::fs::read_to_string(&log_path).expect("captured diagnostic");
    let notice_lines = log
        .lines()
        .filter(|line| line.contains("desktop notice: Chart settings could not be restored"))
        .collect::<Vec<_>>();
    assert_eq!(notice_lines.len(), 1);
    assert!(notice_lines[0].contains("Chart settings could not be restored"));
    assert!(!notice_lines[0].contains(&secret));
    assert!(!log.contains(&secret));
    std::fs::remove_dir_all(root).expect("test log cleanup");
}

#[gpui::test]
fn pane_harness_full_symbol_search_queue_reports_once_and_a_retry_clears(cx: &mut TestAppContext) {
    let (harness, cx) = PaneHarness::open(cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    while commands.try_recv().is_ok() {}
    harness.pane.update(cx, |pane, _| {
        for _ in 0..8 {
            pane.market_worker
                .try_set_order_book_detail(OrderBookDetail::Depth)
                .expect("fill bounded command queue");
        }
    });
    let credential = "private-credential-sentinel";
    cx.update(|window, cx| {
        let input = harness
            .pane
            .read(cx)
            .symbol_input
            .clone()
            .expect("symbol input");
        input.update(cx, |input, cx| input.set_value(credential, window, cx));
        harness.pane.update(cx, |pane, cx| {
            assert!(!pane.search_symbol_input(cx));
            assert!(!pane.search_symbol_input(cx));
        });
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(
            harness.pane.read(cx).symbol_selector_status(),
            "No markets found",
            "a full command queue fails the search instead of queueing it; only the notice reports it"
        );
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Symbol search unavailable");
        assert_eq!(
            toaster.notices()[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
        assert_eq!(
            toaster.notices()[0].severity,
            notification_center::NoticeSeverity::Error
        );
        assert!(!toaster.notices()[0].detail.contains(credential));
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "search failure rendered a card"
    );

    while commands.try_recv().is_ok() {}
    harness.pane.update(cx, |pane, cx| {
        pane.poll_market_worker(cx);
        assert!(
            commands.try_recv().is_err(),
            "a failed search is not resent behind the trader's back"
        );
        pane.symbol_browser
            .set_scope(SymbolSearchScope::Provider(pane.provider.id()));
        assert!(pane.search_symbol_input(cx), "retrying sends the search");
    });
    let MarketWorkerCommand::ProviderSearch(request) =
        commands.try_recv().expect("retried search dispatched")
    else {
        panic!("expected provider search");
    };
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(harness.market_events.catalog(
                aeris_market_runtime::MarketRuntimeEvent::ProviderInstrumentSearchResult(
                    ProviderInstrumentSearchResult {
                        provider: request.provider,
                        search_generation: request.search_generation,
                        ..ProviderInstrumentSearchResult::default()
                    },
                ),
            )),
        ))
        .expect("fixture search completion enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(
            harness.pane.read(cx).symbol_selector_status(),
            "No markets found"
        );
        assert_eq!(toaster.notices(), []);
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            0
        );
    });
}

#[gpui::test]
fn pane_harness_rithmic_product_opens_its_front_month_from_the_empty_query_listing(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    let searches = || {
        std::iter::from_fn(|| commands.try_recv().ok())
            .filter_map(|command| match command {
                MarketWorkerCommand::ProviderSearch(request) => Some(Ok(request)),
                MarketWorkerCommand::SelectCatalog(request) => Some(Err(request)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let query = |cx: &mut VisualTestContext| {
        cx.read(|cx| {
            harness
                .pane
                .read(cx)
                .symbol_input
                .as_ref()
                .map(|input| input.read(cx).value().to_string())
        })
    };
    cx.update(|window, cx| {
        harness.pane.update(cx, |pane, cx| {
            pane.choose_symbol_scope(SymbolSearchScope::Provider("rithmic"), window, cx);
        });
    });
    let listing = searches();
    assert_eq!(listing.len(), 1, "provider switch sends one listing search");
    let listing = listing[0].as_ref().expect("a listing search");
    assert_eq!(listing.provider, "rithmic");
    assert_eq!(listing.query, "", "an empty query lists products");
    assert_eq!(query(cx).as_deref(), Some(""));
    complete_fixture_search(
        &harness,
        listing,
        ProviderInstrumentSummary {
            kind: ProviderInstrumentKind::Product,
            symbol: "NQ".into(),
            display_symbol: "NQ".into(),
            exchange: "CME".into(),
            name: Some("E-mini Nasdaq-100".into()),
            product_code: Some("NQ".into()),
            asset_class: Some("futures".into()),
            ..ProviderInstrumentSummary::default()
        },
    );
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        let entries = pane.instrument_entries(cx);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "NQ");
        assert_eq!(
            pane.symbol_selector_status(),
            "1 product · choose one for its front month"
        );
        pane.chrome.set_selection(0);
        assert!(
            pane.submit_symbol_input(cx),
            "Enter on a product opens its front month"
        );
    });
    let contracts = searches();
    assert_eq!(
        contracts.len(),
        1,
        "opening searches the product's contracts before selecting"
    );
    let contracts = contracts[0].as_ref().expect("a contract search");
    assert_eq!(contracts.query, "NQ");
    assert_eq!(
        query(cx).as_deref(),
        Some(""),
        "opening leaves the query empty"
    );
    complete_fixture_search_with(
        &harness,
        contracts,
        vec![
            nq_contract("NQH7", "CME", "20270319"),
            nq_contract("NQZ6", "CME", "20261218"),
            nq_contract("NQZ6", "CME-Delayed", "20261218"),
        ],
    );
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let selected = searches();
    assert_eq!(selected.len(), 1, "the front month is selected at once");
    let selected = selected[0].as_ref().expect_err("a chart selection");
    assert_eq!(
        (selected.symbol.as_str(), selected.exchange.as_str()),
        ("NQZ6", "CME")
    );
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

fn nq_contract(symbol: &str, exchange: &str, expiration: &str) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: symbol.into(),
        display_symbol: symbol.into(),
        exchange: exchange.into(),
        product_code: Some("NQ".into()),
        expiration_date: Some(expiration.into()),
        ..ProviderInstrumentSummary::default()
    }
}

/// Scopes the pane's symbol search to its chart provider and starts one search there.
fn chart_provider_search(pane: &mut WorkspaceSurface, query: &str) -> SymbolSearchRequest {
    pane.symbol_browser
        .set_scope(SymbolSearchScope::Provider(pane.provider.id()));
    let mut requests = pane
        .symbol_browser
        .begin_search(query)
        .expect("valid query");
    assert_eq!(requests.len(), 1, "a provider scope searches one provider");
    requests.remove(0)
}

#[gpui::test]
fn pane_harness_all_providers_search_shows_each_answer_as_it_arrives(cx: &mut TestAppContext) {
    let (harness, cx) = PaneHarness::open(cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    while commands.try_recv().is_ok() {}
    let searches = || {
        std::iter::from_fn(|| commands.try_recv().ok())
            .filter_map(|command| match command {
                MarketWorkerCommand::ProviderSearch(request) => Some(request),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    cx.update(|window, cx| {
        harness.pane.update(cx, |pane, cx| {
            pane.choose_symbol_scope(SymbolSearchScope::All, window, cx);
            pane.search_symbol_query("BT", cx);
        });
    });
    let sent = searches();
    let latest: Vec<_> = sent.iter().filter(|search| search.query == "BT").collect();
    assert_eq!(
        latest
            .iter()
            .map(|search| search.provider.as_str())
            .collect::<Vec<_>>(),
        ["rithmic", "hyperliquid"],
        "one query reaches every provider at once"
    );
    assert!(latest.iter().all(|search| search.exchange.is_none()));
    complete_fixture_search(
        &harness,
        latest[1],
        ProviderInstrumentSummary {
            symbol: "BTC".into(),
            display_symbol: "BTC-USDC".into(),
            exchange: "Hyperliquid".into(),
            ..ProviderInstrumentSummary::default()
        },
    );
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        let entries = pane.instrument_entries(cx);
        assert_eq!(
            entries.len(),
            1,
            "no provider headers, only the answer so far"
        );
        assert_eq!(entries[0].label, "BTC-USDC");
        assert_eq!(pane.symbol_selector_status(), "Searching provider markets");
    });

    let exchange = ExchangeCode::new("Hyperliquid").expect("exchange code");
    cx.update(|window, cx| {
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(
                pane.symbol_browser.exchanges(ProviderId("hyperliquid")),
                [exchange],
                "the exchange is learned from the provider's results"
            );
            pane.choose_symbol_scope(
                SymbolSearchScope::Exchange {
                    provider: "hyperliquid",
                    exchange,
                },
                window,
                cx,
            );
            assert_eq!(
                pane.chart_chrome.symbol_search_scope,
                pane.symbol_browser.scope()
            );
        });
    });
    let scoped = searches();
    assert_eq!(
        scoped.len(),
        1,
        "an exchange scope searches its provider only"
    );
    assert_eq!(scoped[0].provider, "hyperliquid");
    assert_eq!(scoped[0].exchange.as_deref(), Some("Hyperliquid"));
}

fn complete_fixture_search(
    harness: &PaneHarness,
    request: &SearchProviderInstruments,
    instrument: ProviderInstrumentSummary,
) {
    complete_fixture_search_with(harness, request, vec![instrument]);
}

fn complete_fixture_search_with(
    harness: &PaneHarness,
    request: &SearchProviderInstruments,
    instruments: Vec<ProviderInstrumentSummary>,
) {
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(harness.market_events.catalog(
                aeris_market_runtime::MarketRuntimeEvent::ProviderInstrumentSearchResult(
                    ProviderInstrumentSearchResult {
                        provider: request.provider.clone(),
                        search_generation: request.search_generation,
                        instruments,
                        ..ProviderInstrumentSearchResult::default()
                    },
                ),
            )),
        ))
        .expect("fixture search completion enters mailbox");
}

#[gpui::test]
fn pane_harness_verified_terms_replace_the_provisional_chart_descriptor(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let charted = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .product
            .clone()
            .expect("initial instrument")
    });
    let mut other = charted.clone();
    other.instrument_id.push_str(":other");
    other.price_increment = Some(7);
    let mut verified = charted;
    verified.price_increment = Some(25);
    for instrument in [other, verified.clone()] {
        harness
            .publications
            .send(PaneProjectionEvent::RuntimeProjection(
                aeris_desktop::market_worker::RuntimeProjection::event(
                    harness.market_events.catalog(
                        aeris_market_runtime::MarketRuntimeEvent::ProviderInstrumentSelection(
                            aeris_market_runtime::MarketProviderInstrumentSelection {
                                consumer_id: aeris_market_runtime::MarketConsumerId(
                                    std::num::NonZeroU64::MIN,
                                ),
                                command_generation: 1,
                                instrument,
                            },
                        ),
                    ),
                ),
            ))
            .expect("verified terms enter mailbox");
    }
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 2);
        assert_eq!(
            pane.product.as_ref(),
            Some(&verified),
            "only the charted instrument's terms replace its descriptor"
        );
        let descriptor = pane.descriptor.as_ref().expect("updated descriptor");
        assert_eq!(descriptor.instrument, verified);
        assert_eq!(
            descriptor.asset_class,
            aeris_instruments::InstrumentClass::Future
        );
        assert_eq!(descriptor.currency.as_deref(), Some("USDC"));
    });
}

#[gpui::test]
fn pane_harness_rejected_symbol_selection_redacts_detail_and_commit_clears(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    while commands.try_recv().is_ok() {}
    let instrument = local_state::fixture_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("fixture product");
    let provider = harness.pane.update(cx, |pane, _| {
        let request = chart_provider_search(pane, "BTC");
        assert!(pane.symbol_browser.apply_results(
            request.provider,
            request.request_id,
            vec![ProviderInstrumentSummary {
                symbol: "BTC".into(),
                display_symbol: "BTC".into(),
                exchange: "TEST".into(),
                ..ProviderInstrumentSummary::default()
            }],
        ));
        assert_eq!(
            pane.symbol_selector_status(),
            "1 matching markets · search to find more"
        );
        request.provider
    });
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            assert!(pane.select_instrument(
                InstrumentMenuSelection::SearchResult { provider, index: 0 },
                SymbolSelectionTarget::Chart,
                cx,
            ));
            assert_eq!(pane.symbol_selector_status(), "Selecting market");
        });
        let MarketWorkerCommand::SelectCatalog(request) =
            commands.try_recv().expect("selection dispatched")
        else {
            panic!("expected catalog selection");
        };
        let failure = LocalCommandOutcome::CatalogSwitch {
            sequence: request.sequence,
            browser_generation: request.browser_generation,
            result: Err("private-credential-sentinel".into()),
        };
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(failure))
            .expect("fixture failure enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Market selection unavailable");
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("private-credential-sentinel")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "selection failure rendered"
    );
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_instrument(
            InstrumentMenuSelection::SearchResult { provider, index: 0 },
            SymbolSelectionTarget::Chart,
            cx,
        ));
    });
    let MarketWorkerCommand::SelectCatalog(request) = commands
        .try_recv()
        .expect("replacement selection dispatched")
    else {
        panic!("expected catalog selection");
    };
    let success = LocalCommandOutcome::CatalogSwitch {
        sequence: request.sequence,
        browser_generation: request.browser_generation,
        result: Ok((instrument, ChartInterval::Minute1)),
    };
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(success))
        .expect("fixture commit enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_disconnected_order_book_request_renders_one_notice(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    drop(harness.commands.take());
    harness.pane.update(cx, |pane, cx| {
        pane.set_side_panel_visible(SidePanel::OrderBook, true, cx);
        pane.set_side_panel_visible(SidePanel::OrderBook, false, cx);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let window = harness.window.read(cx);
        let notices = window.toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Order book unavailable");
        assert_eq!(notices[0].key.scope, harness.pane.entity_id().as_u64());
        assert_eq!(
            notices[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
        assert_eq!(
            notices[0].severity,
            notification_center::NoticeSeverity::Error
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "failure rendered as a toast card"
    );
}

#[gpui::test]
fn pane_harness_resource_class_failure_deduplicates_and_retry_clears(cx: &mut TestAppContext) {
    let (mut harness, cx) =
        PaneHarness::open_with_restore_and_full_queue(WorkspaceSurfaceRestore::default(), true, cx);
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            pane.set_market_resource_class(ConsumerResourceClass::Background, cx);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].key.kind, "market_resource_class");
        assert_eq!(
            notices[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
        assert_eq!(
            harness
                .window
                .read(cx)
                .toaster
                .live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    while harness.commands.as_ref().unwrap().try_recv().is_ok() {}
    harness.pane.update(cx, |pane, cx| {
        pane.set_market_resource_class(ConsumerResourceClass::Foreground, cx);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
    drop(harness.commands.take());
    harness.pane.update(cx, |pane, cx| {
        pane.set_market_resource_class(ConsumerResourceClass::Detached, cx);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_price_alert_processing_failure_renders_once_and_success_clears(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    for _ in 0..2 {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::PriceAlertSync {
                    sequence: 0,
                    succeeded: false,
                },
            ))
            .expect("fixture alert failure enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Price alerts unavailable");
        assert_eq!(
            toaster.notices()[0].key.scope,
            harness.pane.entity_id().as_u64()
        );
        assert_eq!(
            toaster.notices()[0].source,
            notification_center::NoticeSource::Pane(harness.pane.entity_id().as_u64())
        );
        assert_eq!(
            toaster.notices()[0].severity,
            notification_center::NoticeSeverity::Error
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "alert failure rendered a card"
    );

    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::PriceAlertSync {
                sequence: 0,
                succeeded: true,
            },
        ))
        .expect("fixture alert success enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices(), []);
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            0
        );
    });
}

#[gpui::test]
fn pane_harness_alert_restore_failure_renders_and_processing_success_clears(
    cx: &mut TestAppContext,
) {
    let restore = WorkspaceSurfaceRestore {
        chart: Some(WorkspaceChartState {
            price_alerts: vec![WorkspacePriceAlertState {
                id: "restored-alert".into(),
                ..WorkspacePriceAlertState::default()
            }],
            ..WorkspaceChartState::default()
        }),
        ..WorkspaceSurfaceRestore::default()
    };
    let (harness, cx) = PaneHarness::open_with_restore_and_full_queue(restore, true, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Price alerts unavailable");
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "restore failure rendered"
    );
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::PriceAlertSync {
                sequence: 0,
                succeeded: true,
            },
        ))
        .expect("fixture success enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_alert_delete_queue_failure_renders_once_then_success_clears(
    cx: &mut TestAppContext,
) {
    let restore = WorkspaceSurfaceRestore {
        chart: Some(WorkspaceChartState {
            price_alerts: vec![WorkspacePriceAlertState {
                id: "restored-alert".into(),
                ..WorkspacePriceAlertState::default()
            }],
            ..WorkspaceChartState::default()
        }),
        ..WorkspaceSurfaceRestore::default()
    };
    let (harness, cx) = PaneHarness::open_with_restore(restore, cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    harness.pane.update(cx, |pane, _| {
        for _ in 0..7 {
            pane.market_worker
                .try_set_order_book_detail(OrderBookDetail::Depth)
                .expect("fill remaining command slots");
        }
    });
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            pane.delete_price_alert("restored-alert", cx);
            assert_eq!(pane.alerts.alerts().len(), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(
            toaster.notices()[0].title,
            "Price alert could not be changed"
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "delete failure rendered"
    );
    while commands.try_recv().is_ok() {}
    harness.pane.update(cx, |pane, cx| {
        pane.delete_price_alert("restored-alert", cx);
        assert_eq!(pane.alerts.alerts().len(), 0);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_price_alert_sync_ignores_retired_command_outcomes(cx: &mut TestAppContext) {
    let (harness, cx) = PaneHarness::open(cx);
    harness.pane.update(cx, |pane, _| {
        pane.market_worker
            .try_replace_price_alerts(Vec::new())
            .expect("first replacement is queued");
        pane.market_worker
            .try_replace_price_alerts(Vec::new())
            .expect("second replacement is queued");
        assert_eq!(pane.market_worker.latest_price_alert_sequence(), 2);
    });
    for (sequence, succeeded, expected_notices) in [(1, false, 0), (2, false, 1), (1, true, 1)] {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::PriceAlertSync {
                    sequence,
                    succeeded,
                },
            ))
            .expect("fixture result enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let notices = harness.window.read(cx).toaster.notices();
            assert_eq!(notices.len(), expected_notices);
            if expected_notices == 1 {
                assert_eq!(notices[0].title, "Price alerts unavailable");
            }
        });
    }
    harness.pane.update(cx, |pane, _| {
        pane.market_worker
            .try_replace_price_alerts(Vec::new())
            .expect("third replacement is queued");
        assert_eq!(pane.market_worker.latest_price_alert_sequence(), 3);
    });
    for sequence in [2, 3] {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::PriceAlertSync {
                    sequence,
                    succeeded: true,
                },
            ))
            .expect("fixture success enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                harness.window.read(cx).toaster.notices().len(),
                usize::from(sequence == 2),
            );
        });
    }
}

#[gpui::test]
fn pane_harness_unsupported_zone_renders_one_sanitized_notice_and_valid_zone_clears(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    let private_zone = "private-credential-sentinel/Zone";
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            assert!(!pane.set_chart_time_zone(private_zone, cx));
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Chart settings unavailable");
        assert!(!toaster.notices()[0].detail.contains(private_zone));
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(cx.debug_bounds("toast").is_some(), "zone failure rendered");
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.set_chart_time_zone("Etc/UTC", cx));
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_study_registration_failures_redact_and_success_clears(cx: &mut TestAppContext) {
    let (harness, cx) = PaneHarness::open(cx);
    for sequence in 1..=2 {
        harness.pane.update(cx, |pane, _| {
            pane.studies.insert_pending(
                sequence,
                PendingRuntimeStudyState {
                    persisted: workspace_surface::legacy_runtime_study(
                        sequence,
                        ChartIndicator::Sma,
                        true,
                    )
                    .expect("fixture study"),
                    resolved_chart_series: None,
                    remove_on_registration: false,
                    persist_on_registration: true,
                    blocked: false,
                },
            );
        });
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::StudyRegistration {
                    request_sequence: sequence,
                    result: Err("private-credential-sentinel".into()),
                },
            ))
            .expect("fixture study failure enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Study unavailable");
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("private-credential-sentinel")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(cx.debug_bounds("toast").is_some(), "study failure rendered");
    harness.pane.update(cx, |pane, _| {
        pane.studies.insert_pending(
            3,
            PendingRuntimeStudyState {
                persisted: workspace_surface::legacy_runtime_study(3, ChartIndicator::Sma, true)
                    .expect("fixture study"),
                resolved_chart_series: None,
                remove_on_registration: false,
                persist_on_registration: true,
                blocked: false,
            },
        );
    });
    for result in [
        Ok(StudyInstanceId::try_from_u64(1).expect("fixture identity")),
        Err("retired-registration-error".to_string()),
    ] {
        harness
            .publications
            .send(PaneProjectionEvent::LocalCommandOutcome(
                LocalCommandOutcome::StudyRegistration {
                    request_sequence: 1,
                    result,
                },
            ))
            .expect("retired study outcome enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
            assert!(pane.studies.pending().contains_key(&3));
            assert_eq!(pane.studies.active().len(), 0);
        });
    }
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::StudyRegistration {
                request_sequence: 3,
                result: Ok(StudyInstanceId::try_from_u64(3).expect("fixture identity")),
            },
        ))
        .expect("fixture registration enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

fn deferred_sma_study() -> PendingRuntimeStudyState {
    PendingRuntimeStudyState {
        persisted: workspace_surface::legacy_runtime_study(1, ChartIndicator::Sma, true)
            .expect("fixture study"),
        resolved_chart_series: None,
        remove_on_registration: false,
        persist_on_registration: false,
        blocked: false,
    }
}

#[gpui::test]
fn pane_harness_deferred_study_registers_on_a_selected_rithmic_market(cx: &mut TestAppContext) {
    let entitlement = aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .find(|descriptor| descriptor.id == "rithmic")
        .expect("Rithmic presentation")
        .selection_entitlement_id;
    let product = InstallProviderInstrument {
        provider: "rithmic".into(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "instrument:rithmic:CME:MNQZ6".into(),
        provider_symbol: "MNQZ6".into(),
        display_symbol: "MNQZ6".into(),
        venue_id: "CME".into(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: entitlement.into(),
        ..InstallProviderInstrument::default()
    };
    let (mut harness, cx) = PaneHarness::open_with_product(
        WorkspaceSurfaceRestore::default(),
        false,
        product,
        ChartInterval::Minute1,
        cx,
    );
    harness.publish_snapshot(12, cx);
    let requests = harness.commands.take().expect("command receiver");
    requests.try_iter().for_each(drop);
    harness.pane.update(cx, |pane, cx| {
        pane.studies.push_deferred(deferred_sma_study());
        pane.poll_market_worker(cx);
        assert_eq!(pane.studies.deferred(), [] as [PendingRuntimeStudyState; 0]);
        assert_eq!(pane.studies.pending().len(), 1);
    });
    assert!(
        requests
            .try_iter()
            .any(|command| matches!(command, MarketWorkerCommand::RegisterStudy(_)))
    );
}

#[gpui::test]
fn pane_harness_unqueueable_deferred_study_reports_once_instead_of_every_poll(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let requests = harness.commands.take().expect("command receiver");
    requests.try_iter().for_each(drop);
    let study_posts = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    cx.update(|_, cx| {
        let study_posts = study_posts.clone();
        cx.subscribe(&harness.pane, move |_, event: &PaneNoticeEvent, _| {
            if let PaneNoticeEvent::Post(notice) = event
                && notice.key.kind == "study"
            {
                study_posts.set(study_posts.get() + 1);
            }
        })
        .detach();
    });
    harness.pane.update(cx, |pane, cx| {
        pane.product
            .as_mut()
            .expect("product")
            .entitlement_id
            .clear();
        pane.studies.push_deferred(deferred_sma_study());
        for _ in 0..3 {
            pane.poll_market_worker(cx);
        }
        assert_eq!(pane.studies.deferred().len(), 1);
        assert!(pane.studies.pending().is_empty());
    });
    cx.run_until_parked();
    assert_eq!(study_posts.get(), 1);
    assert!(
        requests
            .try_iter()
            .all(|command| !matches!(command, MarketWorkerCommand::RegisterStudy(_)))
    );
}

#[gpui::test]
fn pane_harness_removed_volume_settings_renders_once_and_readd_clears(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    for _ in 0..2 {
        cx.update(|window, cx| {
            harness.pane.update(cx, |pane, cx| {
                pane.open_volume_dialog(window, cx);
                assert!(pane.volume_dialog().is_none());
            });
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Study unavailable");
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "settings failure rendered"
    );
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.add_indicator(ChartIndicator::Volume, cx));
    });
    cx.update(|window, cx| {
        harness.pane.update(cx, |pane, cx| {
            pane.open_volume_dialog(window, cx);
            assert!(pane.volume_dialog().is_some());
        });
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_fenced_provider_state_rejects_wrong_provider_and_retired_selection(
    cx: &mut TestAppContext,
) {
    use aeris_market_runtime::fence::{ConsumerTag, EpochSequence, Fenced};
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let provider = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .product
            .as_ref()
            .expect("product")
            .provider
            .clone()
    });
    let mut epochs = EpochSequence::<ConsumerTag>::default();
    let connection = |provider: String, generation, state, epoch| {
        PaneProjectionEvent::RuntimeProjection(aeris_desktop::market_worker::RuntimeProjection {
            event: Fenced {
                epoch,
                value: aeris_market_runtime::MarketRuntimeEvent::ProviderState(
                    aeris_contracts::ProviderState {
                        provider,
                        state,
                        generation: 1,
                        detail: Some("private-credential-sentinel".into()),
                        transport_rtt_nanos: Some(12_500_000),
                    },
                ),
            },
            selection_generation: Some(generation),
            received_unix_nanos: None,
            recovery_request_id: None,
        })
    };
    harness
        .publications
        .send(connection(
            "wrong-provider".into(),
            1,
            aeris_contracts::ProviderConnectionState::Failed,
            epochs.next(),
        ))
        .expect("wrong provider");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(pane.chart_failure, Some(ChartFailure::WorkerStopped));
    });
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(Fenced {
                epoch: epochs.next(),
                value: aeris_market_runtime::MarketRuntimeEvent::ProviderState(
                    aeris_contracts::ProviderState {
                        provider: provider.clone(),
                        state: aeris_contracts::ProviderConnectionState::Failed,
                        generation: 1,
                        detail: None,
                        transport_rtt_nanos: None,
                    },
                ),
            }),
        ))
        .expect("missing selection metadata");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(pane.chart_failure, Some(ChartFailure::WorkerStopped));
    });
    harness
        .publications
        .send(connection(
            provider.clone(),
            0,
            aeris_contracts::ProviderConnectionState::Failed,
            epochs.next(),
        ))
        .expect("retired provider state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(pane.chart_failure, Some(ChartFailure::WorkerStopped));
    });
    harness
        .publications
        .send(connection(
            provider,
            1,
            aeris_contracts::ProviderConnectionState::Online,
            epochs.next(),
        ))
        .expect("current provider state");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.connection_state, Some(FeedConnectionState::Streaming));
        assert_eq!(pane.provider_transport_rtt_nanos, Some(12_500_000));
    });
}

#[gpui::test]
fn pane_harness_connection_rejects_pending_and_retired_selection(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    let sequence = harness
        .commands
        .as_ref()
        .expect("commands")
        .try_iter()
        .find_map(|command| match command {
            MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
            _ => None,
        })
        .expect("selection");
    let connection = |generation, state, rtt| PaneProjectionEvent::SessionStatus {
        generation,
        status: LocalSessionStatus::Connection {
            state,
            transport_rtt_nanos: rtt,
        },
    };
    let previous = cx.read(|cx| {
        let pane = harness.pane.read(cx);
        (pane.connection_state, pane.provider_transport_rtt_nanos)
    });
    harness
        .publications
        .send(connection(sequence, FeedConnectionState::Stopped, None))
        .expect("pending connection");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            (pane.connection_state, pane.provider_transport_rtt_nanos),
            previous
        );
        assert_ne!(pane.chart_failure, Some(ChartFailure::WorkerStopped));
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::EngineSelection {
                sequence,
                succeeded: true,
            },
        ))
        .expect("marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    harness
        .publications
        .send(connection(1, FeedConnectionState::Stopped, None))
        .expect("retired connection");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_ne!(pane.chart_failure, Some(ChartFailure::WorkerStopped));
    });
    harness
        .publications
        .send(connection(
            sequence,
            FeedConnectionState::Streaming,
            Some(12_500_000),
        ))
        .expect("current connection");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.connection_state, Some(FeedConnectionState::Streaming));
        assert_eq!(pane.provider_transport_rtt_nanos, Some(12_500_000));
    });
}

#[gpui::test]
fn pane_harness_repeated_connection_stop_has_one_redacted_notice_and_streaming_clears(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    for _ in 0..2 {
        harness
            .publications
            .send(PaneProjectionEvent::SessionStatus {
                generation: 1,
                status: LocalSessionStatus::Connection {
                    state: FeedConnectionState::Stopped,
                    transport_rtt_nanos: None,
                },
            })
            .expect("bounded mailbox accepts connection failure");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_failure, Some(ChartFailure::WorkerStopped));
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].key.kind, "chart_state");
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("private-credential-sentinel")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    harness
        .publications
        .send(PaneProjectionEvent::SessionStatus {
            generation: 1,
            status: LocalSessionStatus::Connection {
                state: FeedConnectionState::Streaming,
                transport_rtt_nanos: None,
            },
        })
        .expect("bounded mailbox accepts connection recovery");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_failure, None);
        assert_eq!(pane.chart_state, ChartState::Loading);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_repeated_series_dispatch_failure_has_one_notice_and_retry_clears(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    harness.pane.update(cx, |pane, _| {
        pane.product
            .as_mut()
            .expect("fixture product")
            .display_symbol = "private-credential-sentinel".into();
        for _ in 0..8 {
            pane.market_worker
                .try_set_order_book_detail(OrderBookDetail::Depth)
                .expect("fill bounded command queue");
        }
    });
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            assert!(!pane.select_interval(ChartInterval::Minute5, cx));
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].key.kind, "series_selection");
        assert_eq!(toaster.notices()[0].title, "Timeframe unavailable");
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("private-credential-sentinel")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    while harness.commands.as_ref().unwrap().try_recv().is_ok() {}
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_interval(ChartInterval::Minute5, cx));
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_trading_projection_failure_is_keyed_and_recovery_clears(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            let chart = pane.chart.clone().expect("fixture chart");
            workspace_layout::refresh_chart_trading_projection(
                &chart,
                None,
                ChartHostOverlaySnapshot::default(),
                vec![(f64::NAN, "private-credential-sentinel".into())],
                cx,
            );
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].key.kind, "trading_projection");
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("private-credential-sentinel")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    harness.pane.update(cx, |pane, cx| {
        let chart = pane.chart.clone().expect("fixture chart");
        workspace_layout::refresh_chart_trading_projection(
            &chart,
            None,
            ChartHostOverlaySnapshot::default(),
            vec![],
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
}

#[gpui::test]
fn pane_harness_catalog_failure_before_selection_does_not_fail_chart(cx: &mut TestAppContext) {
    let (harness, cx) =
        PaneHarness::open_with_startup(WorkspaceSurfaceRestore::default(), false, true, cx);
    let initial_state = cx.read(|cx| harness.pane.read(cx).chart_state);
    for _ in 0..2 {
        harness
            .publications
            .send(PaneProjectionEvent::LocalFailure {
                scope: aeris_desktop::market_worker::LocalFailureScope::Catalog,
                message: "private-catalog-detail".into(),
            })
            .expect("catalog failure enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_state, initial_state);
        assert_eq!(pane.chart_failure, None);
        let notices = harness.window.read(cx).toaster.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Symbol search unavailable");
        assert!(!notices[0].detail.contains("private-catalog-detail"));
    });
}

#[gpui::test]
fn pane_harness_fixture_futures_startup_waits_for_runtime_snapshot(cx: &mut TestAppContext) {
    let (mut harness, cx) =
        PaneHarness::open_with_startup(WorkspaceSurfaceRestore::default(), false, true, cx);
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_state, ChartState::Loading);
        assert!(pane.chart.is_none(), "no chart before the runtime replies");
        assert_eq!(
            pane.product.as_ref().expect("selected fixture").provider,
            "rithmic"
        );
    });
    harness.publish_snapshot(12, cx);
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(pane.chart.is_some(), "runtime snapshot installs the chart");
    });
}

#[gpui::test]
fn pane_harness_late_catalog_failure_cannot_fail_selected_chart(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let chart = cx.read(|cx| harness.pane.read(cx).chart.clone().expect("ready chart"));
    harness
        .publications
        .send(PaneProjectionEvent::LocalFailure {
            scope: aeris_desktop::market_worker::LocalFailureScope::Catalog,
            message: "retired catalog failure".into(),
        })
        .expect("catalog failure enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_state, ChartState::Ready);
        assert_eq!(pane.chart_failure, None);
        assert_eq!(
            pane.chart.as_ref().map(Entity::entity_id),
            Some(chart.entity_id())
        );
        assert_eq!(harness.window.read(cx).toaster.notices().len(), 1);
    });
}

#[gpui::test]
fn pane_harness_endpoint_failure_is_selection_fenced(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    harness
        .publications
        .send(PaneProjectionEvent::LocalFailure {
            scope: aeris_desktop::market_worker::LocalFailureScope::Selection(2),
            message: "retired endpoint failed".into(),
        })
        .expect("retired failure");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Ready);
        assert_eq!(pane.chart_failure, None);
    });
    harness
        .publications
        .send(PaneProjectionEvent::LocalFailure {
            scope: aeris_desktop::market_worker::LocalFailureScope::Selection(1),
            message: "current endpoint failed".into(),
        })
        .expect("current failure");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, ChartState::Error);
        assert_eq!(pane.chart_failure, Some(ChartFailure::Market));
    });
}

#[gpui::test]
fn pane_harness_chart_failure_renders_one_redacted_notice_then_snapshot_clears(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    for _ in 0..2 {
        harness
            .publications
            .send(harness.fixture_readiness(
                1,
                aeris_contracts::SeriesLoadState::Failed,
                "private-credential-sentinel",
            ))
            .expect("bounded fixture mailbox accepts failure");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(pane.chart_failure, Some(ChartFailure::Market));
        let notice = workspace_layout::pane_chart_notice(pane, cx).expect("error status");
        assert_eq!(
            notice.detail.as_deref(),
            Some(ChartFailure::Market.detail())
        );
        let toaster = &harness.window.read(cx).toaster;
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.notices()[0].title, "Chart unavailable");
        assert!(
            !toaster.notices()[0]
                .detail
                .contains("private-credential-sentinel")
        );
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            1
        );
    });
    assert!(cx.debug_bounds("toast").is_some(), "chart failure rendered");
    harness.publish_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(harness.pane.read(cx).chart_failure, None);
        assert_eq!(harness.window.read(cx).toaster.notices(), []);
    });
}

#[gpui::test]
fn pane_harness_smoke_disconnected_fixture_renders_pane(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    cx.read(|cx| assert!(harness.pane.read(cx).chart.is_none()));
    harness.publish_snapshot(12, cx);
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(
            pane.chart.is_some(),
            "fixture snapshot creates the real chart"
        );
        assert_eq!(pane.chart_state, ChartState::Ready);
        assert_eq!(
            (pane.subscription_id.as_str(), pane.worker_label.as_str()),
            (
                "desktop_runtime_hyperliquid_bars",
                "Hyperliquid market runtime"
            ),
            "canonical fixture bars use the runtime's projection identity"
        );
    });
}

#[gpui::test]
fn pane_harness_off_viewport_working_order_line_paints_without_overflow(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    cx.run_until_parked();
    let chart = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .chart
            .clone()
            .expect("populated chart")
    });
    chart.update(cx, |chart, cx| {
        chart
            .set_trading_snapshot(ChartTradingSnapshot {
                orders: vec![ChartWorkingOrder {
                    id: ChartOrderId::new("distant-working-order").expect("order id"),
                    account_id: None,
                    pane_index: 0,
                    price_scale: ChartTradingPriceScale::Right,
                    side: ChartOrderSide::Sell,
                    kind: ChartOrderKind::Limit,
                    role: ChartOrderRole::Working,
                    status: ChartOrderStatus::Working,
                    price: 1.0e16,
                    stop_price: None,
                    trailing_trigger_price: None,
                    break_even_trigger_price: None,
                    quantity: 1.0,
                    filled_quantity: 0.0,
                    position_id: None,
                    parent_order_id: None,
                    bracket_id: None,
                    oco_group_id: None,
                    revision: 1,
                    annotations: Vec::new(),
                }],
                ..Default::default()
            })
            .expect("valid working order");
        cx.notify();
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let chart = chart.read(cx);
        assert_eq!(chart.trading_snapshot().orders[0].price, 1.0e16);
        assert!(
            chart.rendered_viewport_size().0 > 0.0,
            "the real GPUI pane must paint the order"
        );
    });
}

#[gpui::test]
fn replacement_chart_requests_the_current_trading_projection(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let previous = harness.pane.update(cx, |pane, _| {
        pane.trading_pnl.applied_selection = Some(workspace_layout::pane_trading_selection(pane));
        assert!(workspace_layout::trading_projection_current(pane));
        pane.chart.clone().expect("installed chart")
    });
    harness.reinstall_snapshot(12, cx);
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        let chart = pane.chart.as_ref().expect("replacement chart");
        assert_ne!(chart.entity_id(), previous.entity_id());
        assert!(
            !workspace_layout::trading_projection_current(pane),
            "an interval switch's replacement chart must receive the trading lines again"
        );
    });
}

#[gpui::test]
fn descriptor_labels_render_without_desktop_mapping(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    let mut descriptor = cx.read(|cx| {
        harness
            .pane
            .read(cx)
            .descriptor
            .clone()
            .expect("fixture descriptor")
    });
    descriptor.instrument.provider = "fixture-provider".into();
    descriptor.instrument.instrument_id = "opaque-7a1f".into();
    descriptor.instrument.entitlement_id = "fixture-access".into();
    descriptor.entitlement_id = "fixture-access".into();
    descriptor.instrument.display_symbol = "outdated installed symbol".into();
    descriptor.display_symbol = "Fixture Symbol".into();
    descriptor.venue = "Fixture Exchange".into();
    descriptor.asset_class = aeris_instruments::InstrumentClass::Equity;
    descriptor.currency = Some("EUR".into());
    let product = descriptor.instrument.clone();
    harness
        .pane
        .update(cx, |pane, _| pane.product = Some(product.clone()));
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::BoundDescriptor {
                generation: 1,
                descriptor: Box::new(descriptor.clone()),
                provider_display_name: "Fixture Venue".into(),
            },
        ))
        .expect("bound descriptor");
    harness
        .publications
        .send(PaneProjectionEvent::LocalCommandOutcome(
            LocalCommandOutcome::SelectedInstrument {
                generation: 1,
                instrument: product.clone(),
            },
        ))
        .expect("selection");
    harness
        .pane
        .update(cx, |pane, cx| assert_eq!(pane.poll_market_worker(cx), 2));
    let route = FixtureSeries {
        series: aeris_market_data::BarSeriesKey {
            provider_id: product.provider.clone(),
            instrument_id: product.instrument_id.clone(),
            entitlement_id: product.entitlement_id.clone(),
            period: aeris_market_data::BarPeriod::time(60).expect("minute"),
            definition_version: 1,
        },
        provider_generation: product.session_generation,
        price_scale: descriptor.price_scale,
        quantity_scale: u8::try_from(product.quantity_scale).expect("quantity scale"),
    };
    let snapshot = harness
        .market
        .publish_runtime_snapshot(12, &route, 1, &harness.market_events)
        .expect("opaque snapshot");
    harness
        .publications
        .send(PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection {
                event: snapshot,
                received_unix_nanos: Some(301_000_000_000),
                recovery_request_id: None,
                selection_generation: None,
            },
        ))
        .expect("fixture publication");
    harness
        .pane
        .update(cx, |pane, cx| assert_eq!(pane.poll_market_worker(cx), 1));
    cx.run_until_parked();
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert_eq!(terminal_instrument_label(pane), "Fixture Symbol");
        assert_eq!(pane.provider_display_name.as_deref(), Some("Fixture Venue"));
        assert_eq!(
            pane.chart.as_ref().expect("chart").read(cx).legend_title(),
            "Fixture Symbol · 1m · Fixture Venue"
        );
    });
}
