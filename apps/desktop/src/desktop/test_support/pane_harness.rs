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
    market: FixtureMarketWorker,
    publications: MarketWorkerSender,
    _commands: mpsc::Receiver<MarketWorkerCommand>,
}

struct PaneWindow(Entity<WorkspaceSurface>);

impl Render for PaneWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.0.read(cx).chart.clone())
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
            PaneWindow(pane)
        });
        let pane = cx.read(|cx| view.read(cx).0.clone());
        (
            Self {
                pane,
                market: FixtureMarketWorker::try_new().expect("fixture market"),
                publications,
                _commands: requests,
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
