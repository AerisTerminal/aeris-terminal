use std::fmt::Write as _;

use super::*;

const PRICE_SCALE_RIGHT: u32 = 0;
const PRICE_SCALE_LEFT: u32 = 1;
const PRICE_SCALE_OVERLAY: u32 = 2;

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
    chart: Option<&Entity<NucleusChartView>>,
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
            eprintln!("Axiusflow chart alert indicators could not be installed: {error}");
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

const fn chart_price_scale(value: u32) -> Option<ChartAlertPriceScale> {
    match value {
        PRICE_SCALE_RIGHT => Some(ChartAlertPriceScale::Right),
        PRICE_SCALE_LEFT => Some(ChartAlertPriceScale::Left),
        PRICE_SCALE_OVERLAY => Some(ChartAlertPriceScale::Overlay),
        _ => None,
    }
}

const fn persisted_price_scale(value: ChartAlertPriceScale) -> u32 {
    match value {
        ChartAlertPriceScale::Right => PRICE_SCALE_RIGHT,
        ChartAlertPriceScale::Left => PRICE_SCALE_LEFT,
        ChartAlertPriceScale::Overlay => PRICE_SCALE_OVERLAY,
    }
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

fn scaled_price(price: f64, scale: u32) -> Option<i64> {
    if !price.is_finite() || scale > 18 {
        return None;
    }
    (price * 10_f64.powi(i32::try_from(scale).ok()?))
        .round()
        .to_i64()
}

fn fixed_price_text(value: i64, scale: u32) -> String {
    let scale = scale.min(18);
    if scale == 0 {
        return value.to_string();
    }
    let factor = 10_i128.pow(scale);
    let magnitude = i128::from(value).abs();
    let whole = magnitude / factor;
    let fraction = magnitude % factor;
    let sign = if value < 0 { "-" } else { "" };
    format!(
        "{sign}{whole}.{fraction:0width$}",
        width = usize::try_from(scale).unwrap_or(18)
    )
}

fn random_alert_id() -> Result<String, String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    let mut id = String::with_capacity(32);
    for byte in random {
        write!(&mut id, "{byte:02x}")
            .map_err(|_| "alert identity could not be built".to_string())?;
    }
    Ok(id)
}

fn new_alert_id(alerts: &[WorkspacePriceAlertState]) -> Result<String, String> {
    for _ in 0..4 {
        let id = random_alert_id()?;
        if alerts.iter().all(|alert| alert.id != id) {
            return Ok(id);
        }
    }
    Err("a unique alert identity could not be allocated".to_string())
}

fn now_unix_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

impl WorkspaceSurface {
    pub(super) fn open_price_alert_dialog(&mut self, request: ChartAlertCreateRequest) {
        let Some(instrument) = self.product.clone() else {
            self.price_alert_message =
                Some("Select a live market before creating an alert".to_string());
            return;
        };
        self.price_alert_dialog = Some(PriceAlertDialogState {
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
        self.price_alert_message = None;
    }

    pub(super) fn close_price_alert_dialog(&mut self, cx: &mut Context<Self>) {
        self.price_alert_dialog = None;
        self.price_alert_message = None;
        cx.notify();
    }

    pub(super) fn select_price_alert_condition(
        &mut self,
        condition: PriceAlertCondition,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.price_alert_dialog {
            dialog.condition = condition;
            dialog.open_dropdown = None;
            self.price_alert_message = None;
            cx.notify();
        }
    }

    pub(super) fn select_price_alert_frequency(
        &mut self,
        frequency: PriceAlertFrequency,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.price_alert_dialog {
            dialog.frequency = frequency;
            dialog.open_dropdown = None;
            self.price_alert_message = None;
            cx.notify();
        }
    }

    pub(super) fn toggle_price_alert_dropdown(
        &mut self,
        dropdown: PriceAlertDropdown,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.price_alert_dialog {
            dialog.open_dropdown = (dialog.open_dropdown != Some(dropdown)).then_some(dropdown);
            cx.notify();
        }
    }

    pub(super) fn close_price_alert_dropdown(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.price_alert_dialog
            && dialog.open_dropdown.take().is_some()
        {
            cx.notify();
        }
    }

    pub(super) fn create_price_alert(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.price_alert_dialog.clone() else {
            self.price_alert_message =
                Some("Select a live market before creating an alert".to_string());
            cx.notify();
            return;
        };
        let instrument = dialog.instrument;
        if self.price_alerts.len() >= MAXIMUM_PRICE_ALERTS_PER_CONSUMER {
            self.price_alert_message = Some(format!(
                "A chart supports at most {MAXIMUM_PRICE_ALERTS_PER_CONSUMER} alerts"
            ));
            cx.notify();
            return;
        }
        let Some(price) = scaled_price(dialog.request.price, instrument.price_scale) else {
            self.price_alert_message = Some("The selected alert price is invalid".to_string());
            cx.notify();
            return;
        };
        let id = match new_alert_id(&self.price_alerts) {
            Ok(id) => id,
            Err(error) => {
                self.price_alert_message = Some(error);
                cx.notify();
                return;
            }
        };
        let Ok(pane_index) = u32::try_from(dialog.request.pane_index) else {
            self.price_alert_message = Some("The selected chart pane is invalid".to_string());
            cx.notify();
            return;
        };
        let alert = WorkspacePriceAlertState {
            id,
            instrument: Some(instrument.clone()),
            price,
            pane_index,
            price_scale_side: persisted_price_scale(dialog.request.price_scale),
            condition: dialog.condition as i32,
            frequency: dialog.frequency as i32,
            status: PriceAlertStatus::Active as i32,
            created_at_unix_nanos: now_unix_nanos(),
        };
        let mut next = self.price_alerts.clone();
        next.push(alert);
        if self
            .market_worker
            .try_replace_price_alerts(runtime_price_alerts(&next))
            .is_err()
        {
            self.price_alert_message =
                Some("Market data is unavailable; alert was not created".to_string());
            cx.notify();
            return;
        }
        self.price_alerts = next;
        self.price_alert_dialog = None;
        self.price_alert_message = None;
        self.chart_persistence_dirty = true;
        replace_chart_price_alert_lines(
            self.chart.as_ref(),
            &self.price_alerts,
            self.product.as_ref(),
            cx,
        );
        cx.notify();
    }

    pub(super) fn delete_price_alert(&mut self, alert_id: &str, cx: &mut Context<Self>) {
        let mut next = self.price_alerts.clone();
        next.retain(|alert| alert.id != alert_id);
        if next.len() == self.price_alerts.len() {
            return;
        }
        if self
            .market_worker
            .try_replace_price_alerts(runtime_price_alerts(&next))
            .is_err()
        {
            self.price_alert_message =
                Some("Market data is unavailable; alert was not removed".to_string());
            cx.notify();
            return;
        }
        self.price_alerts = next;
        self.chart_persistence_dirty = true;
        self.price_alert_message = None;
        replace_chart_price_alert_lines(
            self.chart.as_ref(),
            &self.price_alerts,
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
        if let Some(alert) = self
            .price_alerts
            .iter_mut()
            .find(|alert| alert.id == trigger.alert_id)
        {
            if !trigger.remains_active {
                alert.status = PriceAlertStatus::Triggered as i32;
                self.chart_persistence_dirty = true;
            }
            self.price_alert_message = Some(format!(
                "{} alert triggered at {}",
                trigger.instrument.display_symbol,
                fixed_price_text(trigger.observed_price, trigger.instrument.price_scale)
            ));
            replace_chart_price_alert_lines(
                self.chart.as_ref(),
                &self.price_alerts,
                self.product.as_ref(),
                cx,
            );
            cx.notify();
        }
    }
}

const CONDITIONS: [PriceAlertCondition; 5] = [
    PriceAlertCondition::Crossing,
    PriceAlertCondition::CrossingUp,
    PriceAlertCondition::CrossingDown,
    PriceAlertCondition::GreaterThan,
    PriceAlertCondition::LessThan,
];

const fn condition_label(condition: PriceAlertCondition) -> &'static str {
    match condition {
        PriceAlertCondition::Crossing => "Crossing",
        PriceAlertCondition::CrossingUp => "Crossing up",
        PriceAlertCondition::CrossingDown => "Crossing down",
        PriceAlertCondition::GreaterThan => "Greater than (>)",
        PriceAlertCondition::LessThan => "Less than (<)",
    }
}

fn dropdown_trigger(
    app: &Entity<WorkspaceSurface>,
    dropdown: PriceAlertDropdown,
    id: &'static str,
    label: &'static str,
    open: bool,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    let toggle = app.clone();
    let colors = theme.colors;
    div()
        .id(id)
        .w_full()
        .h(px(34.0))
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_1()
        .border_color(gpui_color(if open {
            colors.ring
        } else {
            colors.input_border
        }))
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .bg(gpui_color(colors.input_fill))
        .text_sm()
        .text_color(gpui_color(colors.text_primary))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(label)
        .hover(move |style| style.bg(gpui_color(colors.hover_bg.over(colors.input_fill))))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_price_alert_dropdown(dropdown, surface_cx);
            });
            cx.stop_propagation();
        })
        .child(label)
        .child(header_icon(HugeIcon::ChevronDown).with_size(px(14.0)))
}

fn dropdown_panel(id: &'static str, theme: &AxiusflowTheme) -> Stateful<Div> {
    let colors = theme.colors;
    div()
        .id(id)
        .absolute()
        .top(px(38.0))
        .left_0()
        .right_0()
        .occlude()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .shadow_md()
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
}

fn condition_dropdown(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    theme: &AxiusflowTheme,
) -> Div {
    let open = dialog.open_dropdown == Some(PriceAlertDropdown::Condition);
    let mut field = div()
        .w_full()
        .relative()
        .flex_none()
        .child(dropdown_trigger(
            app,
            PriceAlertDropdown::Condition,
            "price_alert_condition_select",
            condition_label(dialog.condition),
            open,
            theme,
        ));
    if open {
        let mut menu = dropdown_panel("price_alert_condition_menu", theme);
        for (index, condition) in CONDITIONS.into_iter().enumerate() {
            let choose = app.clone();
            menu = menu.child(
                MenuRow::compact(
                    ("price_alert_condition_option", condition as u32),
                    condition_label(condition),
                    theme,
                )
                .highlighted(dialog.condition == condition)
                .flush_in_panel(index == 0, index + 1 == CONDITIONS.len())
                .on_click(move |_, _, cx| {
                    choose.update(cx, |surface, surface_cx| {
                        surface.select_price_alert_condition(condition, surface_cx);
                    });
                }),
            );
        }
        field = field.child(gpui::deferred(menu));
    }
    field
}

const fn frequency_label(frequency: PriceAlertFrequency) -> &'static str {
    match frequency {
        PriceAlertFrequency::OnlyOnce => "Only once",
        PriceAlertFrequency::EveryTime => "Every time",
    }
}

fn frequency_dropdown(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    theme: &AxiusflowTheme,
) -> Div {
    const FREQUENCIES: [PriceAlertFrequency; 2] = [
        PriceAlertFrequency::OnlyOnce,
        PriceAlertFrequency::EveryTime,
    ];
    let open = dialog.open_dropdown == Some(PriceAlertDropdown::Frequency);
    let mut field = div()
        .w_full()
        .relative()
        .flex_none()
        .child(dropdown_trigger(
            app,
            PriceAlertDropdown::Frequency,
            "price_alert_frequency_select",
            frequency_label(dialog.frequency),
            open,
            theme,
        ));
    if open {
        let mut menu = dropdown_panel("price_alert_frequency_menu", theme);
        for (index, frequency) in FREQUENCIES.into_iter().enumerate() {
            let choose = app.clone();
            menu = menu.child(
                MenuRow::compact(
                    ("price_alert_frequency_option", frequency as u32),
                    frequency_label(frequency),
                    theme,
                )
                .highlighted(dialog.frequency == frequency)
                .flush_in_panel(index == 0, index + 1 == FREQUENCIES.len())
                .on_click(move |_, _, cx| {
                    choose.update(cx, |surface, surface_cx| {
                        surface.select_price_alert_frequency(frequency, surface_cx);
                    });
                }),
            );
        }
        field = field.child(gpui::deferred(menu));
    }
    field
}

fn alerts_for_instrument(
    alerts: &[WorkspacePriceAlertState],
    instrument: &InstallProviderInstrument,
) -> Vec<WorkspacePriceAlertState> {
    alerts
        .iter()
        .filter(|alert| {
            alert.instrument.as_ref().is_some_and(|candidate| {
                candidate.provider == instrument.provider
                    && candidate.instrument_id == instrument.instrument_id
                    && candidate.entitlement_id == instrument.entitlement_id
            })
        })
        .cloned()
        .collect()
}

fn price_alert_existing_rows(
    app: &Entity<WorkspaceSurface>,
    existing: &[WorkspacePriceAlertState],
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let mut rows = div().flex().flex_col().gap_1();
    for (index, alert) in existing.iter().enumerate() {
        let remove = app.clone();
        let id = alert.id.clone();
        let condition =
            PriceAlertCondition::try_from(alert.condition).map_or("Alert", condition_label);
        let alert_price = alert.instrument.as_ref().map_or_else(
            || "Unavailable".to_string(),
            |value| fixed_price_text(alert.price, value.price_scale),
        );
        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .bg(gpui_color(colors.surface))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_sm()
                        .text_color(gpui_color(colors.text_primary))
                        .child(format!("{condition} ·"))
                        .child(
                            div()
                                .font_family(axiusflow_design_system::platform_font_family())
                                .font_features(platform_tabular_numerals())
                                .child(alert_price),
                        ),
                )
                .child(
                    Button::new(("delete_price_alert", index))
                        .theme(theme)
                        .with_size(px(28.0))
                        .resting_fill(colors.surface)
                        .icon(header_icon(HugeIcon::DeleteIcon02))
                        .aria_label("Delete price alert")
                        .on_click(move |_, _, cx| {
                            remove.update(cx, |surface, surface_cx| {
                                surface.delete_price_alert(&id, surface_cx);
                            });
                        }),
                ),
        );
    }
    rows.into_any_element()
}

fn price_alert_dialog_header(
    close: Entity<WorkspaceSurface>,
    symbol: &str,
    price: &str,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child("Create price alert"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(format!("{symbol} at"))
                        .child(
                            div()
                                .font_family(axiusflow_design_system::platform_font_family())
                                .font_features(platform_tabular_numerals())
                                .child(price.to_string()),
                        ),
                ),
        )
        .child(
            Button::new("price_alert_close")
                .theme(theme)
                .with_size(px(WORKSPACE_TAB_ICON_HIT))
                .rounded_full()
                .resting_fill(colors.surface_secondary)
                .icon(header_icon(HugeIcon::CancelIcon01))
                .aria_label("Close price alert dialog")
                .on_click(move |_, _, cx| {
                    close.update(cx, WorkspaceSurface::close_price_alert_dialog);
                }),
        )
        .into_any_element()
}

fn price_alert_dialog_footer(
    cancel: Entity<WorkspaceSurface>,
    create: Entity<WorkspaceSurface>,
    capacity_reached: bool,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .justify_end()
        .gap_2()
        .p_4()
        .border_t_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            Button::new("price_alert_cancel")
                .theme(theme)
                .resting_fill(colors.surface_secondary)
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, WorkspaceSurface::close_price_alert_dialog);
                }),
        )
        .child(
            Button::new("price_alert_create")
                .theme(theme)
                .resting_fill(colors.surface)
                .icon(header_icon(HugeIcon::AddIcon01))
                .label("Create alert")
                .disabled(capacity_reached)
                .on_click(move |_, _, cx| {
                    create.update(cx, WorkspaceSurface::create_price_alert);
                }),
        )
        .into_any_element()
}

fn price_alert_dialog_body(
    app: &Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    existing: &[WorkspacePriceAlertState],
    message: Option<&str>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .id("price_alert_dialog_body")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child("Condition"),
        )
        .child(
            condition_dropdown(app, dialog, theme),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child("Frequency"),
        )
        .child(
            frequency_dropdown(app, dialog, theme),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .bg(gpui_color(colors.surface))
                .text_sm()
                .text_color(gpui_color(colors.text_secondary))
                .child("Axiusflow monitors accepted live trades and sends an operating-system notification while the desktop app is running."),
        )
        .children(message.map(|message| {
            div()
                .text_sm()
                .text_color(gpui_color(colors.danger))
                .child(message.to_string())
        }))
        .children((!existing.is_empty()).then(|| {
            div()
                .mt_2()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child("Existing alerts for this market"),
                )
                .child(
                    div()
                        .id("price_alert_existing_scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(price_alert_existing_rows(app, existing, theme)),
                )
        }))
        .into_any_element()
}

pub(super) fn price_alert_dialog_layer(
    app: Entity<WorkspaceSurface>,
    dialog: &PriceAlertDialogState,
    alerts: &[WorkspacePriceAlertState],
    message: Option<&str>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let dismiss_dropdown = app.clone();
    let cancel = app.clone();
    let create = app.clone();
    let symbol = dialog.instrument.display_symbol.as_str();
    let price = scaled_price(dialog.request.price, dialog.instrument.price_scale).map_or_else(
        || "Unavailable".to_string(),
        |price| fixed_price_text(price, dialog.instrument.price_scale),
    );
    let existing = alerts_for_instrument(alerts, &dialog.instrument);

    div()
        .id("price_alert_dialog_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_color(colors.surface.with_alpha(0.72)))
        .on_any_mouse_down(move |_, _, cx| {
            dismiss.update(cx, WorkspaceSurface::close_price_alert_dialog);
            cx.stop_propagation();
        })
        .child(
            div()
                .id("price_alert_dialog")
                .w(px(460.0))
                .max_h(px(600.0))
                .flex()
                .flex_col()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border_secondary))
                .bg(gpui_color(colors.surface))
                .shadow_lg()
                .on_any_mouse_down(move |_, _, cx| {
                    dismiss_dropdown.update(cx, WorkspaceSurface::close_price_alert_dropdown);
                    cx.stop_propagation();
                })
                .child(price_alert_dialog_header(cancel, symbol, &price, theme))
                .child(price_alert_dialog_body(
                    &app, dialog, &existing, message, theme,
                ))
                .child(price_alert_dialog_footer(
                    app,
                    create,
                    alerts.len() >= MAXIMUM_PRICE_ALERTS_PER_CONSUMER,
                    theme,
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_price_formatting_preserves_scale_and_negative_values() {
        assert_eq!(fixed_price_text(6_700_075_000_000, 8), "67000.75000000");
        assert_eq!(fixed_price_text(-125, 2), "-1.25");
    }

    #[test]
    fn chart_and_persisted_conditions_round_trip() {
        for condition in CONDITIONS {
            assert_eq!(persisted_condition(chart_condition(condition)), condition);
        }
    }
}
