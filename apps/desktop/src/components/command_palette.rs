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
    Dialog::new(
        "command_palette",
        DialogSize::Lg,
        theme,
        move |window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_command_palette(window, terminal_cx);
            });
        },
    )
    .align(DialogAlign::Top)
    .list_body()
    .max_height(px(520.0))
    .header(Input::new(input).platform(theme).flex_1())
    .children(mnemonic_row)
    .children(rows)
    .children(message.map(|message| {
        div()
            .px_2()
            .py_2()
            .text_sm()
            .text_color(gpui_color(colors.text_warning))
            .child(message)
    }))
    .footer_leading("↑↓ select · Enter run · Esc close")
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
    // The typed mnemonic is the palette's primary action, so it stays highlighted.
    MenuRow::search_result("command_palette_mnemonic", label, theme)
        .highlighted(true)
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
    let execute = terminal.clone();
    MenuRow::search_result(("command_palette_row", index), title, theme)
        .highlighted(selected)
        .when_some(chord, MenuRow::detail)
        .on_click(move |_, window, cx| {
            execute.update(cx, |terminal, terminal_cx| {
                terminal.execute_registered_command(command, window, terminal_cx);
                terminal.close_command_palette(window, terminal_cx);
            });
        })
        .into_any_element()
}
