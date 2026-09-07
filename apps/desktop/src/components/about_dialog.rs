use super::*;

#[derive(Clone, Copy)]
enum AboutAction {
    Update,
    Retry,
}

struct AboutUpdateView {
    status: String,
    status_color: ThemeColor,
    action: Option<AboutAction>,
}

fn about_update_view(
    update: Option<&UpdatePresentation>,
    theme: &AxiusflowTheme,
) -> AboutUpdateView {
    let colors = theme.colors;
    let (status, status_color, action) = match update.map(|value| &value.state) {
        Some(UpdateState::Idle | UpdateState::Checking) => {
            ("Checking for updates…".to_string(), colors.text_muted, None)
        }
        Some(UpdateState::Current) => {
            ("Axiusflow is up to date.".to_string(), colors.bullish, None)
        }
        Some(UpdateState::Available { latest_generation }) => (
            format!("An Axiusflow update is available (build {latest_generation})."),
            colors.bullish,
            Some(AboutAction::Update),
        ),
        Some(UpdateState::Error(error)) => (error.clone(), colors.danger, Some(AboutAction::Retry)),
        Some(UpdateState::PreparingRestart) => (
            "Preparing update and restart…".to_string(),
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

fn about_dialog_header(terminal: &Entity<TerminalApp>, theme: &AxiusflowTheme) -> AnyElement {
    let colors = theme.colors;
    let close_terminal = terminal.clone();
    div()
        .h(px(52.0))
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(gpui_color(colors.text_primary))
                        .child("About Axiusflow"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                ),
        )
        .child(
            Button::new("about_dialog_close")
                .theme(theme)
                .resting_fill(colors.surface_secondary)
                .icon(header_icon(HugeIcon::WindowClose))
                .aria_label("Close About")
                .on_click(move |_, _, cx| {
                    close_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.close_about_dialog(terminal_cx);
                    });
                }),
        )
        .into_any_element()
}

fn about_dialog_body(
    update: Option<&UpdatePresentation>,
    view: &AboutUpdateView,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let release = axiusflow_platform_runtime::current_release_identity();
    let system_version = update.map_or_else(
        || std::env::consts::OS.to_string(),
        |value| value.system_version.clone(),
    );
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .child(about_detail_row(
            "System",
            system_version,
            colors.text_primary,
            theme,
        ))
        .child(about_detail_row(
            "Installed build",
            release.install_generation.to_string(),
            colors.text_primary,
            theme,
        ))
        .child(menu_separator(theme))
        .child(about_detail_row(
            "Updates",
            view.status.clone(),
            view.status_color,
            theme,
        ))
        .into_any_element()
}

fn about_dialog_footer(
    terminal: &Entity<TerminalApp>,
    action: Option<AboutAction>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let action_button = action.map(|action| {
        let action_terminal = terminal.clone();
        let label = match action {
            AboutAction::Update => "Update now",
            AboutAction::Retry => "Retry",
        };
        Button::new("about_update_action")
            .theme(theme)
            .resting_fill(colors.surface)
            .icon(header_icon(HugeIcon::Refresh01Icon))
            .label(label)
            .on_click(move |_, _, cx| {
                action_terminal.update(cx, |terminal, terminal_cx| match action {
                    AboutAction::Update => terminal.update_now(terminal_cx),
                    AboutAction::Retry => terminal.retry_update_check(terminal_cx),
                });
            })
    });
    div()
        .min_h(px(52.0))
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(gpui_color(colors.border_secondary))
        .children(action_button)
        .into_any_element()
}

pub(super) fn about_dialog_layer(
    terminal: &Entity<TerminalApp>,
    update: Option<&UpdatePresentation>,
    theme: &AxiusflowTheme,
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
                .w(px(440.0))
                .flex()
                .flex_col()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border_secondary))
                .bg(gpui_color(colors.surface_secondary))
                .shadow_lg()
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .child(about_dialog_header(terminal, theme))
                .child(about_dialog_body(update, &view, theme))
                .child(about_dialog_footer(terminal, view.action, theme)),
        )
        .into_any_element()
}

fn about_detail_row(
    label: &'static str,
    value: String,
    value_color: ThemeColor,
    theme: &AxiusflowTheme,
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
