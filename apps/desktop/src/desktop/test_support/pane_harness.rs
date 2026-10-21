//! GPUI pane fixture exercising the real bounded market mailbox without a provider session.

use super::*;
use aeris_desktop::market_worker::{
    EngineWorkerStartup, MarketWorkerCommand, MarketWorkerSender, market_worker_channel,
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
    publications: MarketWorkerSender,
    commands: Option<mpsc::Receiver<MarketWorkerCommand>>,
}

struct PaneWindow {
    pane: Entity<WorkspaceSurface>,
    toaster: Toaster,
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
    fn fixture_snapshot(
        &mut self,
        product: &InstallProviderInstrument,
        interval: ChartInterval,
        generation: u64,
        bar_count: usize,
    ) -> MarketWorkerMessage {
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
        MarketWorkerMessage::RuntimeSeriesPublication {
            event,
            received_unix_nanos: 301_000_000_000,
        }
    }

    fn fixture_readiness(
        &self,
        generation: u64,
        state: aeris_contracts::SeriesLoadState,
        detail: &str,
    ) -> MarketWorkerMessage {
        MarketWorkerMessage::RuntimeSeriesStatus(
            self.market_events.readiness(generation, state, detail),
        )
    }

    fn fixture_recovering(&self, generation: u64, detail: &str) -> MarketWorkerMessage {
        MarketWorkerMessage::RuntimeSeriesStatus(self.market_events.demand_error(
            generation,
            aeris_market_runtime::MarketDemandErrorScope::Series,
            detail,
        ))
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
        rithmic_startup: bool,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        let product = local_state::default_workspace().watchlist_entries[0]
            .instrument
            .clone()
            .expect("fixture product");
        Self::open_with_product(
            restore,
            fill_command_queue,
            rithmic_startup,
            product,
            ChartInterval::Minute1,
            cx,
        )
    }

    fn open_with_product(
        restore: WorkspaceSurfaceRestore,
        fill_command_queue: bool,
        rithmic_startup: bool,
        product: InstallProviderInstrument,
        interval: ChartInterval,
        cx: &mut TestAppContext,
    ) -> (Self, &mut VisualTestContext) {
        let (commands, requests) = mpsc::sync_channel(8);
        let (publications, messages) = market_worker_channel(NonZeroUsize::new(8).unwrap());
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
                    .try_set_order_book_visible(true)
                    .expect("fill bounded command queue before restore");
            }
        }
        let (view, cx) = cx.add_window_view(move |window, cx| {
            gpui_base::init(cx);
            let startup = if rithmic_startup {
                MarketWorkerStartup::Rithmic
            } else {
                MarketWorkerStartup::Loading(Box::new(EngineWorkerStartup {
                    product,
                    interval,
                    restored_viewport: None,
                    subscription_id: "desktop_fixture_market_bars".into(),
                    worker_label: "disconnected fixture".into(),
                }))
            };
            let pane = workspace_surface_entity(
                startup,
                worker,
                &DesktopLifecycle::new_for_test(),
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
                pane.emit_initial_price_alert_restore_notice(cx);
                pane.emit_initial_chart_failure_notice(cx);
                pane.apply_initial_chart_settings(cx);
            });
            PaneWindow {
                pane,
                toaster: Toaster::default(),
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
            .send(MarketWorkerMessage::RuntimeSeriesStatus(
                self.market_events.readiness(
                    1,
                    aeris_contracts::SeriesLoadState::Live,
                    "disconnected fixture ready",
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

#[gpui::test]
fn pane_harness_four_restored_panes_apply_one_initial_reply_each(cx: &mut TestAppContext) {
    let default_product = local_state::default_workspace().watchlist_entries[0]
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
        let mut product = default_product.clone();
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
            .send(MarketWorkerMessage::SelectedInstrument {
                generation: 1,
                instrument: product.clone(),
            })
            .expect("restored selection reply enters mailbox");
        harness.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        let mut duplicate = product.clone();
        duplicate.price_increment = Some(999);
        harness
            .publications
            .send(MarketWorkerMessage::SelectedInstrument {
                generation: 1,
                instrument: duplicate,
            })
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
            .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        .send(MarketWorkerMessage::MarketSessionStatus {
            generation: 1,
            status: old_status.clone(),
        })
        .expect("old status while pending");
    harness.pane.update(cx, |pane, cx| {
        pane.poll_market_worker(cx);
        assert_eq!(pane.market_session_status.as_ref(), Some(&old_status));
    });
    harness
        .publications
        .send(MarketWorkerMessage::MarketSessionStatus {
            generation: 1,
            status: MarketSessionStatus {
                instrument_id: "uncommitted instrument".into(),
                ..old_status.clone()
            },
        })
        .expect("catalog status ahead of selection");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.market_session_status.as_ref(), Some(&old_status));
    });
    harness
        .publications
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
        .expect("marker");
    harness
        .publications
        .send(MarketWorkerMessage::MarketSessionStatus {
            generation: 1,
            status: old_status.clone(),
        })
        .expect("queued stale status");
    let new_status = MarketSessionStatus {
        phase: MarketSessionPhase::Regular,
        ..old_status.clone()
    };
    harness
        .publications
        .send(MarketWorkerMessage::MarketSessionStatus {
            generation: sequence,
            status: new_status.clone(),
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
    use aeris_market_data::{OrderBookFrame, OrderBookState};

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
    let frame = |generation, revision| OrderBookFrame {
        provider_id: product.provider.clone(),
        instrument_id: product.instrument_id.clone(),
        entitlement_id: product.entitlement_id.clone(),
        session_generation: 1,
        selection_generation: generation,
        revision,
        source_watermark: revision,
        bbo_source_watermark: revision,
        state: OrderBookState::Ready,
        price_scale: 2,
        quantity_scale: 0,
        price_increment: Some(1),
        best_bid: None,
        best_ask: None,
        traded_volumes: std::collections::BTreeMap::default(),
        trade_source_watermark: revision,
        top_of_book_only: false,
        rows: Vec::new(),
    };
    harness
        .publications
        .send(MarketWorkerMessage::OrderBook(frame(1, 1)))
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
        .send(MarketWorkerMessage::OrderBook(frame(sequence, 90)))
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
        .expect("selection marker");
    harness
        .publications
        .send(MarketWorkerMessage::OrderBook(frame(1, 99)))
        .expect("retired higher revision");
    harness
        .publications
        .send(MarketWorkerMessage::OrderBook(frame(sequence, 2)))
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
            .send(MarketWorkerMessage::EngineSelectionFailed { sequence })
            .expect("selection failure enters mailbox");
        harness
            .publications
            .send(MarketWorkerMessage::MarketSessionStatus {
                generation: 1,
                status: old_status.clone(),
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
        .expect("replacement marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let before = cx.read(|cx| harness.pane.read(cx).chart_state);
    harness
        .publications
        .send(MarketWorkerMessage::Recovery {
            generation: 1,
            request_id,
            result: Err(aeris_desktop::market_worker::MarketRecoveryFailure::Failed(
                "retired private recovery failure".into(),
            )),
        })
        .expect("retired recovery");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_state, before);
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
            .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
fn pane_harness_interval_switch_keeps_chart_uncovered_until_replacement(cx: &mut TestAppContext) {
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
            !pane.showing_superseded_series(),
            "pending interval-only switch"
        );
        assert_eq!(pane.chart.as_ref().map(Entity::entity_id), Some(original));
    });
    harness
        .publications
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
        .expect("marker enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.read(|cx| {
        let pane = harness.pane.read(cx);
        assert!(
            !pane.showing_superseded_series(),
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        .send(MarketWorkerMessage::RuntimeSeriesPublication {
            event: stale_delta,
            received_unix_nanos: 841_000_000_000,
        })
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
            0 => MarketWorkerMessage::EngineSelectionCommitted { sequence },
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence: initial })
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
        .send(MarketWorkerMessage::EngineSelectionCommitted {
            sequence: final_generation,
        })
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
    publication: impl Fn(u64) -> MarketWorkerMessage,
) {
    let final_generation = *sequences.last().expect("final request");
    for &generation in sequences[..sequences.len() - 1].iter().rev() {
        harness
            .publications
            .send(MarketWorkerMessage::EngineSelectionCommitted {
                sequence: generation,
            })
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
) -> MarketWorkerMessage {
    MarketWorkerMessage::RuntimeSeriesPublication {
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
        received_unix_nanos: 301_000_000_000,
    }
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
        .send(MarketWorkerMessage::RuntimeSeriesPublication {
            event: events.update(
                &FixtureSeries {
                    series,
                    provider_generation: provider_generation.0.get(),
                    price_scale: u8::try_from(product.price_scale).expect("price scale"),
                    quantity_scale: u8::try_from(product.quantity_scale).expect("quantity scale"),
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
            received_unix_nanos: 601_000_000_000,
        })
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        MarketWorkerMessage::RuntimeSeriesStatus(events.readiness(generation, state, detail))
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
}

#[gpui::test]
fn pane_harness_fenced_runtime_failure_restores_previous_binding(cx: &mut TestAppContext) {
    use aeris_contracts::SeriesLoadState;
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let events = FixtureMarketEvents::default();
    let failed = |generation| {
        MarketWorkerMessage::RuntimeSeriesStatus(events.readiness(
            generation,
            SeriesLoadState::Failed,
            "market unavailable",
        ))
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        MarketWorkerMessage::RuntimeSeriesStatus(events.demand_error(
            generation,
            scope,
            "coverage unavailable",
        ))
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        .send(MarketWorkerMessage::Recovery {
            generation: 1,
            request_id,
            result: Err(aeris_desktop::market_worker::MarketRecoveryFailure::Failed(
                "private old recovery failure".into(),
            )),
        })
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
    harness.pane.update(cx, |pane, _| {
        let search = pane.symbol_browser.begin_search("ETH").expect("search");
        assert!(pane.symbol_browser.apply_results(
            search.request_id,
            vec![ProviderInstrumentSummary {
                symbol: "ETH".into(),
                display_symbol: "ETH".into(),
                exchange: "TEST".into(),
                ..ProviderInstrumentSummary::default()
            }]
        ));
    });
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_instrument(
            InstrumentMenuSelection(0),
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
    harness
        .publications
        .send(MarketWorkerMessage::CatalogSwitchFailed {
            sequence: request.sequence,
            browser_generation: request.browser_generation,
            detail: "catalog failed".into(),
        })
        .expect("catalog failure");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(pane.chart_pane_controller.pending_selection(), None);
        assert!(pane.chart_pane_controller.accept_series_state(1));
        assert_eq!(pane.chart_state, ChartState::Ready);
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
        .expect("marker");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    let study_id = StudyInstanceId::try_from_u64(3).expect("study identity");
    harness.pane.update(cx, |pane, _| {
        pane.studies.active.push(RuntimeStudyState {
            study_id,
            persisted: workspace_surface::legacy_runtime_study(3, ChartIndicator::Sma, true)
                .expect("study fixture"),
            resolved_chart_series: None,
        });
    });
    let removed = |generation| {
        MarketWorkerMessage::StudyRemoved(MarketStudyRemoved {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: Some(MarketGenerationId(
                NonZeroU64::new(generation).expect("generation"),
            )),
            study_ids: vec![study_id],
        })
    };
    harness
        .publications
        .send(removed(1))
        .expect("retired removal");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert_eq!(
            pane.studies.active.len(),
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
        assert!(pane.studies.active.is_empty(), "current removal is applied");
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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

fn check_instrument_cover_through_ready(snapshot_first: bool, cx: &mut TestAppContext) {
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
    harness.pane.update(cx, |pane, cx| {
        assert!(pane.select_installed_instrument(&replacement, cx));
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
        .expect("selection commit");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
        assert!(pane.showing_superseded_series());
    });

    let covering = harness.fixture_snapshot(&replacement, ChartInterval::Minute1, sequence, 13);
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
                "cover requires both readiness and covering bars; snapshot_first={snapshot_first}, step={step}"
            );
            if step == 1 {
                assert_eq!(pane.chart_state, ChartState::Ready);
            }
        });
    }
}

#[gpui::test]
fn pane_harness_instrument_cover_waits_for_snapshot_after_ready(cx: &mut TestAppContext) {
    check_instrument_cover_through_ready(false, cx);
}

#[gpui::test]
fn pane_harness_instrument_cover_waits_for_ready_after_snapshot(cx: &mut TestAppContext) {
    check_instrument_cover_through_ready(true, cx);
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
                .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
                .expect("marker enters mailbox");
        }
        harness
            .publications
            .send(MarketWorkerMessage::SelectedInstrument {
                generation: sequence.saturating_sub(1),
                instrument: stale.clone(),
            })
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
        .send(MarketWorkerMessage::EngineSelectionFailed { sequence: first })
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
        .send(MarketWorkerMessage::EngineSelectionFailed { sequence })
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
        assert!(
            harness.pane.read(cx).chart.is_some(),
            "startup chart exists"
        );
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
    let (harness, cx) = PaneHarness::open_with_startup(restore, false, true, cx);
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
fn pane_harness_full_symbol_search_queue_renders_once_and_completion_clears(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    while commands.try_recv().is_ok() {}
    harness.pane.update(cx, |pane, _| {
        for _ in 0..8 {
            pane.market_worker
                .try_set_order_book_visible(true)
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
            "Waiting to search the latest query"
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
    });
    let MarketWorkerCommand::ProviderSearch(request) =
        commands.try_recv().expect("retained search dispatched")
    else {
        panic!("expected provider search");
    };
    harness
        .publications
        .send(MarketWorkerMessage::ProviderCatalog(
            ProviderCatalogEvent::SearchCompleted(ProviderInstrumentSearchResult {
                provider: request.provider,
                search_generation: request.search_generation,
                ..ProviderInstrumentSearchResult::default()
            }),
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
            "No matching markets · try another search"
        );
        assert_eq!(toaster.notices(), []);
        assert_eq!(
            toaster.live_notice_cards(toaster::ToastStack::of(&harness.pane)),
            0
        );
    });
}

#[gpui::test]
fn pane_harness_rejected_symbol_selection_redacts_detail_and_commit_clears(
    cx: &mut TestAppContext,
) {
    let (harness, cx) = PaneHarness::open(cx);
    let commands = harness.commands.as_ref().expect("command receiver");
    while commands.try_recv().is_ok() {}
    let instrument = local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("fixture product");
    harness.pane.update(cx, |pane, _| {
        let request = pane
            .symbol_browser
            .begin_search("BTC")
            .expect("valid query");
        assert!(pane.symbol_browser.apply_results(
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
    });
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            assert!(pane.select_instrument(
                InstrumentMenuSelection(0),
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
        harness
            .publications
            .send(MarketWorkerMessage::CatalogSwitchFailed {
                sequence: request.sequence,
                browser_generation: request.browser_generation,
                detail: "private-credential-sentinel".into(),
            })
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
            InstrumentMenuSelection(0),
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
    harness
        .publications
        .send(MarketWorkerMessage::CatalogSwitchCommitted {
            sequence: request.sequence,
            browser_generation: request.browser_generation,
            instrument,
            interval: ChartInterval::Minute1,
        })
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
            .send(MarketWorkerMessage::PriceAlertSyncFailed)
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
        .send(MarketWorkerMessage::PriceAlertSyncSucceeded)
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
        .send(MarketWorkerMessage::PriceAlertSyncSucceeded)
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
                .try_set_order_book_visible(true)
                .expect("fill remaining command slots");
        }
    });
    for _ in 0..2 {
        harness.pane.update(cx, |pane, cx| {
            pane.delete_price_alert("restored-alert", cx);
            assert_eq!(pane.price_alerts.len(), 1);
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
        assert_eq!(pane.price_alerts.len(), 0);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
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
            pane.studies.pending.insert(
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
            .send(MarketWorkerMessage::StudyRegistrationFailed {
                request_sequence: sequence,
                message: "private-credential-sentinel".into(),
            })
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
        pane.studies.pending.insert(
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
    harness
        .publications
        .send(MarketWorkerMessage::StudyRegistered {
            request_sequence: 3,
            study_id: StudyInstanceId::try_from_u64(3).expect("fixture identity"),
        })
        .expect("fixture registration enters mailbox");
    harness.pane.update(cx, |pane, cx| {
        assert_eq!(pane.poll_market_worker(cx), 1);
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(harness.window.read(cx).toaster.notices(), []));
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
    let connection = |generation, state, rtt| MarketWorkerMessage::Connection {
        generation,
        state,
        message: "fixture connection".into(),
        transport_rtt_nanos: rtt,
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
        .send(MarketWorkerMessage::EngineSelectionCommitted { sequence })
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
            .send(MarketWorkerMessage::Connection {
                generation: 1,
                state: FeedConnectionState::Stopped,
                message: "private-credential-sentinel".into(),
                transport_rtt_nanos: None,
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
        .send(MarketWorkerMessage::Connection {
            generation: 1,
            state: FeedConnectionState::Streaming,
            message: "private-credential-sentinel".into(),
            transport_rtt_nanos: None,
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
                .try_set_order_book_visible(true)
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
            .send(MarketWorkerMessage::CatalogFailure {
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
fn pane_harness_late_catalog_failure_cannot_fail_selected_chart(cx: &mut TestAppContext) {
    let (mut harness, cx) = PaneHarness::open(cx);
    harness.publish_snapshot(12, cx);
    let chart = cx.read(|cx| harness.pane.read(cx).chart.clone().expect("ready chart"));
    harness
        .publications
        .send(MarketWorkerMessage::CatalogFailure {
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
            engine_market_worker::worker_identity("hyperliquid"),
            "canonical fixture bars use the runtime's projection identity"
        );
    });
}
