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
                WorkspaceSurfaceRestore::default(),
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
