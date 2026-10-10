//! Notices stacked in the top-right corner of a chart, clear of its price axis: the chart's own
//! status (stale, reconnecting, unavailable) and order fills. Each card slides in from the right
//! while it fades in, eased out, and leaves the same way.
//!
//! The trading owner stays authoritative for fills. This module only remembers which fills it
//! has already announced, so each new execution produces one notice.

use super::*;
use std::collections::BTreeSet;

const ENTER_DURATION: Duration = Duration::from_millis(240);
const LEAVE_DURATION: Duration = Duration::from_millis(200);
/// How far a card travels while it slides in or out.
const SLIDE_DISTANCE: f32 = 16.0;
/// Space between the stack and the chart's top edge and price axis, and between cards.
const NOTICE_GAP: f32 = 8.0;
const NOTICE_MAX_WIDTH: f32 = 280.0;
const TONE_DOT_SIZE: f32 = 6.0;
const FILL_NOTICE_HOLD: Duration = Duration::from_secs(6);
const MAXIMUM_FILL_NOTICES: usize = 4;
/// A fill first reported this long after it executed is history the owner caught up on after a
/// reconnect or restart, not news.
const FILL_NOTICE_FRESHNESS_NANOS: i64 = 60_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NoticePhase {
    Shown,
    Leaving,
}

/// Slides and fades a card: `shown` runs from 0 (offset right, transparent) to 1 (settled).
fn place<E: Styled>(card: E, shown: f32) -> E {
    card.left(px(SLIDE_DISTANCE * (1.0 - shown))).opacity(shown)
}

fn animated<E: IntoElement + Styled + 'static>(
    card: E,
    name: &'static str,
    key: u64,
    phase: NoticePhase,
) -> AnyElement {
    match phase {
        NoticePhase::Shown => card
            .with_animation(
                (name, key),
                Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                place,
            )
            .into_any_element(),
        NoticePhase::Leaving => card
            .with_animation(
                (name, key),
                Animation::new(LEAVE_DURATION).with_easing(ease_out_quint()),
                |card, eased| place(card, 1.0 - eased),
            )
            .into_any_element(),
    }
}

/// One notice: a dot in the notice's tone, a title and an optional quieter detail line.
pub(super) fn notice_card(
    accent: ThemeColor,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    div()
        .relative()
        .max_w(px(NOTICE_MAX_WIDTH))
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .child(
            div()
                .flex_none()
                .size(px(TONE_DOT_SIZE))
                .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
                .bg(gpui_color(accent)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_primary))
                        .child(title.into()),
                )
                .children(detail.map(|detail| {
                    div()
                        .text_xs()
                        .font_features(platform_tabular_numerals())
                        .text_color(gpui_color(colors.text_secondary))
                        .child(detail)
                })),
        )
}

pub(super) fn chart_notice_accent(tone: ChartNoticeTone, theme: &AerisTheme) -> ThemeColor {
    match tone {
        ChartNoticeTone::Muted => theme.colors.text_secondary,
        ChartNoticeTone::Warning => theme.colors.warning,
        ChartNoticeTone::Loss => theme.colors.danger,
    }
}

/// The top-right stack of a chart, inset past its price axis. It holds no pointer handlers of
/// its own, so the chart under the gaps keeps its hover and clicks.
pub(super) fn corner_notice_stack(
    cards: Vec<AnyElement>,
    price_axis_width: f32,
) -> Option<AnyElement> {
    (!cards.is_empty()).then(|| {
        div()
            .absolute()
            .top(px(NOTICE_GAP))
            .right(px(price_axis_width + NOTICE_GAP))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(NOTICE_GAP))
            .debug_selector(|| "corner_notice_stack".into())
            .children(cards)
            .into_any_element()
    })
}

/// The chart status shown in a pane's corner, kept on screen through its slide-out after the
/// condition clears.
#[derive(Debug, Default)]
pub(super) struct ChartCornerNotice {
    shown: Option<ChartSurfaceNotice>,
    leaving: Option<ChartSurfaceNotice>,
    /// Restarts the slide for each new notice and fences the removal of earlier ones.
    generation: u64,
}

impl ChartCornerNotice {
    /// Follows the pane's current notice. Returns the generation of a notice that has just
    /// started leaving, whose removal the caller schedules.
    pub(super) fn observe(&mut self, notice: Option<ChartSurfaceNotice>) -> Option<u64> {
        // A repair in progress is announced by the legend's own spinner beside the symbol.
        let notice = notice.filter(|notice| {
            notice.placement == ChartNoticePlacement::Corner
                && notice.label != ChartState::Loading.label()
        });
        let same_condition = match (&self.shown, &notice) {
            (Some(shown), Some(next)) => shown.label == next.label,
            (None, None) => true,
            _ => false,
        };
        if same_condition {
            // A changed detail, such as a retry message, updates the card in place.
            self.shown = notice;
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let previous = std::mem::replace(&mut self.shown, notice);
        self.leaving = if self.shown.is_none() { previous } else { None };
        self.leaving.is_some().then_some(self.generation)
    }

    /// Removes a notice once its slide-out has run. Returns whether anything changed.
    pub(super) fn finish(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.leaving.is_none() {
            return false;
        }
        self.leaving = None;
        true
    }

    pub(super) fn card(&self, theme: &AerisTheme) -> Option<AnyElement> {
        let (notice, phase) = match (&self.shown, &self.leaving) {
            (Some(notice), _) => (notice, NoticePhase::Shown),
            (None, Some(notice)) => (notice, NoticePhase::Leaving),
            (None, None) => return None,
        };
        let card = notice_card(
            chart_notice_accent(notice.tone, theme),
            notice.label,
            notice.detail.clone().map(SharedString::from),
            theme,
        )
        .debug_selector(|| "chart_corner_notice".into());
        Some(animated(card, "chart_notice", self.generation, phase))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FillNotice {
    key: u64,
    side: aeris_trading::OrderSide,
    title: String,
    detail: String,
    phase: NoticePhase,
}

/// Announcements of new order fills, newest first and bounded.
#[derive(Debug, Default)]
pub(super) struct FillNotifications {
    revision: Option<u64>,
    /// Fills the owner reported last. `None` until the first snapshot, whose fills are history.
    seen: Option<BTreeSet<aeris_trading::FillId>>,
    notices: VecDeque<FillNotice>,
    next_key: u64,
}

impl FillNotifications {
    /// Adopts a trading snapshot. Returns the keys of notices that have just started, whose hold
    /// and slide-out the caller schedules.
    pub(super) fn observe(
        &mut self,
        snapshot: &aeris_trading_runtime::TradingSnapshot,
        now_unix_nanos: i64,
    ) -> Vec<u64> {
        self.observe_fills(snapshot.revision, &snapshot.fills, now_unix_nanos, |fill| {
            fill_notice_text(fill, &snapshot.instruments, &snapshot.accounts)
        })
    }

    fn observe_fills(
        &mut self,
        revision: u64,
        fills: &[aeris_trading::Fill],
        now_unix_nanos: i64,
        text: impl Fn(&aeris_trading::Fill) -> (String, String),
    ) -> Vec<u64> {
        if self.revision == Some(revision) {
            return Vec::new();
        }
        self.revision = Some(revision);
        let current = fills.iter().map(|fill| fill.id.clone()).collect();
        let Some(seen) = self.seen.replace(current) else {
            return Vec::new();
        };
        let mut fresh: Vec<&aeris_trading::Fill> = fills
            .iter()
            .filter(|fill| {
                !seen.contains(&fill.id)
                    && now_unix_nanos.saturating_sub(fill.execution_unix_nanos)
                        <= FILL_NOTICE_FRESHNESS_NANOS
            })
            .collect();
        fresh.sort_by_key(|fill| fill.execution_unix_nanos);
        let mut started = Vec::with_capacity(fresh.len());
        for fill in fresh {
            self.next_key = self.next_key.wrapping_add(1);
            let (title, detail) = text(fill);
            self.notices.push_front(FillNotice {
                key: self.next_key,
                side: fill.side,
                title,
                detail,
                phase: NoticePhase::Shown,
            });
            started.push(self.next_key);
        }
        self.notices.truncate(MAXIMUM_FILL_NOTICES);
        started.retain(|key| self.notices.iter().any(|notice| notice.key == *key));
        started
    }

    /// Starts sliding a notice away. Returns whether anything changed.
    fn begin_leaving(&mut self, key: u64) -> bool {
        match self.notices.iter_mut().find(|notice| notice.key == key) {
            Some(notice) if notice.phase == NoticePhase::Shown => {
                notice.phase = NoticePhase::Leaving;
                true
            }
            _ => false,
        }
    }

    /// Removes a notice once its slide-out has run. Returns whether anything changed.
    fn finish(&mut self, key: u64) -> bool {
        let before = self.notices.len();
        self.notices
            .retain(|notice| notice.key != key || notice.phase != NoticePhase::Leaving);
        self.notices.len() != before
    }

    pub(super) fn cards(
        &self,
        terminal: &Entity<TerminalApp>,
        theme: &AerisTheme,
    ) -> impl Iterator<Item = AnyElement> {
        self.notices.iter().map(|notice| {
            let accent = match notice.side {
                aeris_trading::OrderSide::Buy => theme.colors.buy,
                aeris_trading::OrderSide::Sell => theme.colors.sell,
            };
            let key = notice.key;
            let terminal = terminal.clone();
            let card = notice_card(
                accent,
                notice.title.clone(),
                Some(notice.detail.clone().into()),
                theme,
            )
            .id(("fill_notice", key))
            .occlude()
            .cursor_pointer()
            .debug_selector(|| "fill_notice".into())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.dismiss_fill_notice(key, terminal_cx);
                });
            });
            animated(card, "fill_notice_slide", key, notice.phase)
        })
    }
}

/// "Bought 2 ESZ5" over "at 6,012.25 · Account", in the owner's exact fixed-point values.
fn fill_notice_text(
    fill: &aeris_trading::Fill,
    instruments: &[aeris_trading_runtime::TradingInstrument],
    accounts: &[aeris_trading::TradingAccount],
) -> (String, String) {
    let verb = match fill.side {
        aeris_trading::OrderSide::Buy => "Bought",
        aeris_trading::OrderSide::Sell => "Sold",
    };
    let symbol = instruments
        .iter()
        .find(|instrument| instrument.instrument_id == fill.instrument_id)
        .map_or(fill.instrument_id.as_str(), |instrument| {
            instrument.contract.provenance.display_symbol.as_str()
        });
    let account = accounts
        .iter()
        .find(|account| account.id == fill.account_id)
        .map_or(fill.account_id.as_str(), |account| {
            account.display_name.as_str()
        });
    let quantity = market_price_text(fill.quantity.units(), u32::from(fill.quantity.scale()));
    let price = market_price_text(fill.price.units(), u32::from(fill.price.scale()));
    (
        format!("{verb} {quantity} {symbol}"),
        format!("at {price} · {account}"),
    )
}

impl TerminalApp {
    pub(super) fn announce_fills(
        &mut self,
        snapshot: &aeris_trading_runtime::TradingSnapshot,
        cx: &mut Context<Self>,
    ) {
        let started = self
            .fill_notifications
            .observe(snapshot, terminal_chrome::current_unix_nanos());
        if started.is_empty() {
            return;
        }
        cx.notify();
        for key in started {
            cx.spawn(async move |terminal, cx| {
                cx.background_executor()
                    .timer(ENTER_DURATION + FILL_NOTICE_HOLD)
                    .await;
                let _ = terminal.update(cx, |terminal, terminal_cx| {
                    terminal.dismiss_fill_notice(key, terminal_cx);
                });
            })
            .detach();
        }
    }

    /// Slides a fill notice out, then removes it. A notice already leaving is left alone, so
    /// a click and the hold timer start one slide-out.
    fn dismiss_fill_notice(&mut self, key: u64, cx: &mut Context<Self>) {
        if !self.fill_notifications.begin_leaving(key) {
            return;
        }
        cx.notify();
        cx.spawn(async move |terminal, cx| {
            cx.background_executor().timer(LEAVE_DURATION).await;
            let _ = terminal.update(cx, |terminal, terminal_cx| {
                if terminal.fill_notifications.finish(key) {
                    terminal_cx.notify();
                }
            });
        })
        .detach();
    }

    /// Follows each visible pane's chart status so a cleared notice slides out instead of
    /// vanishing.
    pub(super) fn track_chart_corner_notices(&mut self, cx: &mut Context<Self>) {
        let surfaces: Vec<Entity<WorkspaceSurface>> = self.workspaces[self.active]
            .panes
            .iter()
            .map(|pane| pane.surface.clone())
            .collect();
        for surface in surfaces {
            let notice = pane_chart_notice(surface.read(cx), cx);
            let Some(generation) =
                surface.update(cx, |surface, _| surface.chart_corner_notice.observe(notice))
            else {
                continue;
            };
            let surface = surface.downgrade();
            cx.spawn(async move |terminal, cx| {
                cx.background_executor().timer(LEAVE_DURATION).await;
                let finished = surface.update(cx, |surface, _| {
                    surface.chart_corner_notice.finish(generation)
                });
                if matches!(finished, Ok(true)) {
                    let _ = terminal.update(cx, |_, terminal_cx| terminal_cx.notify());
                }
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    const NOW: i64 = 1_800_000_000_000_000_000;

    fn fill(id: &str, side: aeris_trading::OrderSide, executed: i64) -> aeris_trading::Fill {
        aeris_trading::Fill {
            id: aeris_trading::FillId::try_new(id).expect("fill id"),
            order_id: aeris_trading::OrderId::try_new(format!("order-{id}")).expect("order id"),
            account_id: aeris_trading::TradingAccountId::try_new("account").expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("EURUSD").expect("instrument"),
            side,
            price: aeris_trading::FixedPoint::try_new(108_412, 5).expect("price"),
            quantity: aeris_trading::FixedPoint::try_new(1, 2).expect("quantity"),
            execution_unix_nanos: executed,
            provenance: aeris_trading::TradingProvenance {
                venue_id: "simulated".to_string(),
                provider_id: "simulated".to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: executed,
            },
        }
    }

    fn observe(
        notifications: &mut FillNotifications,
        revision: u64,
        fills: &[aeris_trading::Fill],
    ) -> Vec<u64> {
        notifications.observe_fills(revision, fills, NOW, |fill| {
            fill_notice_text(fill, &[], &[])
        })
    }

    #[test]
    fn only_fills_that_arrive_after_the_first_snapshot_are_announced() {
        let mut notifications = FillNotifications::default();
        let history = fill("old", aeris_trading::OrderSide::Buy, NOW - 5_000_000_000);
        assert_eq!(
            observe(&mut notifications, 1, std::slice::from_ref(&history)),
            Vec::<u64>::new()
        );

        let new = fill("new", aeris_trading::OrderSide::Sell, NOW - 1_000_000_000);
        let started = observe(&mut notifications, 2, &[history.clone(), new.clone()]);
        assert_eq!(started.len(), 1);
        let notice = &notifications.notices[0];
        assert_eq!(notice.side, aeris_trading::OrderSide::Sell);
        assert_eq!(notice.title, "Sold 0.01 EURUSD");
        assert_eq!(notice.detail, "at 1.08412 · account");

        assert_eq!(
            observe(&mut notifications, 2, &[history.clone(), new.clone()]),
            Vec::<u64>::new(),
            "the same revision is adopted once"
        );
        assert_eq!(
            observe(&mut notifications, 3, &[history, new]),
            Vec::<u64>::new(),
            "an announced fill is not announced again"
        );
    }

    #[test]
    fn fills_caught_up_after_a_reconnect_are_not_news() {
        let mut notifications = FillNotifications::default();
        observe(&mut notifications, 1, &[]);
        let backfilled = fill(
            "backfilled",
            aeris_trading::OrderSide::Buy,
            NOW - 3_600_000_000_000,
        );
        assert_eq!(
            observe(&mut notifications, 2, &[backfilled]),
            Vec::<u64>::new()
        );
        assert_eq!(notifications.notices, VecDeque::new());
    }

    #[test]
    fn the_stack_keeps_the_newest_fills_first_and_bounded() {
        let mut notifications = FillNotifications::default();
        observe(&mut notifications, 1, &[]);
        let fills: Vec<_> = (0..6_i64)
            .map(|index| {
                fill(
                    &format!("fill-{index}"),
                    aeris_trading::OrderSide::Buy,
                    NOW - 10_000_000_000 + index,
                )
            })
            .collect();
        let started = observe(&mut notifications, 2, &fills);
        assert_eq!(started.len(), MAXIMUM_FILL_NOTICES);
        assert_eq!(notifications.notices.len(), MAXIMUM_FILL_NOTICES);
        assert_eq!(
            notifications.notices[0].key,
            *started.last().expect("newest"),
            "the latest execution is on top"
        );
    }

    #[test]
    fn a_fill_notice_slides_out_before_it_is_removed() {
        let mut notifications = FillNotifications::default();
        observe(&mut notifications, 1, &[]);
        let started = observe(
            &mut notifications,
            2,
            &[fill("new", aeris_trading::OrderSide::Buy, NOW)],
        );
        let key = started[0];
        assert!(
            !notifications.finish(key),
            "a shown notice slides out first"
        );
        assert!(notifications.begin_leaving(key));
        assert!(
            !notifications.begin_leaving(key),
            "a dismissal and the hold timer start one slide-out"
        );
        assert!(notifications.finish(key));
        assert_eq!(notifications.notices, VecDeque::new());
        assert!(!notifications.begin_leaving(key));
    }

    fn corner(label: &'static str, detail: &str) -> ChartSurfaceNotice {
        ChartSurfaceNotice {
            label,
            detail: Some(detail.to_string()),
            placement: ChartNoticePlacement::Corner,
            tone: ChartNoticeTone::Warning,
        }
    }

    #[test]
    fn a_cleared_chart_notice_slides_out_and_a_new_one_replaces_it() {
        let mut notice = ChartCornerNotice::default();
        assert_eq!(notice.observe(None), None);
        assert_eq!(
            notice.observe(Some(corner("Reconnecting chart", "retry 1"))),
            None
        );
        let first = notice.generation;
        assert_eq!(
            notice.observe(Some(corner("Reconnecting chart", "retry 2"))),
            None
        );
        assert_eq!(
            notice.generation, first,
            "a new detail does not restart the slide"
        );
        assert_eq!(
            notice
                .shown
                .as_ref()
                .and_then(|shown| shown.detail.as_deref()),
            Some("retry 2")
        );

        let leaving = notice.observe(None).expect("a cleared notice slides out");
        assert!(notice.leaving.is_some());
        assert_eq!(notice.observe(Some(corner("Chart stale", "silent"))), None);
        assert!(
            notice.leaving.is_none(),
            "a new notice replaces one leaving"
        );
        assert!(!notice.finish(leaving), "an earlier slide-out is retired");

        let leaving = notice.observe(None).expect("slides out");
        assert!(notice.finish(leaving));
        assert!(notice.card(&AerisTheme::dark()).is_none());
    }

    #[test]
    fn loading_and_centred_notices_never_reach_the_corner() {
        let mut notice = ChartCornerNotice::default();
        let loading = ChartSurfaceNotice {
            label: ChartState::Loading.label(),
            ..corner("", "repairing")
        };
        assert_eq!(notice.observe(Some(loading)), None);
        let centred = ChartSurfaceNotice {
            placement: ChartNoticePlacement::Center,
            ..corner("Chart unavailable", "no data")
        };
        assert_eq!(notice.observe(Some(centred)), None);
        assert!(notice.shown.is_none());
    }

    const PRICE_AXIS_WIDTH: f32 = 64.0;

    struct StackHarness(ChartCornerNotice);

    impl Render for StackHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().relative().size_full().children(corner_notice_stack(
                self.0.card(&AerisTheme::dark()).into_iter().collect(),
                PRICE_AXIS_WIDTH,
            ))
        }
    }

    #[gpui::test]
    fn the_stack_sits_top_right_clear_of_the_price_axis(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, cx| {
            gpui_base::init(cx);
            let mut notice = ChartCornerNotice::default();
            notice.observe(Some(corner("Chart unavailable", "provider error")));
            StackHarness(notice)
        });
        cx.run_until_parked();
        let viewport = cx.update(|window, _| window.viewport_size());
        let stack = cx
            .debug_bounds("corner_notice_stack")
            .expect("notice stack");
        assert_eq!(f32::from(stack.origin.y), NOTICE_GAP);
        let right_inset = f32::from(viewport.width - (stack.origin.x + stack.size.width));
        assert!(
            (right_inset - (PRICE_AXIS_WIDTH + NOTICE_GAP)).abs() <= 0.5,
            "the stack clears the price axis: {right_inset} from the right edge"
        );
    }
}
