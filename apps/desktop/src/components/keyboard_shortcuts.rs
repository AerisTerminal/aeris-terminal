//! The keyboard shortcuts dialog, opened from the profile menu. It reads every shortcut from the
//! command registry, the same tables the key bindings and workspace shortcuts are matched from,
//! so the list cannot drift from the keys the app actually handles.

use super::chart_chrome::TradingShortcutMode;
use super::*;
use aeris_desktop::command_registry::{self, CommandGroup, ShortcutRow};

const KEYBOARD_SHORTCUTS_MAX_HEIGHT: f32 = 600.0;
const SHORTCUT_ROW_HEIGHT: f32 = 32.0;
const KEYCAP_HEIGHT: f32 = 20.0;

impl TerminalApp {
    pub(super) fn open_keyboard_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_platform_menu(window, cx);
        self.keyboard_shortcuts_open = true;
        // The shell's key handler closes the dialog on Escape, so it needs keyboard focus.
        self.chrome_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_keyboard_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.keyboard_shortcuts_open) {
            self.focus_workspace(window, cx);
            cx.notify();
        }
    }

    pub(super) fn keyboard_shortcuts_layer(&self, terminal: &Entity<Self>) -> Option<AnyElement> {
        self.keyboard_shortcuts_open.then(|| {
            keyboard_shortcuts_dialog(terminal, self.chart_chrome.trading_shortcuts, &self.theme)
        })
    }
}

fn keyboard_shortcuts_dialog(
    terminal: &Entity<TerminalApp>,
    trading_shortcuts: TradingShortcutMode,
    theme: &AerisTheme,
) -> AnyElement {
    let dismiss = terminal.clone();
    Dialog::new(
        "keyboard_shortcuts",
        DialogSize::Md,
        theme,
        move |window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_keyboard_shortcuts(window, terminal_cx);
            });
        },
    )
    .title("Keyboard shortcuts")
    .subtitle("Shortcuts act on the active workspace")
    .max_height(px(KEYBOARD_SHORTCUTS_MAX_HEIGHT))
    .children(
        command_registry::shortcut_sections()
            .into_iter()
            .map(|(group, rows)| shortcut_section(group, &rows, trading_shortcuts, theme)),
    )
    .footer_leading("Esc close")
    .into_any_element()
}

fn shortcut_section(
    group: CommandGroup,
    rows: &[ShortcutRow],
    trading_shortcuts: TradingShortcutMode,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let note = (group == CommandGroup::Trading).then(|| {
        if trading_shortcuts.one_click() {
            "One-click trading is on: shortcuts send at once."
        } else {
            "Each shortcut asks for confirmation before it sends."
        }
    });
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_muted))
                .child(group.label()),
        )
        .children(note.map(|note| {
            div()
                .pt_1()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(note)
        }))
        .children(rows.iter().map(|row| shortcut_row(row, theme)))
        .into_any_element()
}

fn shortcut_row(row: &ShortcutRow, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let triggers = row.triggers.iter().enumerate().map(|(index, trigger)| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .children((index > 0).then(|| {
                div()
                    .pr_1()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child("or")
            }))
            .children(trigger.keys().into_iter().map(|key| keycap(key, theme)))
    });
    div()
        .h(px(SHORTCUT_ROW_HEIGHT))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(gpui_color(colors.text_primary))
                .child(row.title),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .children(triggers),
        )
}

fn keycap(key: String, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .h(px(KEYCAP_HEIGHT))
        .min_w(px(KEYCAP_HEIGHT))
        .px_1p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(key)
}
