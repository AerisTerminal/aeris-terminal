use super::{
    AerisTheme, AnyElement, Button, ButtonSize, ButtonVariant, Context,
    ContextCredentialDialogState, ContextPanelTab, ContextSnapshot, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, ScrollHandle, StatefulInteractiveElement, Styled, Tab,
    TabList, Window, WorkspaceSurface, chrome_tooltip, close_button, div, gpui_color,
    platform_tabular_numerals, px,
};
use crate::desktop::native_ui::input::Input;
use aeris_context_runtime::{
    ContextMetric, ContextProvenance, ContextSource, CotPosition, DegreeDayMetric, EconomicEvent,
    SourceAvailability,
};
use chrono::{TimeZone, Utc};
use gpui::{AppContext as _, Div, prelude::FluentBuilder as _};
use std::time::{SystemTime, UNIX_EPOCH};

const CONTEXT_PANEL_HEADER_HEIGHT: f32 = 30.0;
const CONTEXT_PANEL_RESIZE_HANDLE_HEIGHT: f32 = 6.0;

/// Drag payload for resizing the bottom context panel from its top edge.
#[derive(Clone)]
pub(super) struct ContextPanelHeightDrag;

impl Render for ContextPanelHeightDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

pub(super) struct ContextPanelState<'a> {
    pub(super) app: Entity<WorkspaceSurface>,
    pub(super) height: f32,
    pub(super) snapshot: &'a ContextSnapshot,
    pub(super) tab: ContextPanelTab,
    pub(super) scroll: ScrollHandle,
    pub(super) credential_dialog: Option<&'a ContextCredentialDialogState>,
    pub(super) credential_message: Option<&'a str>,
    pub(super) risk_message: Option<&'a str>,
    pub(super) theme: &'a AerisTheme,
}

pub(super) fn context_panel(state: ContextPanelState<'_>) -> impl IntoElement + use<> {
    let ContextPanelState {
        app,
        height,
        snapshot,
        tab,
        scroll,
        credential_dialog,
        credential_message,
        risk_message,
        theme,
    } = state;
    let rows = context_rows(snapshot, tab, theme);
    let close_app = app.clone();
    let credentials_app = app.clone();
    let tabs = ContextPanelTab::ALL
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| context_tab(app.clone(), index, candidate, tab, theme));
    div()
        .id("context_panel")
        .relative()
        .h(px(height))
        .flex_none()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_t_1()
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.surface))
        .child(
            div()
                .h(px(CONTEXT_PANEL_HEADER_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .border_b_1()
                .border_color(gpui_color(theme.colors.border))
                .child(TabList::new("context_panel_tabs", "Market context", theme).children(tabs))
                .child(div().flex_1())
                .child(
                    Button::new("context_credentials", theme)
                        .button_size(ButtonSize::Sm)
                        .label("API keys")
                        .aria_label("Configure official context API keys")
                        .on_click(move |_, window, cx| {
                            credentials_app.update(cx, |surface, surface_cx| {
                                surface.toggle_context_credential_dialog(window, surface_cx);
                            });
                        }),
                )
                .child(chrome_tooltip(
                    "close_context_panel",
                    "Close market context",
                    close_button("close_context_panel", theme, move |_, cx| {
                        close_app.update(cx, |surface, surface_cx| {
                            surface.set_context_panel_visible(false, surface_cx);
                        });
                    }),
                    theme,
                )),
        )
        .child(match credential_dialog {
            Some(dialog) => credential_editor(app, dialog, credential_message, theme),
            None => div()
                .id("context_panel_rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .children(rows)
                .into_any_element(),
        })
        .children(risk_message.map(|message| {
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(gpui_color(theme.colors.warning))
                .child(message.to_string())
        }))
        .child(source_status_bar(snapshot, theme))
        .child(context_panel_resize_handle())
}

/// Top-edge handle; the center column owns the drag and converts it to a height.
fn context_panel_resize_handle() -> impl IntoElement {
    div()
        .id("context_panel_resize")
        .absolute()
        .occlude()
        .top_0()
        .left_0()
        .w_full()
        .h(px(CONTEXT_PANEL_RESIZE_HANDLE_HEIGHT))
        .cursor_row_resize()
        .on_drag(ContextPanelHeightDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
}

fn credential_editor(
    app: Entity<WorkspaceSurface>,
    dialog: &ContextCredentialDialogState,
    message: Option<&str>,
    theme: &AerisTheme,
) -> AnyElement {
    let fields = dialog
        .inputs
        .iter()
        .enumerate()
        .map(|(index, (source, input))| {
            let label = match source {
                ContextSource::Eia => "EIA key",
                ContextSource::Usda => "USDA NASS key",
                ContextSource::UsdaFas => "USDA FAS / api.data.gov key",
                ContextSource::Fred => "FRED key",
                _ => source.label(),
            };
            div()
                .id(("context_credential_row", index))
                .w(px(300.0))
                .flex()
                .flex_col()
                .gap_1()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(label)
                .child(Input::new(input).platform(theme).flex_1())
        });
    let save_app = app.clone();
    let cancel_app = app;
    div()
        .id("context_credential_editor")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child("Keys are write-only, masked, and stored in the operating-system credential vault. Blank fields leave existing keys unchanged."),
        )
        .child(div().flex().gap_2().children(fields))
        .when_some(message, |editor, message| {
            editor.child(
                div()
                    .text_xs()
                    .text_color(gpui_color(theme.colors.text_muted))
                    .child(message.to_string()),
            )
        })
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel_context_credentials", theme)
                        .variant(ButtonVariant::Secondary)
                        .button_size(ButtonSize::Sm)
                        .label("Cancel")
                        .on_click(move |_, window, cx| {
                            cancel_app.update(cx, |surface, surface_cx| {
                                surface.toggle_context_credential_dialog(window, surface_cx);
                            });
                        }),
                )
                .child(
                    Button::new("save_context_credentials", theme)
                        .variant(ButtonVariant::Filled)
                        .button_size(ButtonSize::Sm)
                        .label("Save")
                        .on_click(move |_, window, cx| {
                            save_app.update(cx, |surface, surface_cx| {
                                surface.save_context_api_keys(window, surface_cx);
                            });
                        }),
                ),
        )
        .into_any_element()
}

fn context_tab(
    app: Entity<WorkspaceSurface>,
    index: usize,
    candidate: ContextPanelTab,
    selected: ContextPanelTab,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    Tab::new(("context_panel_tab", index), theme)
        .selected(candidate == selected)
        .aria_label(format!("Show {} context", candidate.label()))
        .on_click(move |_, _, cx| {
            app.update(cx, |surface, surface_cx| {
                surface.set_context_panel_tab(candidate, surface_cx);
            });
        })
        .child(candidate.label())
}

fn context_rows(snapshot: &ContextSnapshot, tab: ContextPanelTab, theme: &AerisTheme) -> Vec<Div> {
    let mut rows = match tab {
        ContextPanelTab::Calendar => calendar_rows(&snapshot.economic_events, theme),
        ContextPanelTab::Energy => energy_rows(&snapshot.energy, &snapshot.weather, theme),
        ContextPanelTab::Commitments => cot_rows(&snapshot.commitments, theme),
        ContextPanelTab::Agriculture => metric_rows(&snapshot.agriculture, theme),
        ContextPanelTab::Macro => metric_rows(&snapshot.macro_observations, theme),
    };
    if rows.is_empty() {
        rows.push(empty_row(theme));
    }
    rows
}

fn calendar_rows(events: &[EconomicEvent], theme: &AerisTheme) -> Vec<Div> {
    let now = now_unix_seconds();
    events
        .iter()
        .filter(|event| event.scheduled_unix_seconds >= now.saturating_sub(86_400))
        .take(128)
        .map(|event| {
            let countdown = countdown(event.scheduled_unix_seconds, now);
            let importance = match event.importance {
                aeris_context_runtime::EventImportance::High => "HIGH",
                aeris_context_runtime::EventImportance::Medium => "MED",
                aeris_context_runtime::EventImportance::Low => "LOW",
            };
            context_row(
                format!("{importance} · {}", event.title),
                format!(
                    "{} · {countdown}",
                    format_timestamp(event.scheduled_unix_seconds)
                ),
                &event.provenance,
                theme,
            )
        })
        .collect()
}

fn energy_rows(
    energy: &[ContextMetric],
    weather: &[DegreeDayMetric],
    theme: &AerisTheme,
) -> Vec<Div> {
    let mut rows = metric_rows(energy, theme);
    rows.extend(weather.iter().take(32).map(|metric| {
        context_row(
            format!("NOAA degree days · {}", metric.region),
            format!(
                "HDD {} · CDD {} · {}",
                fixed_value(i128::from(metric.heating_degree_days_units), metric.scale),
                fixed_value(i128::from(metric.cooling_degree_days_units), metric.scale),
                metric.period
            ),
            &metric.provenance,
            theme,
        )
    }));
    rows
}

fn metric_rows(metrics: &[ContextMetric], theme: &AerisTheme) -> Vec<Div> {
    metrics
        .iter()
        .rev()
        .take(256)
        .map(|metric| {
            let range = metric
                .five_year_min_units
                .zip(metric.five_year_max_units)
                .map_or_else(String::new, |(minimum, maximum)| {
                    format!(
                        " · 5y {}–{}",
                        fixed_value(minimum, metric.value_scale),
                        fixed_value(maximum, metric.value_scale)
                    )
                });
            let surprise = metric.expected_units.map_or_else(String::new, |expected| {
                format!(
                    " · surprise {}",
                    fixed_value(
                        metric.value_units.saturating_sub(expected),
                        metric.value_scale
                    )
                )
            });
            context_row(
                metric.label.clone(),
                format!(
                    "{} {} · {}{range}{surprise}",
                    fixed_value(metric.value_units, metric.value_scale),
                    metric.unit,
                    metric.period
                ),
                &metric.provenance,
                theme,
            )
        })
        .collect()
}

fn cot_rows(positions: &[CotPosition], theme: &AerisTheme) -> Vec<Div> {
    positions
        .iter()
        .rev()
        .take(256)
        .map(|position| {
            let net = position
                .long_contracts
                .saturating_sub(position.short_contracts);
            context_row(
                format!("{} · {}", position.market_name, position.category),
                format!(
                    "Long {} · Short {} · Net {net:+} · OI {} · {}",
                    position.long_contracts,
                    position.short_contracts,
                    position.open_interest,
                    format_timestamp(position.report_date_unix_seconds)
                ),
                &position.provenance,
                theme,
            )
        })
        .collect()
}

fn context_row(
    title: String,
    detail: String,
    provenance: &ContextProvenance,
    theme: &AerisTheme,
) -> Div {
    div()
        .min_h(px(42.0))
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(theme.colors.text_primary))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(theme.colors.text_muted))
                        .font_features(platform_tabular_numerals())
                        .child(detail),
                ),
        )
        .child(
            div()
                .ml_2()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(format!(
                    "{} · known {}",
                    provenance.source.label(),
                    format_timestamp(provenance.release_unix_seconds)
                )),
        )
}

fn empty_row(theme: &AerisTheme) -> Div {
    div()
        .h(px(56.0))
        .px_2()
        .flex()
        .items_center()
        .text_sm()
        .text_color(gpui_color(theme.colors.text_muted))
        .child("No released values are available for this view.")
}

fn source_status_bar(snapshot: &ContextSnapshot, theme: &AerisTheme) -> Div {
    let statuses = snapshot.source_statuses.iter().map(|status| {
        let (label, color) = match status.availability {
            SourceAvailability::Pending => ("pending", theme.colors.text_muted),
            SourceAvailability::Available => ("ready", theme.colors.text_positive),
            SourceAvailability::MissingCredential => ("key required", theme.colors.warning),
            SourceAvailability::Unavailable => ("unavailable", theme.colors.text_negative),
        };
        div()
            .text_xs()
            .text_color(gpui_color(color))
            .child(format!("{} {label}", status.source.label()))
    });
    div()
        .h(px(26.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .border_t_1()
        .border_color(gpui_color(theme.colors.border))
        .children(statuses)
}

fn fixed_value(units: i128, scale: u8) -> String {
    if scale == 0 {
        return units.to_string();
    }
    let factor = 10_i128.pow(u32::from(scale));
    let magnitude = units.abs();
    let sign = if units < 0 { "-" } else { "" };
    format!(
        "{sign}{}.{:0width$}",
        magnitude / factor,
        magnitude % factor,
        width = usize::from(scale)
    )
}

fn format_timestamp(unix_seconds: i64) -> String {
    Utc.timestamp_opt(unix_seconds, 0).single().map_or_else(
        || "invalid time".to_string(),
        |time| time.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

fn countdown(event_unix_seconds: i64, now_unix_seconds: i64) -> String {
    let remaining = event_unix_seconds.saturating_sub(now_unix_seconds);
    if remaining <= 0 {
        return "released".to_string();
    }
    let days = remaining / 86_400;
    let hours = remaining % 86_400 / 3_600;
    let minutes = remaining % 3_600 / 60;
    if days > 0 {
        format!("in {days}d {hours}h")
    } else if hours > 0 {
        format!("in {hours}h {minutes}m")
    } else {
        format!("in {minutes}m")
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_context_values_preserve_scale() {
        assert_eq!(fixed_value(12_340, 3), "12.340");
        assert_eq!(fixed_value(-5, 2), "-0.05");
    }

    #[test]
    fn countdown_is_stable_at_release_boundary() {
        assert_eq!(countdown(1_000, 1_000), "released");
        assert_eq!(countdown(1_000 + 90 * 60, 1_000), "in 1h 30m");
    }
}
