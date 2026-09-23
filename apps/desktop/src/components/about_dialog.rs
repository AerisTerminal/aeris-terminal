use super::*;

#[derive(Clone, Copy)]
enum AboutAction {
    Restart,
    Retry,
}

struct AboutUpdateView {
    status: String,
    status_color: ThemeColor,
    action: Option<AboutAction>,
}

fn about_update_view(update: Option<&UpdatePresentation>, theme: &AsceifyTheme) -> AboutUpdateView {
    let colors = theme.colors;
    let (status, status_color, action) = match update.map(|value| &value.state) {
        Some(UpdateState::Idle) => (
            "Updates are checked automatically.".to_string(),
            colors.text_muted,
            None,
        ),
        Some(UpdateState::Checking) => {
            ("Checking for updates…".to_string(), colors.text_muted, None)
        }
        Some(UpdateState::Current) => ("Asceify is up to date.".to_string(), colors.primary, None),
        Some(UpdateState::Downloading { latest_version }) => (
            format!("Downloading Asceify {latest_version}…"),
            colors.text_muted,
            None,
        ),
        Some(UpdateState::ReadyToRestart { latest_version }) => (
            format!("Asceify {latest_version} is ready. Restart to update."),
            colors.primary,
            Some(AboutAction::Restart),
        ),
        Some(UpdateState::Error(error)) => (error.clone(), colors.danger, Some(AboutAction::Retry)),
        Some(UpdateState::PreparingRestart) => (
            "Preparing restart to install update…".to_string(),
            colors.text_muted,
            None,
        ),
        None => (
            "Update checking is unavailable in this session.".to_string(),
            colors.text_muted,
            None,
        ),
    };
    AboutUpdateView {
        status,
        status_color,
        action,
    }
}

fn about_dialog_header(terminal: &Entity<TerminalApp>, theme: &AsceifyTheme) -> AnyElement {
    let colors = theme.colors;
    let close_terminal = terminal.clone();
    div()
        .flex()
        .items_start()
        .justify_between()
        .p_4()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(super::terminal_chrome::brand_mark_sized(px(34.0)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_base()
                                .font_weight(platform_font_weight(TypographyRole::Strong))
                                .text_color(gpui_color(colors.text_primary))
                                .child("About Asceify"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                        ),
                ),
        )
        .child(chrome_close_button(
            "about_dialog_close",
            theme,
            move |_, cx| {
                close_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.close_about_dialog(terminal_cx);
                });
            },
        ))
        .into_any_element()
}

fn about_dialog_body(
    terminal: &Entity<TerminalApp>,
    update: Option<&UpdatePresentation>,
    view: &AboutUpdateView,
    theme: &AsceifyTheme,
) -> AnyElement {
    let colors = theme.colors;
    let system_version = update.map_or_else(
        || std::env::consts::OS.to_string(),
        |value| value.system_version.clone(),
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .pt_3()
        .pb_4()
        .child(about_detail_row(
            "System",
            system_version,
            colors.text_primary,
            theme,
        ))
        .child(about_update_row(terminal, view, theme))
        .into_any_element()
}

fn about_update_row(
    terminal: &Entity<TerminalApp>,
    view: &AboutUpdateView,
    theme: &AsceifyTheme,
) -> AnyElement {
    let colors = theme.colors;
    let action_button = view.action.map(|action| {
        let action_terminal = terminal.clone();
        let label = match action {
            AboutAction::Restart => "Restart to update",
            AboutAction::Retry => "Retry",
        };
        Button::new("about_update_action")
            .variant(theme, ButtonVariant::Filled)
            .icon(header_icon(HugeIcon::Refresh))
            .label(label)
            .on_click(move |_, _, cx| {
                action_terminal.update(cx, |terminal, terminal_cx| match action {
                    AboutAction::Restart => terminal.restart_to_update(terminal_cx),
                    AboutAction::Retry => terminal.retry_update_check(terminal_cx),
                });
            })
    });
    div()
        .mt_1()
        .min_h(px(38.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .flex()
                .min_w_0()
                .flex_1()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child("Updates"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(view.status_color))
                        .child(view.status.clone()),
                ),
        )
        .children(action_button)
        .into_any_element()
}

pub(super) fn about_dialog_layer(
    terminal: &Entity<TerminalApp>,
    update: Option<&UpdatePresentation>,
    theme: &AsceifyTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = terminal.clone();
    let view = about_update_view(update, theme);
    div()
        .id("about_dialog_scrim")
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
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_about_dialog(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            div()
                .id("about_dialog")
                .w(px(420.0))
                .flex()
                .flex_col()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border_secondary))
                .bg(gpui_color(colors.surface))
                .shadow_lg()
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .child(about_dialog_header(terminal, theme))
                .child(about_dialog_body(terminal, update, &view, theme)),
        )
        .into_any_element()
}

fn about_detail_row(
    label: &'static str,
    value: String,
    value_color: ThemeColor,
    theme: &AsceifyTheme,
) -> AnyElement {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap_4()
        .child(
            div()
                .w(px(112.0))
                .flex_none()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .text_right()
                .text_sm()
                .text_color(gpui_color(value_color))
                .child(value),
        )
        .into_any_element()
}
