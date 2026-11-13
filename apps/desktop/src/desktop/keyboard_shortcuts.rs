//! The keyboard shortcuts dialog, opened from the profile menu. It reads every shortcut from the
//! command registry with the user's overrides applied, the same set the key bindings are built
//! from, so the list cannot drift from the keys the app actually handles. Each command row
//! changes, clears or resets its shortcut; a change applies to the keymap at once and is saved
//! with the chart chrome.

use super::chart_chrome::TradingShortcutMode;
use super::*;
use aeris_desktop::command_registry::{
    self, CommandGroup, CommandId, ShortcutChord, ShortcutOverrides, ShortcutRefusal, ShortcutRow,
};
use aeris_localization::{shell, tr};

const KEYBOARD_SHORTCUTS_MAX_HEIGHT: f32 = 600.0;
const SHORTCUT_ROW_HEIGHT: f32 = 32.0;
const KEYCAP_HEIGHT: f32 = 20.0;

/// The shortcut being recorded and the last change the registry refused, while the dialog is
/// open.
#[derive(Default)]
pub(super) struct ShortcutEditor {
    recording: Option<ShortcutRecording>,
    refusal: Option<(CommandId, ShortcutRefusal)>,
}

struct ShortcutRecording {
    command: CommandId,
    /// Takes every key press in the window ahead of the key bindings and the shell's key
    /// handler until it is dropped, so the chord being recorded runs nothing.
    _interceptor: gpui::Subscription,
}

impl ShortcutEditor {
    pub(super) fn recording(&self) -> Option<CommandId> {
        self.recording.as_ref().map(|recording| recording.command)
    }

    pub(super) fn refusal(&self, command: CommandId) -> Option<ShortcutRefusal> {
        self.refusal
            .and_then(|(refused, refusal)| (refused == command).then_some(refusal))
    }
}

impl TerminalApp {
    pub(super) fn open_keyboard_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .overlays
            .open_keyboard_shortcuts()
            .is_some_and(OverlayKind::pins_title_bar)
        {
            self.release_frameless_title_bar(window, cx);
        }
        // The shell's key handler closes the dialog on Escape, so it needs keyboard focus.
        self.window.chrome_focus().focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_keyboard_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcut_editor = ShortcutEditor::default();
        if self.overlays.close(OverlayKind::KeyboardShortcuts) {
            self.focus_workspace(window, cx);
            cx.notify();
        }
    }

    pub(super) fn keyboard_shortcuts_layer(&self, terminal: &Entity<Self>) -> Option<AnyElement> {
        self.overlays
            .is_open(OverlayKind::KeyboardShortcuts)
            .then(|| {
                keyboard_shortcuts_dialog(
                    terminal,
                    &ShortcutsDialogState {
                        trading_shortcuts: self.chrome.preferences().trading_shortcuts,
                        overrides: self.chrome.preferences().shortcut_overrides,
                        editor: &self.shortcut_editor,
                    },
                    &self.theme,
                )
            })
    }

    /// Listens for the next chord pressed in this window and makes it `command`'s shortcut.
    pub(super) fn record_shortcut(
        &mut self,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal = cx.weak_entity();
        let recording_window = window.window_handle();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != recording_window {
                return;
            }
            let consumed = terminal
                .update(cx, |terminal, cx| {
                    terminal.record_shortcut_keystroke(&event.keystroke, cx)
                })
                .unwrap_or(false);
            if consumed {
                cx.stop_propagation();
            }
        });
        self.shortcut_editor = ShortcutEditor {
            recording: Some(ShortcutRecording {
                command,
                _interceptor: interceptor,
            }),
            refusal: None,
        };
        cx.notify();
    }

    /// Handles a key press while a shortcut is being recorded, returning whether it was taken.
    /// Modifiers alone wait for the key they hold, Escape stops recording, and a chord the
    /// registry refuses keeps recording with the reason shown.
    fn record_shortcut_keystroke(
        &mut self,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(command) = self.shortcut_editor.recording() else {
            return false;
        };
        if !self.overlays.is_open(OverlayKind::KeyboardShortcuts) {
            self.shortcut_editor = ShortcutEditor::default();
            return false;
        }
        if keystroke.is_modifier_key() {
            return true;
        }
        if ShortcutChord::named("escape").matches(keystroke) {
            self.shortcut_editor = ShortcutEditor::default();
            cx.notify();
            return true;
        }
        let overrides = self.chrome.preferences().shortcut_overrides;
        match ShortcutChord::from_keystroke(keystroke).and_then(|chord| {
            overrides.bind(command, chord, &aeris_chart_integration::chart_keystrokes())
        }) {
            Ok(overrides) => self.apply_shortcut_overrides(overrides, cx),
            Err(refusal) => {
                self.shortcut_editor.refusal = Some((command, refusal));
                cx.notify();
            }
        }
        true
    }

    pub(super) fn clear_shortcut(&mut self, command: CommandId, cx: &mut Context<Self>) {
        let overrides = self.chrome.preferences().shortcut_overrides.unbind(command);
        self.apply_shortcut_overrides(overrides, cx);
    }

    pub(super) fn reset_shortcut(&mut self, command: CommandId, cx: &mut Context<Self>) {
        match self.chrome.preferences().shortcut_overrides.reset(command) {
            Ok(overrides) => self.apply_shortcut_overrides(overrides, cx),
            Err(refusal) => {
                self.shortcut_editor = ShortcutEditor {
                    recording: None,
                    refusal: Some((command, refusal)),
                };
                cx.notify();
            }
        }
    }

    pub(super) fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.apply_shortcut_overrides(ShortcutOverrides::default(), cx);
    }

    /// Binds `overrides` in the keymap, then keeps and saves them with the chart chrome. The
    /// registry checked them, so binding fails only on a registry defect, which keeps the
    /// current shortcuts.
    pub(super) fn apply_shortcut_overrides(
        &mut self,
        overrides: ShortcutOverrides,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_editor = ShortcutEditor::default();
        if let Err(error) = bind_desktop_keys(&overrides, cx) {
            diagnostic!("Aeris keyboard shortcuts could not change: {error}");
            cx.notify();
            return;
        }
        self.chrome.preferences_mut().shortcut_overrides = overrides;
        // Surfaces save their own copy of the chrome preferences, so each one must carry the
        // new value or its next save would restore the old one.
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.shortcut_overrides = overrides;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }
}

struct ShortcutsDialogState<'a> {
    trading_shortcuts: TradingShortcutMode,
    overrides: ShortcutOverrides,
    editor: &'a ShortcutEditor,
}

fn keyboard_shortcuts_dialog(
    terminal: &Entity<TerminalApp>,
    state: &ShortcutsDialogState<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let dismiss = terminal.clone();
    let reset_all = terminal.clone();
    Dialog::new(
        "keyboard_shortcuts",
        DialogSize::Lg,
        theme,
        move |window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_keyboard_shortcuts(window, terminal_cx);
            });
        },
    )
    .title(tr!(shell::KEYBOARD_SHORTCUTS))
    .subtitle(tr!(shell::SHORTCUTS_SUBTITLE))
    .max_height(px(KEYBOARD_SHORTCUTS_MAX_HEIGHT))
    .children(
        command_registry::shortcut_sections(&state.overrides)
            .into_iter()
            .map(|(group, rows)| shortcut_section(terminal, group, &rows, state, theme)),
    )
    .footer_leading(tr!(shell::SHORTCUTS_FOOTER))
    .action(
        Button::new("keyboard_shortcuts_reset_all", theme)
            .variant(ButtonVariant::Secondary)
            .button_size(ButtonSize::Sm)
            .label(tr!(shell::SHORTCUT_RESET_ALL))
            .disabled(!state.overrides.any_customized())
            .on_press(move |_, _, cx| {
                reset_all.update(cx, TerminalApp::reset_all_shortcuts);
            }),
    )
    .into_any_element()
}

fn shortcut_section(
    terminal: &Entity<TerminalApp>,
    group: CommandGroup,
    rows: &[ShortcutRow],
    state: &ShortcutsDialogState<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let note = (group == CommandGroup::Trading).then(|| {
        if state.trading_shortcuts.one_click() {
            tr!(shell::ONE_CLICK_NOTE)
        } else {
            tr!(shell::CONFIRM_NOTE)
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
                .child(tr!(group.label())),
        )
        .children(note.map(|note| {
            div()
                .pt_1()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(note)
        }))
        .children(
            rows.iter()
                .map(|row| shortcut_row(terminal, row, state.editor, theme)),
        )
        .into_any_element()
}

fn shortcut_row(
    terminal: &Entity<TerminalApp>,
    row: &ShortcutRow,
    editor: &ShortcutEditor,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let recording = row
        .command
        .is_some_and(|command| editor.recording() == Some(command));
    let keys = if recording {
        recording_prompt(theme).into_any_element()
    } else if row.triggers.is_empty() {
        div()
            .text_xs()
            .text_color(gpui_color(colors.text_muted))
            .child(tr!(shell::SHORTCUT_NOT_SET))
            .into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .gap_2()
            .children(trigger_keycaps(row, theme))
            .into_any_element()
    };
    let refusal = row.command.and_then(|command| editor.refusal(command));
    div()
        .flex()
        .flex_col()
        .border_b(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .h(px(SHORTCUT_ROW_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(gpui_color(colors.text_default))
                        .child(tr!(row.title)),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .child(keys)
                        .children(
                            row.command
                                .map(|command| shortcut_actions(terminal, command, row, theme)),
                        ),
                ),
        )
        .children(refusal.map(|refusal| {
            div()
                .pb_1()
                .text_xs()
                .text_color(gpui_color(colors.text_warning))
                .child(refusal_message(refusal))
        }))
}

fn trigger_keycaps<'a>(
    row: &'a ShortcutRow,
    theme: &'a AerisTheme,
) -> impl Iterator<Item = Div> + 'a {
    let colors = theme.colors;
    row.triggers
        .iter()
        .enumerate()
        .map(move |(index, trigger)| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .children((index > 0).then(|| {
                    div()
                        .pr_1()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(tr!(shell::OR))
                }))
                .children(trigger.keys().into_iter().map(|key| keycap(key, theme)))
        })
}

fn recording_prompt(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .h(px(KEYCAP_HEIGHT))
        .px_1p5()
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.ring_primary))
        .bg(gpui_color(colors.surface_secondary))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(tr!(shell::SHORTCUT_RECORDING))
}

/// Change, and Clear and Reset when they would change anything.
fn shortcut_actions(
    terminal: &Entity<TerminalApp>,
    command: CommandId,
    row: &ShortcutRow,
    theme: &AerisTheme,
) -> Div {
    let title = tr!(row.title);
    let change = terminal.clone();
    let clear = terminal.clone();
    let reset = terminal.clone();
    let id = command.persisted_id();
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            row_button(("shortcut_change", id), tr!(shell::SHORTCUT_CHANGE), theme)
                .aria_label(tr!(shell::SHORTCUT_CHANGE_ARIA, command = title))
                .on_press(move |_, window, cx| {
                    change.update(cx, |terminal, cx| {
                        terminal.record_shortcut(command, window, cx);
                    });
                }),
        )
        .children((!row.triggers.is_empty()).then(|| {
            row_button(("shortcut_clear", id), tr!(shell::SHORTCUT_CLEAR), theme)
                .aria_label(tr!(shell::SHORTCUT_CLEAR_ARIA, command = title))
                .on_press(move |_, _, cx| {
                    clear.update(cx, |terminal, cx| terminal.clear_shortcut(command, cx));
                })
        }))
        .children(row.customized.then(|| {
            row_button(("shortcut_reset", id), tr!(shell::SHORTCUT_RESET), theme)
                .aria_label(tr!(shell::SHORTCUT_RESET_ARIA, command = title))
                .on_press(move |_, _, cx| {
                    reset.update(cx, |terminal, cx| terminal.reset_shortcut(command, cx));
                })
        }))
}

fn row_button(
    (action, command): (&'static str, &'static str),
    label: &'static str,
    theme: &AerisTheme,
) -> Button {
    Button::new(
        gpui::ElementId::Name(SharedString::from(format!("{action}_{command}"))),
        theme,
    )
    .variant(ButtonVariant::Secondary)
    .button_size(ButtonSize::Sm)
    .label(label)
}

fn refusal_message(refusal: ShortcutRefusal) -> String {
    match refusal {
        ShortcutRefusal::Taken(owner) => {
            tr!(shell::SHORTCUT_TAKEN, command = tr!(owner.title()))
        }
        ShortcutRefusal::System => tr!(shell::SHORTCUT_SYSTEM).to_string(),
        ShortcutRefusal::ChartKey => tr!(shell::SHORTCUT_CHART_KEY).to_string(),
        ShortcutRefusal::NeedsModifier => tr!(shell::SHORTCUT_NEEDS_MODIFIER).to_string(),
        ShortcutRefusal::Unsupported | ShortcutRefusal::NotBindable => {
            tr!(shell::SHORTCUT_UNSUPPORTED).to_string()
        }
    }
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
