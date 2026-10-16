//! Rithmic login: the Accounts-panel state for the saved login, and the dialog that picks a
//! Rithmic server and system and takes the user ID and password.
//!
//! Presentation only. The market runtime owns the server catalog, system discovery, the
//! sign-in check and the saved login. The password goes from the dialog straight to the
//! runtime, which keeps it only in the native credential vault; nothing here can read it back.

use super::*;
use aeris_market_runtime::{RithmicLoginSummary, RithmicServer};
use zeroize::Zeroizing;

/// Tallest the server and system lists grow before they scroll.
const SELECT_MENU_MAX_HEIGHT: f32 = 280.0;

/// Presentation mirror of the runtime-owned saved Rithmic login.
#[derive(Clone, Default)]
pub(super) struct RithmicLoginView {
    pub(super) saved: SavedRithmicLogin,
    pub(super) operation: Option<RithmicLoginOperation>,
    pub(super) message: Option<String>,
    pub(super) failed: bool,
}

/// What the runtime last reported about the saved login.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) enum SavedRithmicLogin {
    /// The runtime has not been asked yet, or could not answer.
    #[default]
    Unchecked,
    Missing,
    Saved(RithmicLoginSummary),
}

impl RithmicLoginView {
    pub(super) const fn connected(&self) -> bool {
        matches!(self.saved, SavedRithmicLogin::Saved(_))
    }

    /// Whether the runtime reported that no login is saved.
    pub(super) const fn missing(&self) -> bool {
        matches!(self.saved, SavedRithmicLogin::Missing)
    }

    /// A background check of a known state refreshes silently; only real changes show progress.
    pub(super) const fn busy(&self) -> bool {
        match self.operation {
            Some(RithmicLoginOperation::SigningIn | RithmicLoginOperation::Forgetting) => true,
            Some(RithmicLoginOperation::Checking) => {
                matches!(self.saved, SavedRithmicLogin::Unchecked)
            }
            None => false,
        }
    }

    pub(super) fn detail(&self) -> String {
        match (self.operation, &self.saved) {
            (Some(RithmicLoginOperation::SigningIn), _) => "Signing in to Rithmic…".to_string(),
            (Some(RithmicLoginOperation::Forgetting), _) => "Disconnecting…".to_string(),
            (Some(RithmicLoginOperation::Checking), SavedRithmicLogin::Unchecked) => {
                "Checking saved login…".to_string()
            }
            (_, SavedRithmicLogin::Saved(login)) => {
                format!("{} · {}", login.system_name, login.server.label())
            }
            _ => "CME Group futures with your broker's Rithmic login".to_string(),
        }
    }

    fn saved_login(&self) -> Option<&RithmicLoginSummary> {
        match &self.saved {
            SavedRithmicLogin::Saved(login) => Some(login),
            SavedRithmicLogin::Unchecked | SavedRithmicLogin::Missing => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RithmicLoginOperation {
    Checking,
    /// Signing in with a new login; the runtime saves it only if Rithmic accepts it.
    SigningIn,
    Forgetting,
}

/// The open login dialog and the background work it started. One login change and one system
/// discovery run at a time.
#[derive(Default)]
pub(super) struct RithmicLoginFlow {
    dialog: Option<RithmicLoginDialog>,
    operation: Option<gpui::Task<()>>,
    discovery: Option<gpui::Task<()>>,
}

struct RithmicLoginDialog {
    server: RithmicServer,
    systems: RithmicSystems,
    system: Option<String>,
    open_dropdown: Option<RithmicLoginDropdown>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    message: Option<String>,
}

enum RithmicSystems {
    Discovering,
    Listed(Vec<String>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RithmicLoginDropdown {
    Server,
    System,
}

/// Keeps a still-offered system chosen across a fresh discovery, and chooses the only one a
/// server offers.
fn retained_system(chosen: Option<String>, systems: &[String]) -> Option<String> {
    chosen
        .filter(|system| systems.contains(system))
        .or_else(|| match systems {
            [only] => Some(only.clone()),
            _ => None,
        })
}

impl TerminalApp {
    pub(super) fn connect_rithmic(
        &mut self,
        _: &ConnectRithmic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rithmic_login.dialog.is_some() {
            return;
        }
        let saved = self.data_connections.rithmic.saved_login().cloned();
        let user = cx.new(|input_cx| InputState::new(window, input_cx).placeholder("User ID"));
        let password = cx.new(|input_cx| {
            InputState::new(window, input_cx)
                .placeholder("Password")
                .masked(true)
        });
        user.update(cx, |input, input_cx| input.focus(window, input_cx));
        self.rithmic_login.dialog = Some(RithmicLoginDialog {
            server: saved
                .as_ref()
                .map_or(RithmicServer::CORE, |login| login.server),
            systems: RithmicSystems::Discovering,
            system: saved.map(|login| login.system_name),
            open_dropdown: None,
            user,
            password,
            message: None,
        });
        self.discover_rithmic_systems(cx);
        cx.notify();
    }

    pub(super) fn disconnect_rithmic(
        &mut self,
        _: &DisconnectRithmic,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_rithmic_login_operation(RithmicLoginOperation::Forgetting, None, window, cx);
    }

    /// Re-reads the saved login summary when the Accounts panel or symbol menu opens.
    pub(super) fn check_rithmic_login(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.run_rithmic_login_operation(RithmicLoginOperation::Checking, None, window, cx);
    }

    fn close_rithmic_login_dialog(&mut self, cx: &mut Context<Self>) {
        if self.rithmic_login.dialog.take().is_some() {
            cx.notify();
        }
    }

    fn toggle_rithmic_login_dropdown(
        &mut self,
        dropdown: RithmicLoginDropdown,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = &mut self.rithmic_login.dialog {
            dialog.open_dropdown = (dialog.open_dropdown != Some(dropdown)).then_some(dropdown);
            cx.notify();
        }
    }

    fn close_rithmic_login_dropdown(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.rithmic_login.dialog
            && dialog.open_dropdown.take().is_some()
        {
            cx.notify();
        }
    }

    fn select_rithmic_server(&mut self, server: RithmicServer, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.rithmic_login.dialog else {
            return;
        };
        dialog.open_dropdown = None;
        if dialog.server != server {
            dialog.server = server;
            dialog.system = None;
            dialog.message = None;
            self.discover_rithmic_systems(cx);
        }
        cx.notify();
    }

    fn select_rithmic_system(&mut self, system: String, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.rithmic_login.dialog {
            dialog.open_dropdown = None;
            dialog.system = Some(system);
            dialog.message = None;
            cx.notify();
        }
    }

    /// Asks the runtime which systems the dialog's server offers. A discovery already running
    /// is never doubled; when it finishes for a server the dialog has since left, it starts
    /// again for the current one.
    fn discover_rithmic_systems(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.rithmic_login.dialog else {
            return;
        };
        dialog.systems = RithmicSystems::Discovering;
        if self.rithmic_login.discovery.is_some() {
            return;
        }
        let server = dialog.server;
        let work = cx.background_executor().spawn(async move {
            engine_market_worker::shared_market_runtime()
                .and_then(|market| market.discover_rithmic_systems(server))
        });
        self.rithmic_login.discovery = Some(cx.spawn(async move |terminal, cx| {
            let result = work.await;
            let _ = terminal.update(cx, |terminal, cx| {
                terminal.rithmic_login.discovery = None;
                terminal.apply_rithmic_systems(server, result, cx);
            });
        }));
    }

    fn apply_rithmic_systems(
        &mut self,
        server: RithmicServer,
        result: Result<Vec<String>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.rithmic_login.dialog else {
            return;
        };
        if dialog.server != server {
            self.discover_rithmic_systems(cx);
            return;
        }
        dialog.systems = match result {
            Ok(systems) if systems.is_empty() => {
                RithmicSystems::Failed("This server offers no Rithmic systems".to_string())
            }
            Ok(systems) => {
                dialog.system = retained_system(dialog.system.take(), &systems);
                RithmicSystems::Listed(systems)
            }
            Err(error) => RithmicSystems::Failed(error),
        };
        cx.notify();
    }

    fn sign_in_rithmic(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.rithmic_login.operation.is_some() {
            return;
        }
        let Some(dialog) = &mut self.rithmic_login.dialog else {
            return;
        };
        let Some(system_name) = dialog.system.clone() else {
            dialog.message = Some("Choose a Rithmic system".to_string());
            cx.notify();
            return;
        };
        dialog.message = None;
        let login = RithmicLoginInput {
            server: dialog.server,
            system_name,
            user: Zeroizing::new(dialog.user.read(cx).value().to_string()),
            password: Zeroizing::new(dialog.password.read(cx).value().to_string()),
        };
        self.run_rithmic_login_operation(RithmicLoginOperation::SigningIn, Some(login), window, cx);
    }

    /// Runs one runtime-owned login operation off the UI thread and mirrors its outcome. A
    /// background check never interrupts a sign-in or disconnect already in flight.
    fn run_rithmic_login_operation(
        &mut self,
        operation: RithmicLoginOperation,
        login: Option<RithmicLoginInput>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let view = &mut self.data_connections.rithmic;
        if self.rithmic_login.operation.is_some() {
            if operation != RithmicLoginOperation::Checking {
                view.message =
                    Some("A Rithmic login change is already running; wait for it.".to_string());
                view.failed = true;
                cx.notify();
            }
            return;
        }
        view.operation = Some(operation);
        if operation != RithmicLoginOperation::Checking {
            view.message = None;
            view.failed = false;
        }
        let work = cx.background_executor().spawn(async move {
            let market = engine_market_worker::shared_market_runtime()?;
            match (operation, login) {
                (RithmicLoginOperation::Checking, _) => market.saved_rithmic_login(),
                (RithmicLoginOperation::Forgetting, _) => {
                    market.forget_rithmic_login().map(|()| None)
                }
                (RithmicLoginOperation::SigningIn, Some(login)) => market
                    .sign_in_rithmic(
                        login.server,
                        &login.system_name,
                        &login.user,
                        &login.password,
                    )
                    .map(|()| {
                        Some(RithmicLoginSummary {
                            server: login.server,
                            system_name: login.system_name,
                        })
                    }),
                (RithmicLoginOperation::SigningIn, None) => {
                    Err("Enter the Rithmic login to sign in".to_string())
                }
            }
        });
        self.rithmic_login.operation = Some(cx.spawn_in(window, async move |terminal, cx| {
            let result = work.await;
            let _ = cx.update(|window, cx| {
                terminal.update(cx, |terminal, cx| {
                    terminal.rithmic_login.operation = None;
                    terminal.finish_rithmic_login_operation(operation, result, window, cx);
                })
            });
        }));
        cx.notify();
    }

    fn finish_rithmic_login_operation(
        &mut self,
        operation: RithmicLoginOperation,
        result: Result<Option<RithmicLoginSummary>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = &mut self.data_connections.rithmic;
        view.operation = None;
        match (operation, result) {
            (_, Ok(saved)) => {
                match (operation, &saved) {
                    (RithmicLoginOperation::Checking, _)
                    | (RithmicLoginOperation::SigningIn, None) => {}
                    (RithmicLoginOperation::SigningIn, Some(login)) => {
                        view.message = Some(signed_in_message(login));
                        view.failed = false;
                    }
                    (RithmicLoginOperation::Forgetting, _) => {
                        view.message =
                            Some("Rithmic login removed from this computer.".to_string());
                        view.failed = false;
                    }
                }
                view.saved = saved.map_or(SavedRithmicLogin::Missing, SavedRithmicLogin::Saved);
                // A new login goes straight to choosing one of its markets, unless the
                // dialog was cancelled while signing in.
                if operation == RithmicLoginOperation::SigningIn
                    && self.rithmic_login.dialog.take().is_some()
                {
                    self.active_surface().update(cx, |surface, surface_cx| {
                        surface.choose_symbol_provider(
                            TerminalProvider::Rithmic,
                            window,
                            surface_cx,
                        );
                        surface.open_chrome_overlay(ChromeOverlay::Instrument, window, surface_cx);
                    });
                }
            }
            (RithmicLoginOperation::SigningIn, Err(error)) => {
                if let Some(dialog) = &mut self.rithmic_login.dialog {
                    dialog.message = Some(error);
                } else {
                    view.message = Some(error);
                    view.failed = true;
                }
            }
            (RithmicLoginOperation::Checking, Err(error)) => {
                view.saved = SavedRithmicLogin::Unchecked;
                view.message = Some(error);
                view.failed = true;
            }
            // The login is still saved; keep offering Disconnect.
            (RithmicLoginOperation::Forgetting, Err(error)) => {
                view.message = Some(error);
                view.failed = true;
            }
        }
        cx.notify();
    }
}

fn signing_in_status(dialog: &RithmicLoginDialog) -> String {
    format!(
        "Signing in to {} on {}…",
        dialog.system.as_deref().unwrap_or("Rithmic"),
        dialog.server.label()
    )
}

fn signed_in_message(login: &RithmicLoginSummary) -> String {
    format!(
        "Signed in to {} on {}. The login is saved on this computer.",
        login.system_name,
        login.server.label()
    )
}

/// A login on its way to the runtime. The user ID and password are wiped when it drops.
struct RithmicLoginInput {
    server: RithmicServer,
    system_name: String,
    user: Zeroizing<String>,
    password: Zeroizing<String>,
}

/// Rithmic's attribution marks, shown wherever Aeris offers a Rithmic login.
pub(super) fn rithmic_marks(theme: &AerisTheme) -> impl IntoElement + use<> {
    let mark = |mark| terminal_chrome::AttributionMarkImage {
        mark,
        mode: theme.mode,
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(mark(assets::AttributionMark::MarketDataByRithmic))
        .child(
            div()
                .w(px(theme.dimensions.border_width))
                .h(px(assets::AttributionMark::HEIGHT))
                .bg(gpui_color(theme.colors.border)),
        )
        .child(mark(assets::AttributionMark::PoweredByOmne))
}

/// The login dialog, centered over the whole window above the Accounts panel that opened it.
pub(super) fn rithmic_login_dialog_layer(
    terminal: &Entity<TerminalApp>,
    flow: &RithmicLoginFlow,
    view: &RithmicLoginView,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let dialog = flow.dialog.as_ref()?;
    let dismiss = terminal.clone();
    let cancel = terminal.clone();
    let save = terminal.clone();
    let signing_in = view.operation == Some(RithmicLoginOperation::SigningIn);
    Some(
        Dialog::new(
            "rithmic_login_dialog",
            DialogSize::Md,
            theme,
            move |_, cx| {
                dismiss.update(cx, TerminalApp::close_rithmic_login_dialog);
            },
        )
        .title("Connect Rithmic")
        .subtitle(
            div()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child("Use the Rithmic login from your broker or prop firm."),
        )
        .child(rithmic_login_dialog_body(
            terminal, dialog, signing_in, theme,
        ))
        .action(
            Button::new("rithmic_login_cancel", theme)
                .variant(ButtonVariant::Outline)
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, TerminalApp::close_rithmic_login_dialog);
                }),
        )
        .action(
            Button::new("rithmic_login_save", theme)
                .variant(ButtonVariant::Primary)
                .label(if signing_in { "Signing in" } else { "Connect" })
                .loading(signing_in)
                .disabled(signing_in || dialog.system.is_none())
                .on_click(move |_, window, cx| {
                    save.update(cx, |terminal, terminal_cx| {
                        terminal.sign_in_rithmic(window, terminal_cx);
                    });
                }),
        )
        .into_any_element(),
    )
}

fn field_label(label: &'static str, theme: &AerisTheme) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(label)
}

fn rithmic_login_dialog_body(
    terminal: &Entity<TerminalApp>,
    dialog: &RithmicLoginDialog,
    signing_in: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(field_label("Server", theme))
        .child(server_dropdown(terminal, dialog, theme))
        .child(field_label("System", theme))
        .child(system_dropdown(terminal, dialog, theme))
        .children(match &dialog.systems {
            RithmicSystems::Failed(error) => {
                let retry = terminal.clone();
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(gpui_color(colors.danger))
                                .child(error.clone()),
                        )
                        .child(
                            Button::new("rithmic_login_retry_discovery", theme)
                                .variant(ButtonVariant::Outline)
                                .label("Retry")
                                .on_click(move |_, _, cx| {
                                    retry.update(cx, |terminal, terminal_cx| {
                                        terminal.discover_rithmic_systems(terminal_cx);
                                        terminal_cx.notify();
                                    });
                                }),
                        ),
                )
            }
            RithmicSystems::Discovering | RithmicSystems::Listed(_) => None,
        })
        .child(field_label("User ID", theme))
        .child(Input::new(&dialog.user).platform(theme))
        .child(field_label("Password", theme))
        .child(Input::new(&dialog.password).platform(theme))
        .child(
            div()
                .mt_1()
                .px_3()
                .py_2()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .bg(gpui_color(colors.surface))
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(
                    "Aeris keeps this login only in this computer's protected credential \
                     storage. Rithmic market data needs a paid market-data subscription on the \
                     account and Aeris enabled by its broker or prop firm.",
                ),
        )
        .children(signing_in.then(|| {
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_secondary))
                .child(signing_in_status(dialog))
        }))
        .children(dialog.message.as_ref().map(|message| {
            div()
                .text_sm()
                .text_color(gpui_color(colors.danger))
                .child(message.clone())
        }))
        .child(div().mt_1().child(rithmic_marks(theme)))
}

fn dropdown_trigger(
    terminal: &Entity<TerminalApp>,
    dropdown: RithmicLoginDropdown,
    id: &'static str,
    label: String,
    open: bool,
    theme: &AerisTheme,
) -> Button {
    let toggle = terminal.clone();
    Button::new(id, theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Lg)
        .trigger()
        .full_width()
        .open(open)
        .aria_label(label.clone())
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_rithmic_login_dropdown(dropdown, terminal_cx);
            });
        })
}

/// A form select: the trigger plus, while open, its menu stretched under it.
fn select_field(
    terminal: &Entity<TerminalApp>,
    id: &'static str,
    trigger: Button,
    menu: Option<MenuPanel>,
) -> MenuAnchor {
    let dismiss = terminal.clone();
    MenuAnchor::new(id, trigger)
        .full_width()
        .menu(menu)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, TerminalApp::close_rithmic_login_dropdown);
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
    .max_height(px(SELECT_MENU_MAX_HEIGHT))
    .animate_from(PopupAnimationOrigin::TOP_LEFT)
}

fn server_dropdown(
    terminal: &Entity<TerminalApp>,
    dialog: &RithmicLoginDialog,
    theme: &AerisTheme,
) -> MenuAnchor {
    let open = dialog.open_dropdown == Some(RithmicLoginDropdown::Server);
    let menu = open.then(|| {
        select_menu("rithmic_login_server_menu", theme).children(
            RithmicServer::all().enumerate().map(|(index, server)| {
                let choose = terminal.clone();
                MenuRow::compact(
                    ("rithmic_login_server_option", index),
                    server.label(),
                    theme,
                )
                .checked(dialog.server == server)
                .on_click(move |_, _, cx| {
                    choose.update(cx, |terminal, terminal_cx| {
                        terminal.select_rithmic_server(server, terminal_cx);
                    });
                })
            }),
        )
    });
    select_field(
        terminal,
        "rithmic_login_server_field",
        dropdown_trigger(
            terminal,
            RithmicLoginDropdown::Server,
            "rithmic_login_server_select",
            dialog.server.label().to_string(),
            open,
            theme,
        ),
        menu,
    )
}

fn system_dropdown(
    terminal: &Entity<TerminalApp>,
    dialog: &RithmicLoginDialog,
    theme: &AerisTheme,
) -> MenuAnchor {
    let systems = match &dialog.systems {
        RithmicSystems::Listed(systems) => systems.as_slice(),
        RithmicSystems::Discovering | RithmicSystems::Failed(_) => &[],
    };
    let open = dialog.open_dropdown == Some(RithmicLoginDropdown::System) && !systems.is_empty();
    let menu = open.then(|| {
        select_menu("rithmic_login_system_menu", theme).children(systems.iter().enumerate().map(
            |(index, system)| {
                let choose = terminal.clone();
                let chosen = system.clone();
                MenuRow::compact(
                    ("rithmic_login_system_option", index),
                    system.clone(),
                    theme,
                )
                .checked(dialog.system.as_ref() == Some(system))
                .on_click(move |_, _, cx| {
                    let chosen = chosen.clone();
                    choose.update(cx, |terminal, terminal_cx| {
                        terminal.select_rithmic_system(chosen, terminal_cx);
                    });
                })
            },
        ))
    });
    let label = match (&dialog.systems, &dialog.system) {
        (RithmicSystems::Discovering, _) => "Finding systems on this server…".to_string(),
        (_, Some(system)) => system.clone(),
        (RithmicSystems::Listed(_), None) => "Choose a system".to_string(),
        (RithmicSystems::Failed(_), None) => "No systems found".to_string(),
    };
    let trigger = dropdown_trigger(
        terminal,
        RithmicLoginDropdown::System,
        "rithmic_login_system_select",
        label,
        open,
        theme,
    )
    .loading(matches!(dialog.systems, RithmicSystems::Discovering))
    .disabled(systems.is_empty());
    select_field(terminal, "rithmic_login_system_field", trigger, menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn systems(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn rediscovery_keeps_an_offered_system_and_picks_a_lone_one() {
        let offered = systems(&["Rithmic Paper Trading", "Rithmic 01"]);
        assert_eq!(
            retained_system(Some("Rithmic 01".to_string()), &offered),
            Some("Rithmic 01".to_string())
        );
        assert_eq!(retained_system(Some("Gone".to_string()), &offered), None);
        assert_eq!(retained_system(None, &offered), None);
        assert_eq!(
            retained_system(None, &systems(&["Rithmic Paper Trading"])),
            Some("Rithmic Paper Trading".to_string())
        );
    }

    #[test]
    fn login_card_names_the_saved_system_and_server_only() {
        let mut view = RithmicLoginView::default();
        assert!(!view.connected());
        assert!(!view.missing());
        view.operation = Some(RithmicLoginOperation::Checking);
        assert!(view.busy());
        assert_eq!(view.detail(), "Checking saved login…");
        view.operation = None;
        view.saved = SavedRithmicLogin::Missing;
        assert!(view.missing());
        assert!(!view.busy());
        view.saved = SavedRithmicLogin::Saved(RithmicLoginSummary {
            server: RithmicServer::CORE,
            system_name: "Rithmic Paper Trading".to_string(),
        });
        assert!(view.connected());
        assert_eq!(view.detail(), "Rithmic Paper Trading · Chicago (core)");
        // A background re-check of a known login never shows progress.
        view.operation = Some(RithmicLoginOperation::Checking);
        assert!(!view.busy());
        view.operation = Some(RithmicLoginOperation::Forgetting);
        assert!(view.busy());
        assert_eq!(view.detail(), "Disconnecting…");
        view.operation = Some(RithmicLoginOperation::SigningIn);
        assert!(view.busy());
        assert_eq!(view.detail(), "Signing in to Rithmic…");
    }

    #[test]
    fn sign_in_reports_name_the_system_and_server() {
        let login = RithmicLoginSummary {
            server: RithmicServer::CORE,
            system_name: "Rithmic Paper Trading".to_string(),
        };
        assert_eq!(
            signed_in_message(&login),
            "Signed in to Rithmic Paper Trading on Chicago (core). The login is saved on this \
             computer."
        );
    }
}
