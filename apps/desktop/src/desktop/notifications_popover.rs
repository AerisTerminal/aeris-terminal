//! The notifications popover beside the panel rail's bell: the session's announcement
//! history, the price alerts that have triggered and the alerts still running, across every
//! tab and pane. Alerts stay owned by their pane, so removing one asks that pane to delete it.

use aeris_localization::{drawing, panels, tr};
use gpui::Point;

use super::notification_feed::{FeedEntry, entry_wall_unix_nanos};
use super::price_alert_dialog::{condition_label, frequency_label};
use super::toaster::tone_icon;
use super::workflows_page::firing_time_text;
use super::*;

const POPOVER_WIDTH: f32 = 360.0;
const POPOVER_MAX_HEIGHT: f32 = 480.0;
/// The popover never shrinks below this, even beside a bell near the top of a short window.
const POPOVER_MIN_HEIGHT: f32 = 160.0;
/// Space between the bell and the popover.
const POPOVER_GAP: f32 = 8.0;
/// Half of the bell's `ButtonSize::Default` square, from its click point to its edges.
const BELL_HALF_EXTENT: f32 = 14.0;
const ROW_ICON_SIZE: f32 = 16.0;
const ROW_TITLE_LINE_HEIGHT: f32 = 20.0;

/// The sections of the notifications popover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationsTab {
    /// What the terminal announced this session.
    Feed,
    /// Price alerts that triggered and stopped.
    Triggered,
    /// Price alerts still watching the market.
    Running,
}

impl NotificationsTab {
    const ALL: [Self; 3] = [Self::Feed, Self::Triggered, Self::Running];

    fn label(self) -> &'static str {
        match self {
            Self::Feed => tr!(panels::NOTIFICATIONS),
            Self::Triggered => tr!(panels::NOTIFICATIONS_TAB_TRIGGERED),
            Self::Running => tr!(panels::NOTIFICATIONS_TAB_RUNNING),
        }
    }

    const fn alert_status(self) -> Option<PriceAlertStatus> {
        match self {
            Self::Feed => None,
            Self::Triggered => Some(PriceAlertStatus::Triggered),
            Self::Running => Some(PriceAlertStatus::Active),
        }
    }
}

/// A price alert the popover lists, with the pane that owns it.
struct ListedAlert {
    surface: Entity<WorkspaceSurface>,
    alert: WorkspacePriceAlertState,
}

/// Where the popover sits: its left edge, its distance from the window bottom and its
/// height cap. It opens left of the bell with its bottom beside the bell's, growing upward
/// and staying clear of the title bar and the window edges.
fn popover_frame(anchor: Point<Pixels>, viewport: gpui::Size<Pixels>) -> (Pixels, Pixels, Pixels) {
    let margin = px(OVERLAY_EDGE_MARGIN);
    let width = px(POPOVER_WIDTH);
    let left = (anchor.x - px(BELL_HALF_EXTENT + POPOVER_GAP) - width)
        .min(viewport.width - width - margin)
        .max(margin);
    let bottom = (viewport.height - anchor.y - px(BELL_HALF_EXTENT)).max(margin);
    let top_limit = px(WORKSPACE_TITLE_BAR_HEIGHT) + margin;
    let max_height = (viewport.height - bottom - top_limit)
        .min(px(POPOVER_MAX_HEIGHT))
        .max(px(POPOVER_MIN_HEIGHT));
    (left, bottom, max_height)
}

fn notifications_tabs(
    terminal: &Entity<TerminalApp>,
    shown: NotificationsTab,
    theme: &AerisTheme,
) -> TabList {
    let tabs = NotificationsTab::ALL
        .into_iter()
        .enumerate()
        .map(|(index, tab)| {
            let select = terminal.clone();
            Tab::new(("notifications_tab", index), theme)
                .compact()
                .selected(tab == shown)
                .aria_label(tab.label())
                .on_click(move |_, _, cx| {
                    select.update(cx, |terminal, terminal_cx| {
                        terminal.select_notifications_tab(tab, terminal_cx);
                    });
                })
                .child(tab.label())
        });
    TabList::new(
        "notifications_tabs",
        tr!(panels::NOTIFICATIONS_SECTIONS),
        theme,
    )
    .compact()
    .children(tabs)
}

fn notifications_header(
    terminal: &Entity<TerminalApp>,
    shown: NotificationsTab,
    clearable: bool,
    theme: &AerisTheme,
) -> Div {
    let clear = terminal.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_3()
        .pt_2()
        .child(notifications_tabs(terminal, shown, theme))
        .child(div().flex_1())
        .when(clearable, |header| {
            header.child(
                Button::new("notifications_clear", theme)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Sm)
                    .label(tr!(panels::NOTIFICATIONS_CLEAR))
                    .aria_label(tr!(panels::NOTIFICATIONS_CLEAR_ARIA))
                    .on_click(move |_, _, cx| {
                        clear.update(cx, TerminalApp::clear_notification_feed);
                    }),
            )
        })
}

fn empty_state(text: &'static str, theme: &AerisTheme) -> AnyElement {
    div()
        .px_2()
        .py_3()
        .text_sm()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(text)
        .into_any_element()
}

fn row_time(time: String, theme: &AerisTheme) -> Div {
    div()
        .flex_none()
        .text_xs()
        .line_height(px(ROW_TITLE_LINE_HEIGHT))
        .text_color(gpui_color(theme.colors.text_muted))
        .map(numeric_readout)
        .child(time)
}

fn feed_row(entry: &FeedEntry, time: String, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let (icon, icon_color) = tone_icon(entry.tone, theme);
    div()
        .flex()
        .items_start()
        .gap_2()
        .px_2()
        .py_1()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(ROW_TITLE_LINE_HEIGHT))
                .child(
                    header_icon(icon)
                        .with_size(px(ROW_ICON_SIZE))
                        .color(gpui_color(icon_color)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .line_height(px(ROW_TITLE_LINE_HEIGHT))
                                .text_color(gpui_color(colors.text_default))
                                .child(entry.title.clone()),
                        )
                        .child(row_time(time, theme)),
                )
                .children(entry.detail.clone().map(|detail| {
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_secondary))
                        .child(detail)
                })),
        )
        .into_any_element()
}

/// "Crossing · 31258.00 · Every time", in the instrument's exact price format.
fn alert_summary(alert: &WorkspacePriceAlertState) -> String {
    let condition =
        PriceAlertCondition::try_from(alert.condition).map_or(drawing::ALERT, condition_label);
    let price = alert.instrument.as_ref().map_or_else(
        || tr!(drawing::UNAVAILABLE).to_string(),
        |instrument| market_price_text(alert.price, instrument.price_scale),
    );
    match PriceAlertFrequency::try_from(alert.frequency) {
        Ok(frequency) => format!(
            "{} · {price} · {}",
            tr!(condition),
            tr!(frequency_label(frequency))
        ),
        Err(_) => format!("{} · {price}", tr!(condition)),
    }
}

/// When the alert last triggered, for a triggered alert or one that triggers every time.
fn alert_trigger_text(alert: &WorkspacePriceAlertState) -> Option<String> {
    let known = alert.triggered_at_unix_nanos > 0;
    if alert.status == PriceAlertStatus::Triggered as i32 {
        return Some(if known {
            tr!(
                panels::NOTIFICATIONS_TRIGGERED_AT,
                time = firing_time_text(alert.triggered_at_unix_nanos)
            )
        } else {
            tr!(panels::NOTIFICATIONS_TAB_TRIGGERED).to_string()
        });
    }
    let every_time = alert.frequency == PriceAlertFrequency::EveryTime as i32;
    (every_time && known).then(|| {
        tr!(
            panels::NOTIFICATIONS_LAST_TRIGGERED,
            time = firing_time_text(alert.triggered_at_unix_nanos)
        )
    })
}

/// One alert with a button that asks the pane owning it to delete it; the pane reports a
/// refused removal itself.
fn alert_row(index: usize, listed: ListedAlert, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let symbol = listed.alert.instrument.as_ref().map_or_else(
        || tr!(drawing::UNAVAILABLE).to_string(),
        |instrument| instrument.display_symbol.clone(),
    );
    let summary = alert_summary(&listed.alert);
    let trigger = alert_trigger_text(&listed.alert);
    let ListedAlert { surface, alert } = listed;
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .line_height(px(ROW_TITLE_LINE_HEIGHT))
                        .text_color(gpui_color(colors.text_default))
                        .child(symbol),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_secondary))
                        .font_features(platform_tabular_numerals())
                        .child(summary),
                )
                .children(trigger.map(|trigger| {
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .font_features(platform_tabular_numerals())
                        .child(trigger)
                })),
        )
        .child(
            Button::new(("notifications_remove_alert", index), theme)
                .variant(ButtonVariant::Ghost)
                .icon(header_icon(HugeIcon::Trash))
                .aria_label(tr!(drawing::DELETE_PRICE_ALERT))
                .danger_on_hover()
                .on_click(move |_, _, cx| {
                    surface.update(cx, |surface, surface_cx| {
                        surface.delete_price_alert(&alert.id, surface_cx);
                    });
                }),
        )
        .into_any_element()
}

/// The popover and the scrim that closes it, over the whole window.
fn notifications_layer(
    terminal: &Entity<TerminalApp>,
    anchor: Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    header: Div,
    rows: Vec<AnyElement>,
    theme: &AerisTheme,
) -> AnyElement {
    let (left, bottom, max_height) = popover_frame(anchor, viewport);
    let width = px(POPOVER_WIDTH);
    let animation_origin = PopupAnimationOrigin::from_trigger(
        anchor,
        Bounds::new(
            point(left, viewport.height - bottom - max_height),
            size(width, max_height),
        ),
    );
    let panel = MenuPanel::new("notifications_popover", MenuPlacement::InFlow, theme)
        .width(width)
        .max_height(max_height)
        .animate_from(animation_origin)
        .header(header)
        .children(rows);
    let dismiss = terminal.clone();
    let dismiss_right = terminal.clone();
    div()
        .id("notifications_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_click(move |_, _, cx| {
            dismiss.update(cx, TerminalApp::close_notifications);
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            dismiss_right.update(cx, TerminalApp::close_notifications);
            cx.stop_propagation();
        })
        .child(div().absolute().left(left).bottom(bottom).child(panel))
        .into_any_element()
}

impl TerminalApp {
    /// Opens the notifications popover beside the bell's click point, or closes it when
    /// open. Opening it, like closing it, reads everything announced so far.
    pub(super) fn toggle_notifications_at(
        &mut self,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.overlays.toggle_notifications(anchor);
        self.notices.toaster_mut().feed_mut().mark_read();
        cx.notify();
    }

    pub(super) fn close_notifications(&mut self, cx: &mut Context<Self>) {
        if self.overlays.close(OverlayKind::Notifications) {
            self.notices.toaster_mut().feed_mut().mark_read();
            cx.notify();
        }
    }

    pub(super) fn notifications_open(&self) -> bool {
        self.overlays.is_open(OverlayKind::Notifications)
    }

    /// Announcements the bell marks as unread; none while the popover shows them.
    pub(super) fn unread_notifications(&self) -> usize {
        if self.notifications_open() {
            0
        } else {
            self.notices.toaster().feed().unread()
        }
    }

    pub(super) fn select_notifications_tab(
        &mut self,
        tab: NotificationsTab,
        cx: &mut Context<Self>,
    ) {
        if self.overlays.select_notifications_tab(tab) {
            cx.notify();
        }
    }

    pub(super) fn clear_notification_feed(&mut self, cx: &mut Context<Self>) {
        if self.notices.toaster_mut().feed_mut().clear() {
            cx.notify();
        }
    }

    /// Every pane's alerts in `status`, across all tabs: triggered ones by when they
    /// triggered and running ones by when they were created, newest first.
    fn listed_alerts(&self, status: PriceAlertStatus, cx: &App) -> Vec<ListedAlert> {
        let mut listed: Vec<ListedAlert> = self
            .tabs
            .iter()
            .flat_map(|workspace| workspace.panes.iter())
            .flat_map(|pane| {
                pane.surface
                    .read(cx)
                    .alerts
                    .alerts()
                    .iter()
                    .filter(|alert| alert.status == status as i32)
                    .map(|alert| ListedAlert {
                        surface: pane.surface.clone(),
                        alert: alert.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        listed.sort_by_key(|listed| {
            std::cmp::Reverse(match status {
                PriceAlertStatus::Triggered => listed.alert.triggered_at_unix_nanos,
                PriceAlertStatus::Active => listed.alert.created_at_unix_nanos,
            })
        });
        listed
    }

    fn notification_rows(&self, tab: NotificationsTab, cx: &App) -> Vec<AnyElement> {
        let Some(status) = tab.alert_status() else {
            let feed = self.notices.toaster().feed();
            if feed.is_empty() {
                return vec![empty_state(
                    tr!(panels::NOTIFICATIONS_FEED_EMPTY),
                    &self.theme,
                )];
            }
            let now = Instant::now();
            let now_wall = self.lifecycle.clock().wall_unix_nanos();
            return feed
                .entries()
                .map(|entry| {
                    let time =
                        firing_time_text(entry_wall_unix_nanos(entry.recorded, now, now_wall));
                    feed_row(entry, time, &self.theme)
                })
                .collect();
        };
        let alerts = self.listed_alerts(status, cx);
        if alerts.is_empty() {
            let empty = match status {
                PriceAlertStatus::Triggered => tr!(panels::NOTIFICATIONS_TRIGGERED_EMPTY),
                PriceAlertStatus::Active => tr!(panels::NOTIFICATIONS_RUNNING_EMPTY),
            };
            return vec![empty_state(empty, &self.theme)];
        }
        alerts
            .into_iter()
            .enumerate()
            .map(|(index, listed)| alert_row(index, listed, &self.theme))
            .collect()
    }

    pub(super) fn notifications_overlay(
        &self,
        terminal: &Entity<Self>,
        viewport: gpui::Size<Pixels>,
        cx: &App,
    ) -> Option<AnyElement> {
        let (anchor, tab) = self.overlays.notifications()?;
        let clearable = tab == NotificationsTab::Feed && !self.notices.toaster().feed().is_empty();
        let header = notifications_header(terminal, tab, clearable, &self.theme);
        let rows = self.notification_rows(tab, cx);
        Some(notifications_layer(
            terminal,
            anchor,
            viewport,
            header,
            rows,
            &self.theme,
        ))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::*;

    fn alert(
        status: PriceAlertStatus,
        frequency: PriceAlertFrequency,
        triggered: i64,
    ) -> WorkspacePriceAlertState {
        WorkspacePriceAlertState {
            id: "alert".to_string(),
            instrument: None,
            price: 0,
            pane_index: 0,
            price_scale_side: 0,
            condition: PriceAlertCondition::Crossing as i32,
            frequency: frequency as i32,
            status: status as i32,
            created_at_unix_nanos: 0,
            triggered_at_unix_nanos: triggered,
        }
    }

    #[test]
    fn the_popover_opens_left_of_the_bell_and_stays_inside_the_window() {
        let viewport = size(px(1200.0), px(800.0));
        let (left, bottom, max_height) = popover_frame(point(px(1182.0), px(780.0)), viewport);
        assert_eq!(left, px(1182.0 - 14.0 - 8.0 - 360.0));
        assert_eq!(bottom, px(OVERLAY_EDGE_MARGIN));
        assert_eq!(max_height, px(POPOVER_MAX_HEIGHT));

        let narrow = size(px(300.0), px(240.0));
        let (left, bottom, max_height) = popover_frame(point(px(282.0), px(100.0)), narrow);
        assert_eq!(
            left,
            px(OVERLAY_EDGE_MARGIN),
            "a narrow window keeps the left edge"
        );
        assert_eq!(bottom, px(240.0 - 100.0 - 14.0));
        assert_eq!(max_height, px(POPOVER_MIN_HEIGHT));
    }

    #[test]
    fn alerts_show_when_they_last_triggered() {
        let at = 3_723_000_000_000;
        assert_eq!(
            alert_trigger_text(&alert(
                PriceAlertStatus::Triggered,
                PriceAlertFrequency::OnlyOnce,
                at
            )),
            Some(tr!(
                panels::NOTIFICATIONS_TRIGGERED_AT,
                time = "01:02:03 UTC"
            ))
        );
        assert_eq!(
            alert_trigger_text(&alert(
                PriceAlertStatus::Active,
                PriceAlertFrequency::EveryTime,
                at
            )),
            Some(tr!(
                panels::NOTIFICATIONS_LAST_TRIGGERED,
                time = "01:02:03 UTC"
            ))
        );
        assert_eq!(
            alert_trigger_text(&alert(
                PriceAlertStatus::Active,
                PriceAlertFrequency::EveryTime,
                0
            )),
            None,
            "an alert that never triggered has no time"
        );
        assert_eq!(
            alert_trigger_text(&alert(
                PriceAlertStatus::Triggered,
                PriceAlertFrequency::OnlyOnce,
                0
            )),
            Some(tr!(panels::NOTIFICATIONS_TAB_TRIGGERED).to_string()),
            "an alert triggered before times were kept says only that it triggered"
        );
    }
}
