use aeris_localization::{Text, common, drawing, tr};
use aeris_observability::diagnostic;

use super::alerts_controller::{AlertTriggerOutcome, chart_price_scale, scaled_price};
use super::*;

/// "NQZ6 crossed 31258.00" over "Last trade 31258.25", in the runtime's exact values.
pub(super) fn price_alert_trigger_text(trigger: &MarketPriceAlertTrigger) -> (String, String) {
    let scale = trigger.instrument.price_scale;
    (
        tr!(
            price_alert_condition_text(trigger.condition),
            symbol = trigger.instrument.display_symbol,
            price = market_price_text(trigger.threshold_price, scale)
        ),
        tr!(
            drawing::TRIGGER_LAST_TRADE,
            price = market_price_text(trigger.observed_price, scale)
        ),
    )
}

const fn price_alert_condition_text(condition: PriceAlertCondition) -> Text {
    match condition {
        PriceAlertCondition::Crossing => drawing::TRIGGER_CROSSING,
        PriceAlertCondition::CrossingUp => drawing::TRIGGER_CROSSING_UP,
        PriceAlertCondition::CrossingDown => drawing::TRIGGER_CROSSING_DOWN,
        PriceAlertCondition::GreaterThan => drawing::TRIGGER_GREATER_THAN,
        PriceAlertCondition::LessThan => drawing::TRIGGER_LESS_THAN,
    }
}

pub(super) fn runtime_price_alerts(alerts: &[WorkspacePriceAlertState]) -> Vec<MarketPriceAlert> {
    alerts.iter().filter_map(runtime_price_alert).collect()
}

fn runtime_price_alert(alert: &WorkspacePriceAlertState) -> Option<MarketPriceAlert> {
    Some(MarketPriceAlert {
        id: alert.id.clone(),
        instrument: alert.instrument.clone()?,
        price: alert.price,
        condition: PriceAlertCondition::try_from(alert.condition).ok()?,
        frequency: PriceAlertFrequency::try_from(alert.frequency).ok()?,
        active: PriceAlertStatus::try_from(alert.status).ok()? == PriceAlertStatus::Active,
    })
}

pub(super) fn replace_chart_price_alert_lines(
    chart: Option<&Entity<AerisChartView>>,
    alerts: &[WorkspacePriceAlertState],
    selected_instrument: Option<&InstallProviderInstrument>,
    cx: &mut Context<WorkspaceSurface>,
) {
    let (Some(chart), Some(selected)) = (chart, selected_instrument) else {
        return;
    };
    let lines = alerts
        .iter()
        .filter(|alert| {
            alert.instrument.as_ref().is_some_and(|instrument| {
                instrument.provider == selected.provider
                    && instrument.instrument_id == selected.instrument_id
                    && instrument.entitlement_id == selected.entitlement_id
            })
        })
        .filter_map(chart_alert_line)
        .collect();
    chart.update(cx, |chart, chart_cx| {
        if let Err(error) = chart.replace_price_alert_lines(ChartAlertSnapshot { lines }) {
            diagnostic!("Aeris chart alert indicators could not be installed: {error}");
        }
        chart_cx.notify();
    });
}

fn chart_alert_line(alert: &WorkspacePriceAlertState) -> Option<ChartAlertLine> {
    let instrument = alert.instrument.as_ref()?;
    let divisor = 10_f64.powi(i32::try_from(instrument.price_scale).ok()?);
    Some(ChartAlertLine {
        id: ChartAlertId::new(alert.id.clone()).ok()?,
        pane_index: usize::try_from(alert.pane_index).ok()?,
        price_scale: chart_price_scale(alert.price_scale_side)?,
        price: alert.price.to_f64()? / divisor,
        condition: chart_condition(PriceAlertCondition::try_from(alert.condition).ok()?),
        frequency: chart_frequency(PriceAlertFrequency::try_from(alert.frequency).ok()?),
        status: match PriceAlertStatus::try_from(alert.status).ok()? {
            PriceAlertStatus::Active => ChartAlertLineStatus::Active,
            PriceAlertStatus::Triggered => ChartAlertLineStatus::Triggered,
        },
        label: Some(instrument.display_symbol.clone()),
    })
}

const fn chart_condition(value: PriceAlertCondition) -> ChartAlertCondition {
    match value {
        PriceAlertCondition::Crossing => ChartAlertCondition::Crossing,
        PriceAlertCondition::CrossingUp => ChartAlertCondition::CrossingUp,
        PriceAlertCondition::CrossingDown => ChartAlertCondition::CrossingDown,
        PriceAlertCondition::GreaterThan => ChartAlertCondition::GreaterThan,
        PriceAlertCondition::LessThan => ChartAlertCondition::LessThan,
    }
}

const fn persisted_condition(value: ChartAlertCondition) -> PriceAlertCondition {
    match value {
        ChartAlertCondition::Crossing => PriceAlertCondition::Crossing,
        ChartAlertCondition::CrossingUp => PriceAlertCondition::CrossingUp,
        ChartAlertCondition::CrossingDown => PriceAlertCondition::CrossingDown,
        ChartAlertCondition::GreaterThan => PriceAlertCondition::GreaterThan,
        ChartAlertCondition::LessThan => PriceAlertCondition::LessThan,
    }
}

const fn chart_frequency(value: PriceAlertFrequency) -> ChartAlertFrequency {
    match value {
        PriceAlertFrequency::OnlyOnce => ChartAlertFrequency::OnlyOnce,
        PriceAlertFrequency::EveryTime => ChartAlertFrequency::EveryTime,
    }
}

impl WorkspaceSurface {
    pub(super) fn apply_current_price_alert_sync(
        &self,
        sequence: u64,
        succeeded: bool,
        cx: &mut Context<Self>,
    ) {
        if sequence == self.market_worker.latest_price_alert_sequence() {
            Self::apply_price_alert_sync(succeeded, cx);
        }
    }

    pub(super) fn emit_initial_price_alert_restore_notice(&mut self, cx: &mut Context<Self>) {
        if self.alerts.take_restore_failure() {
            Self::apply_price_alert_sync(false, cx);
        }
    }

    fn post_price_alert_action_notice(detail: &'static str, cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
            key: notification_center::NoticeKey {
                scope: cx.entity_id().as_u64(),
                kind: "price_alert_action",
            },
            severity: notification_center::NoticeSeverity::Warning,
            tone: notification_center::NoticeTone::Warning,
            source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
            title: tr!(drawing::ALERT_ACTION_FAILED),
            detail: detail.into(),
            action: None,
            lifetime: notification_center::NoticeLifetime::UntilCleared,
        }));
        cx.notify();
    }

    fn clear_price_alert_action_notice(cx: &mut Context<Self>) {
        cx.emit(PaneNoticeEvent::Clear(notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "price_alert_action",
        }));
    }

    pub(super) fn apply_price_alert_sync(succeeded: bool, cx: &mut Context<Self>) {
        let key = notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "price_alert_sync",
        };
        if succeeded {
            cx.emit(PaneNoticeEvent::Clear(key));
        } else {
            cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
                key,
                severity: notification_center::NoticeSeverity::Error,
                tone: notification_center::NoticeTone::Loss,
                source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
                title: tr!(drawing::ALERTS_UNAVAILABLE),
                detail: tr!(drawing::ALERTS_UNAVAILABLE_DETAIL).into(),
                action: None,
                lifetime: notification_center::NoticeLifetime::UntilCleared,
            }));
        }
        cx.notify();
    }

    pub(super) fn open_price_alert_dialog(
        &mut self,
        request: ChartAlertCreateRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(instrument) = self.product.clone() else {
            Self::post_price_alert_action_notice(tr!(drawing::SELECT_LIVE_MARKET), cx);
            return;
        };
        self.alerts.open_dialog(PriceAlertDialogState {
            instrument,
            condition: persisted_condition(request.condition),
            frequency: match request.frequency {
                ChartAlertFrequency::OnlyOnce
                | ChartAlertFrequency::OncePerBar
                | ChartAlertFrequency::OncePerBarClose
                | ChartAlertFrequency::OncePerMinute => PriceAlertFrequency::OnlyOnce,
                ChartAlertFrequency::EveryTime => PriceAlertFrequency::EveryTime,
            },
            request,
            open_dropdown: None,
        });
        Self::clear_price_alert_action_notice(cx);
    }

    pub(super) fn close_price_alert_dialog(&mut self, cx: &mut Context<Self>) {
        self.alerts.close_dialog();
        Self::clear_price_alert_action_notice(cx);
        cx.notify();
    }

    pub(super) fn select_price_alert_condition(
        &mut self,
        condition: PriceAlertCondition,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.alerts.dialog_mut() {
            dialog.condition = condition;
            dialog.open_dropdown = None;
            Self::clear_price_alert_action_notice(cx);
            cx.notify();
        }
    }

    pub(super) fn select_price_alert_frequency(
        &mut self,
        frequency: PriceAlertFrequency,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.alerts.dialog_mut() {
            dialog.frequency = frequency;
            dialog.open_dropdown = None;
            Self::clear_price_alert_action_notice(cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_price_alert_dropdown(
        &mut self,
        dropdown: PriceAlertDropdown,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.alerts.dialog_mut() {
            dialog.open_dropdown = (dialog.open_dropdown != Some(dropdown)).then_some(dropdown);
            cx.notify();
        }
    }

    pub(super) fn close_price_alert_dropdown(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.alerts.dialog_mut()
            && dialog.open_dropdown.take().is_some()
        {
            cx.notify();
        }
    }

    pub(super) fn create_price_alert(&mut self, cx: &mut Context<Self>) {
        let next = match self
            .alerts
            .with_dialog_alert(self.lifecycle.clock().wall_unix_nanos())
        {
            Ok(next) => next,
            Err(detail) => {
                Self::post_price_alert_action_notice(detail, cx);
                return;
            }
        };
        if self
            .market_worker
            .try_replace_price_alerts(runtime_price_alerts(&next))
            .is_err()
        {
            Self::post_price_alert_action_notice(tr!(drawing::ALERT_NOT_CREATED), cx);
            return;
        }
        self.alerts.adopt_created(next);
        Self::clear_price_alert_action_notice(cx);
        self.persistence.mark_dirty();
        replace_chart_price_alert_lines(
            self.chart.as_ref(),
            self.alerts.alerts(),
            self.product.as_ref(),
            cx,
        );
        cx.notify();
    }

    pub(super) fn delete_price_alert(&mut self, alert_id: &str, cx: &mut Context<Self>) {
        let Some(next) = self.alerts.without_alert(alert_id) else {
            return;
        };
        if self
            .market_worker
            .try_replace_price_alerts(runtime_price_alerts(&next))
            .is_err()
        {
            Self::post_price_alert_action_notice(tr!(drawing::ALERT_NOT_REMOVED), cx);
            return;
        }
        self.alerts.adopt(next);
        self.persistence.mark_dirty();
        Self::clear_price_alert_action_notice(cx);
        replace_chart_price_alert_lines(
            self.chart.as_ref(),
            self.alerts.alerts(),
            self.product.as_ref(),
            cx,
        );
        cx.notify();
    }

    pub(super) fn apply_price_alert_trigger(
        &mut self,
        trigger: &MarketPriceAlertTrigger,
        cx: &mut Context<Self>,
    ) {
        let (title, description) = price_alert_trigger_text(trigger);
        let outcome = self
            .alerts
            .apply_trigger(trigger, PriceAlertAnnouncement { title, description });
        if outcome == AlertTriggerOutcome::Unknown {
            return;
        }
        // Both outcomes changed the persisted alert: its status or its latest trigger time.
        self.persistence.mark_dirty();
        replace_chart_price_alert_lines(
            self.chart.as_ref(),
            self.alerts.alerts(),
            self.product.as_ref(),
            cx,
        );
        cx.notify();
    }

    pub(super) fn take_price_alert_announcements(&mut self) -> Vec<PriceAlertAnnouncement> {
        self.alerts.take_announcements()
    }
}

const CONDITIONS: [PriceAlertCondition; 5] = [
    PriceAlertCondition::Crossing,
    PriceAlertCondition::CrossingUp,
    PriceAlertCondition::CrossingDown,
    PriceAlertCondition::GreaterThan,
    PriceAlertCondition::LessThan,
];

pub(super) const fn condition_label(condition: PriceAlertCondition) -> Text {
    match condition {
        PriceAlertCondition::Crossing => drawing::CONDITION_CROSSING,
        PriceAlertCondition::CrossingUp => drawing::CONDITION_CROSSING_UP,
        PriceAlertCondition::CrossingDown => drawing::CONDITION_CROSSING_DOWN,
        PriceAlertCondition::GreaterThan => drawing::CONDITION_GREATER_THAN,
        PriceAlertCondition::LessThan => drawing::CONDITION_LESS_THAN,
    }
}

fn dropdown_trigger(
    app: &Entity<WorkspaceSurface>,
    dropdown: PriceAlertDropdown,
    id: &'static str,
    label: &'static str,
    open: bool,
    theme: &AerisTheme,
) -> Button {
    let toggle = app.clone();
    Button::new(id, theme)
        .variant(ButtonVariant::Secondary)
        .button_size(ButtonSize::Lg)
        .trigger()
        .full_width()
        .open(open)
        .aria_label(label)
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_price_alert_dropdown(dropdown, surface_cx);
            });
        })
}

/// A form select: the trigger plus, while open, its menu stretched under it.
fn select_field(
    app: &Entity<WorkspaceSurface>,
    id: &'static str,
    trigger: Button,
    menu: Option<MenuPanel>,
) -> MenuAnchor {
    let dismiss = app.clone();
    MenuAnchor::new(id, trigger)
        .full_width()
        .menu(menu)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_price_alert_dropdown);
        })
}

fn select_menu(id: &'static str, theme: &AerisTheme) -> MenuPanel {
    MenuPanel::new(
        id,
        MenuPlacement::Anchored {
            side: MenuSide::Below,
            align: MenuAlign::Stretch,
        },
        theme,
    )
    .animate_from(PopupAnimationOrigin::TOP_LEFT)
}

fn condition_dropdown(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    theme: &AerisTheme,
) -> MenuAnchor {
    let open = dialog.open_dropdown == Some(PriceAlertDropdown::Condition);
    let menu = open.then(|| {
        select_menu("price_alert_condition_menu", theme).children(CONDITIONS.into_iter().map(
            |condition| {
                let choose = app.clone();
                MenuRow::compact(
                    ("price_alert_condition_option", condition as u32),
                    tr!(condition_label(condition)),
                    theme,
                )
                .checked(dialog.condition == condition)
                .on_click(move |_, _, cx| {
                    choose.update(cx, |surface, surface_cx| {
                        surface.select_price_alert_condition(condition, surface_cx);
                    });
                })
            },
        ))
    });
    select_field(
        app,
        "price_alert_condition_field",
        dropdown_trigger(
            app,
            PriceAlertDropdown::Condition,
            "price_alert_condition_select",
            tr!(condition_label(dialog.condition)),
            open,
            theme,
        ),
        menu,
    )
}

pub(super) const fn frequency_label(frequency: PriceAlertFrequency) -> Text {
    match frequency {
        PriceAlertFrequency::OnlyOnce => drawing::FREQUENCY_ONLY_ONCE,
        PriceAlertFrequency::EveryTime => drawing::FREQUENCY_EVERY_TIME,
    }
}

fn frequency_dropdown(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    theme: &AerisTheme,
) -> MenuAnchor {
    const FREQUENCIES: [PriceAlertFrequency; 2] = [
        PriceAlertFrequency::OnlyOnce,
        PriceAlertFrequency::EveryTime,
    ];
    let open = dialog.open_dropdown == Some(PriceAlertDropdown::Frequency);
    let menu = open.then(|| {
        select_menu("price_alert_frequency_menu", theme).children(FREQUENCIES.into_iter().map(
            |frequency| {
                let choose = app.clone();
                MenuRow::compact(
                    ("price_alert_frequency_option", frequency as u32),
                    tr!(frequency_label(frequency)),
                    theme,
                )
                .checked(dialog.frequency == frequency)
                .on_click(move |_, _, cx| {
                    choose.update(cx, |surface, surface_cx| {
                        surface.select_price_alert_frequency(frequency, surface_cx);
                    });
                })
            },
        ))
    });
    select_field(
        app,
        "price_alert_frequency_field",
        dropdown_trigger(
            app,
            PriceAlertDropdown::Frequency,
            "price_alert_frequency_select",
            tr!(frequency_label(dialog.frequency)),
            open,
            theme,
        ),
        menu,
    )
}

/// "SYMBOL at PRICE", with the price in tabular numerals.
fn price_alert_dialog_subtitle(symbol: &str, price: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(tr!(drawing::ALERT_DIALOG_SUBTITLE, symbol = symbol))
        .child(div().map(numeric_readout).child(price.to_string()))
}

fn price_alert_dialog_body(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(tr!(drawing::CONDITION)),
        )
        .child(condition_dropdown(app, dialog, theme))
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(tr!(drawing::FREQUENCY)),
        )
        .child(frequency_dropdown(app, dialog, theme))
        .child(
            div()
                .px_3()
                .py_2()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .bg(gpui_color(field_surface(theme)))
                .text_sm()
                .text_color(gpui_color(colors.text_secondary))
                .child(tr!(drawing::MONITORING_NOTE)),
        )
        .into_any_element()
}

pub(super) fn price_alert_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    alerts: &[WorkspacePriceAlertState],
    theme: &AerisTheme,
) -> AnyElement {
    let dismiss = app.clone();
    let cancel = app.clone();
    let create = app.clone();
    let symbol = dialog.instrument.display_symbol.as_str();
    let price = scaled_price(dialog.request.price, dialog.instrument.price_scale).map_or_else(
        || tr!(drawing::UNAVAILABLE).to_string(),
        |price| market_price_text(price, dialog.instrument.price_scale),
    );
    Dialog::new("price_alert_dialog", DialogSize::Md, theme, move |_, cx| {
        dismiss.update(cx, WorkspaceSurface::close_price_alert_dialog);
    })
    .title(tr!(drawing::CREATE_PRICE_ALERT))
    .subtitle(price_alert_dialog_subtitle(symbol, &price))
    .child(price_alert_dialog_body(app, dialog, theme))
    .action(
        Button::new("price_alert_cancel", theme)
            .variant(ButtonVariant::Secondary)
            .label(tr!(common::CANCEL))
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_price_alert_dialog);
            }),
    )
    .action(
        Button::new("price_alert_create", theme)
            .variant(ButtonVariant::Primary)
            .icon(header_icon(HugeIcon::Add))
            .label(tr!(drawing::CREATE_ALERT))
            .disabled(alerts.len() >= MAXIMUM_PRICE_ALERTS_PER_CONSUMER)
            .on_click(move |_, _, cx| {
                create.update(cx, WorkspaceSurface::create_price_alert);
            }),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_and_persisted_conditions_round_trip() {
        for condition in CONDITIONS {
            assert_eq!(persisted_condition(chart_condition(condition)), condition);
        }
    }

    #[test]
    fn a_triggered_alert_reads_as_a_sentence_in_the_runtimes_exact_values() {
        let trigger = MarketPriceAlertTrigger {
            consumer_id: aeris_market_runtime::MarketConsumerId(std::num::NonZeroU64::MIN),
            alert_id: "alert".to_string(),
            instrument: InstallProviderInstrument {
                display_symbol: "NQZ6".to_string(),
                price_scale: 8,
                ..Default::default()
            },
            threshold_price: 3_125_800_000_000,
            observed_price: 3_125_825_000_000,
            condition: PriceAlertCondition::CrossingUp,
            frequency: PriceAlertFrequency::OnlyOnce,
            observed_unix_nanos: 1,
            remains_active: false,
        };
        let (title, description) = price_alert_trigger_text(&trigger);
        assert_eq!(title, "NQZ6 crossed up through 31258.00");
        assert_eq!(description, "Last trade 31258.25");
    }
}
