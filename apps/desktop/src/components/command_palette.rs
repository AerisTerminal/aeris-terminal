use super::*;
use aeris_desktop::command_registry::{self, CommandId};

const COMMAND_PALETTE_LIMIT: usize = 8;

pub(super) fn command_palette_layer(
    terminal: &Entity<TerminalApp>,
    input: &Entity<InputState>,
    selected: usize,
    message: Option<String>,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    let query = input.read(cx).value().to_string();
    let results = command_registry::search(&query, COMMAND_PALETTE_LIMIT);
    let mnemonic = command_registry::parse_mnemonic(&query);
    let dismiss = terminal.clone();
    let rows = results.into_iter().enumerate().map(|(index, spec)| {
        command_row(
            terminal,
            spec.id,
            spec.title,
            spec.chord,
            index,
            index == selected,
            theme,
        )
    });
    let mnemonic_row = mnemonic
        .as_ref()
        .map(|mnemonic| mnemonic_command_row(terminal, mnemonic, theme));
    div()
        .id("command_palette_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(96.0))
        .bg(gpui_color(colors.surface.with_alpha(0.62)))
        .on_any_mouse_down(move |_, window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_command_palette(window, terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            div()
                .id("command_palette")
                .w(px(560.0))
                .max_h(px(520.0))
                .flex()
                .flex_col()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border_secondary))
                .bg(gpui_color(colors.surface))
                .shadow_lg()
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .p_3()
                        .border_b_1()
                        .border_color(gpui_color(colors.border))
                        .child(Input::new(input).platform(theme).flex_1()),
                )
                .children(mnemonic_row)
                .children(rows)
                .children(message.map(|message| {
                    div()
                        .px_5()
                        .py_3()
                        .text_sm()
                        .text_color(gpui_color(colors.warning))
                        .child(message)
                }))
                .child(
                    div()
                        .px_5()
                        .py_2()
                        .border_t_1()
                        .border_color(gpui_color(colors.border))
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child("↑↓ select · Enter run · Esc close"),
                ),
        )
        .into_any_element()
}

fn mnemonic_command_row(
    terminal: &Entity<TerminalApp>,
    mnemonic: &command_registry::Mnemonic,
    theme: &AerisTheme,
) -> AnyElement {
    let label = format!(
        "Run {}{}{}",
        mnemonic.symbol.as_deref().unwrap_or_default(),
        mnemonic
            .chart
            .map(|id| format!(
                " · {}",
                command_registry::command(id)
                    .title
                    .trim_start_matches("Chart: ")
            ))
            .unwrap_or_default(),
        mnemonic
            .interval
            .map(|id| format!(
                " · {}",
                command_registry::command(id)
                    .title
                    .trim_start_matches("Interval: ")
            ))
            .unwrap_or_default()
    );
    let execute = terminal.clone();
    div()
        .id("command_palette_mnemonic")
        .mx_2()
        .px_3()
        .py_2()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .cursor_pointer()
        .bg(gpui_color(theme.colors.primary.with_alpha(0.12)))
        .text_sm()
        .text_color(gpui_color(theme.colors.text_primary))
        .child(label)
        .on_click(move |_, window, cx| {
            execute.update(cx, |terminal, terminal_cx| {
                terminal.execute_command_palette_query(window, terminal_cx);
            });
        })
        .into_any_element()
}

fn command_row(
    terminal: &Entity<TerminalApp>,
    command: CommandId,
    title: &'static str,
    chord: Option<&'static str>,
    index: usize,
    selected: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let execute = terminal.clone();
    div()
        .id(("command_palette_row", index))
        .mx_2()
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .cursor_pointer()
        .when(selected, |row| {
            row.bg(gpui_color(colors.active_bg.over(colors.surface)))
        })
        .hover(|row| row.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_primary))
                .child(title),
        )
        .children(chord.map(|chord| {
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(chord)
        }))
        .on_click(move |_, window, cx| {
            execute.update(cx, |terminal, terminal_cx| {
                terminal.execute_registered_command(command, window, terminal_cx);
                if !matches!(
                    command,
                    CommandId::ConnectBroker | CommandId::DisconnectBroker
                ) {
                    terminal.close_command_palette(window, terminal_cx);
                }
            });
        })
        .into_any_element()
}
