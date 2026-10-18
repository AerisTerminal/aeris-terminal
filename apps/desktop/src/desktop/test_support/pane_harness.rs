//! GPUI pane fixture exercising the real bounded market mailbox without a provider session.

use super::*;
use aeris_desktop::market_worker::{
    EngineWorkerStartup, MarketWorkerCommand, MarketWorkerSender, market_worker_channel,
};
use aeris_desktop::test_support::FixtureMarketWorker;
use gpui::{TestAppContext, VisualTestContext};
use std::num::NonZeroUsize;
use std::sync::{atomic::AtomicU64, mpsc};

struct PaneHarness {
    pane: Entity<WorkspaceSurface>,
    window: Entity<PaneWindow>,
    market: FixtureMarketWorker,
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
        let product = local_state::default_workspace().watchlist_entries[0]
            .instrument
            .clone()
            .expect("fixture product");
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
            let pane = workspace_surface_entity(
                MarketWorkerStartup::Loading(Box::new(EngineWorkerStartup {
                    product,
                    interval: ChartInterval::Minute1,
                    restored_viewport: None,
                    subscription_id: "desktop_fixture_market_bars".into(),
                    worker_label: "disconnected fixture".into(),
                })),
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
                publications,
                commands: Some(requests),
            },
            cx,
        )
    }

    fn publish_snapshot(&mut self, bar_count: usize, cx: &mut VisualTestContext) {
        let snapshot = self
            .market
            .publish_snapshot(bar_count)
            .expect("fixture snapshot");
        self.publications
            .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Snapshot(snapshot.snapshot),
                generation: MarketPublicationGeneration::from_generation(&snapshot.generation),
                subscription_id: snapshot.subscription_id,
                worker_label: snapshot.worker_label,
                ui_diagnostics: None,
            }))
            .expect("bounded fixture mailbox accepts publication");
        self.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
        self.publications
            .send(MarketWorkerMessage::State {
                state: ChartState::Ready,
                message: "disconnected fixture ready".into(),
            })
            .expect("fixture readiness enters mailbox");
        self.pane.update(cx, |pane, cx| {
            assert_eq!(pane.poll_market_worker(cx), 1);
        });
    }
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
    harness.pane.update(cx, |pane, _| {
        pane.rithmic_switch = RithmicSwitchState::Swapping;
    });
    harness.publish_snapshot(12, cx);
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
    });
    assert!(
        cx.debug_bounds("toast").is_some(),
        "failure rendered as a toast card"
    );
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
fn pane_harness_chart_failure_renders_one_redacted_notice_then_snapshot_clears(
    cx: &mut TestAppContext,
) {
    let (mut harness, cx) = PaneHarness::open(cx);
    for _ in 0..2 {
        harness
            .publications
            .send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: "private-credential-sentinel".into(),
            })
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
        assert!(pane.worker_label.contains("disconnected"));
        assert_eq!(pane.subscription_id, "desktop_fixture_market_bars");
    });
}
