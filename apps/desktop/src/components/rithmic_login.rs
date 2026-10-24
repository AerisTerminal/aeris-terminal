//! Rithmic login: the Accounts-panel state for the saved login, and the inline form in its
//! Connections section that picks a Rithmic server and system and takes the user ID and password.
//!
//! Presentation only. The market runtime owns the server catalog, system discovery, the
//! sign-in check and the saved login. The password goes from the form straight to the
//! runtime, which keeps it only in the native credential vault; nothing here can read it back.

use super::big_trades_dialog::described_setting_row;
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

/// The open login form and the background work it started. One login change and one system
/// discovery run at a time.
#[derive(Default)]
pub(super) struct RithmicLoginFlow {
    form: Option<RithmicLoginForm>,
    operation: Option<gpui::Task<()>>,
    discovery: Option<gpui::Task<()>>,
}

struct RithmicLoginForm {
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
        if self.rithmic_login.form.is_some() {
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
        self.rithmic_login.form = Some(RithmicLoginForm {
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

    /// Drops an unfinished login form when the Accounts panel closes or leaves Connections.
    /// A sign-in already sent keeps running; its outcome lands on the connection card.
    pub(super) fn cancel_rithmic_login(
        &mut self,
        _: &CancelRithmicLogin,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_rithmic_login_form(cx);
    }

    fn close_rithmic_login_form(&mut self, cx: &mut Context<Self>) {
        if self.rithmic_login.form.take().is_some() {
            cx.notify();
        }
    }

    fn toggle_rithmic_login_dropdown(
        &mut self,
        dropdown: RithmicLoginDropdown,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = &mut self.rithmic_login.form {
            form.open_dropdown = (form.open_dropdown != Some(dropdown)).then_some(dropdown);
            cx.notify();
        }
    }

    fn close_rithmic_login_dropdown(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.rithmic_login.form
            && form.open_dropdown.take().is_some()
        {
            cx.notify();
        }
    }

    fn select_rithmic_server(&mut self, server: RithmicServer, cx: &mut Context<Self>) {
        let Some(form) = &mut self.rithmic_login.form else {
            return;
        };
        form.open_dropdown = None;
        if form.server != server {
            form.server = server;
            form.system = None;
            form.message = None;
            self.discover_rithmic_systems(cx);
        }
        cx.notify();
    }

    fn select_rithmic_system(&mut self, system: String, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.rithmic_login.form {
            form.open_dropdown = None;
            form.system = Some(system);
            form.message = None;
            cx.notify();
        }
    }

    /// Asks the runtime which systems the form's server offers. A discovery already running
    /// is never doubled; when it finishes for a server the form has since left, it starts
    /// again for the current one.
    fn discover_rithmic_systems(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.rithmic_login.form else {
            return;
        };
        form.systems = RithmicSystems::Discovering;
        if self.rithmic_login.discovery.is_some() {
            return;
        }
        let server = form.server;
        let market = self.lifecycle.market().clone();
        let work = cx
            .background_executor()
            .spawn(async move { market.discover_rithmic_systems(server) });
        self.rithmic_login.discovery = Some(cx.spawn(async move |terminal, cx| {
            let result = work.await;
            if let Err(error) = terminal.update(cx, |terminal, cx| {
                terminal.rithmic_login.discovery = None;
                terminal.apply_rithmic_systems(server, result, cx);
            }) && terminal.upgrade().is_some()
            {
                diagnostic!("Live workspace could not receive provider system discovery: {error}");
            }
        }));
    }

    fn apply_rithmic_systems(
        &mut self,
        server: RithmicServer,
        result: Result<Vec<String>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.rithmic_login.form else {
            return;
        };
        if form.server != server {
            self.discover_rithmic_systems(cx);
            return;
        }
        form.systems = match result {
            Ok(systems) if systems.is_empty() => {
                RithmicSystems::Failed("This server offers no Rithmic systems".to_string())
            }
            Ok(systems) => {
                form.system = retained_system(form.system.take(), &systems);
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
        let Some(form) = &mut self.rithmic_login.form else {
            return;
        };
        let Some(system_name) = form.system.clone() else {
            form.message = Some("Choose a Rithmic system".to_string());
            cx.notify();
            return;
        };
        form.message = None;
        let login = RithmicLoginInput {
            server: form.server,
            system_name,
            user: Zeroizing::new(form.user.read(cx).value().to_string()),
            password: Zeroizing::new(form.password.read(cx).value().to_string()),
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
        let market = self.lifecycle.market().clone();
        let work = cx.background_executor().spawn(async move {
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
            if let Err(error) = cx
                .update(|window, cx| {
                    terminal.update(cx, |terminal, cx| {
                        terminal.rithmic_login.operation = None;
                        terminal.finish_rithmic_login_operation(operation, result, window, cx);
                    })
                })
                .and_then(|result| result)
                && terminal.upgrade().is_some()
            {
                diagnostic!("Live workspace could not receive provider login result: {error}");
            }
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
                // form was cancelled while signing in.
                if operation == RithmicLoginOperation::SigningIn
                    && self.rithmic_login.form.take().is_some()
                {
                    self.active_surface().update(cx, |surface, surface_cx| {
                        surface.choose_symbol_scope(
                            SymbolSearchScope::Provider(terminal_provider_id(
                                TerminalProvider::Rithmic,
                            )),
                            window,
                            surface_cx,
                        );
                        surface.open_chrome_overlay(ChromeOverlay::Instrument, window, surface_cx);
                    });
                }
            }
            (RithmicLoginOperation::SigningIn, Err(error)) => {
                if let Some(form) = &mut self.rithmic_login.form {
                    form.message = Some(error);
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

fn signing_in_status(form: &RithmicLoginForm) -> String {
    format!(
        "Signing in to {} on {}…",
        form.system.as_deref().unwrap_or("Rithmic"),
        form.server.label()
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

/// Whether the inline login form is open in the Connections tab.
pub(super) const fn rithmic_login_form_open(flow: &RithmicLoginFlow) -> bool {
    flow.form.is_some()
}

/// The inline login form under the Rithmic connection row: one row per field, label on the
/// left and the field on the right. Escape inside it cancels the form without closing the
/// Accounts panel around it.
pub(super) fn rithmic_login_form(
    terminal: &Entity<TerminalApp>,
    flow: &RithmicLoginFlow,
    view: &RithmicLoginView,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let form = flow.form.as_ref()?;
    let colors = theme.colors;
    let escape = terminal.clone();
    let signing_in = view.operation == Some(RithmicLoginOperation::SigningIn);
    let field = |control: AnyElement| {
        div()
            .w(px(accounts_panel::ACCOUNTS_FIELD_WIDTH))
            .child(control)
            .into_any_element()
    };
    Some(
        div()
            .id("rithmic_login_form")
            .flex()
            .flex_col()
            .on_key_down(move |event, _, cx| {
                if event.keystroke.key.eq_ignore_ascii_case("escape") {
                    escape.update(cx, TerminalApp::close_rithmic_login_form);
                    cx.stop_propagation();
                }
            })
            .child(described_setting_row(
                "Server",
                [SharedString::from(
                    "Where your broker's Rithmic system runs",
                )],
                field(server_dropdown(terminal, form, theme).into_any_element()),
                theme,
            ))
            .child(described_setting_row(
                "System",
                [SharedString::from(
                    "The system your broker or prop firm gave you",
                )],
                field(system_dropdown(terminal, form, theme).into_any_element()),
                theme,
            ))
            .children(discovery_failure(terminal, form, theme))
            .child(described_setting_row(
                "User ID",
                [SharedString::from("The Rithmic user ID from your broker")],
                accounts_panel::accounts_field(&form.user, theme),
                theme,
            ))
            .child(described_setting_row(
                "Password",
                [SharedString::from(
                    "Kept only in this computer's protected credential storage",
                )],
                accounts_panel::accounts_field(&form.password, theme),
                theme,
            ))
            .child(
                div()
                    .py_2()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(
                        "Rithmic market data needs a paid market-data subscription on the \
                         account and Aeris enabled by your broker or prop firm.",
                    ),
            )
            .children(signing_in.then(|| {
                div()
                    .pb_2()
                    .text_sm()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(signing_in_status(form))
            }))
            .children(form.message.as_ref().map(|message| {
                div()
                    .pb_2()
                    .text_sm()
                    .text_color(gpui_color(colors.danger))
                    .child(message.clone())
            }))
            .child(form_actions(terminal, form, signing_in, theme))
            .into_any_element(),
    )
}

/// Why the server's systems could not be listed, with a retry.
fn discovery_failure(
    terminal: &Entity<TerminalApp>,
    form: &RithmicLoginForm,
    theme: &AerisTheme,
) -> Option<impl IntoElement> {
    let RithmicSystems::Failed(error) = &form.systems else {
        return None;
    };
    let retry = terminal.clone();
    Some(
        div()
            .pb_2()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(gpui_color(theme.colors.danger))
                    .child(error.clone()),
            )
            .child(
                Button::new("rithmic_login_retry_discovery", theme)
                    .variant(ButtonVariant::Secondary)
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

fn form_actions(
    terminal: &Entity<TerminalApp>,
    form: &RithmicLoginForm,
    signing_in: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let cancel = terminal.clone();
    let save = terminal.clone();
    div()
        .pb_2()
        .flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new("rithmic_login_cancel", theme)
                .variant(ButtonVariant::Secondary)
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, TerminalApp::close_rithmic_login_form);
                }),
        )
        .child(
            Button::new("rithmic_login_save", theme)
                .variant(ButtonVariant::Primary)
                .label(if signing_in { "Signing in" } else { "Connect" })
                .loading(signing_in)
                .disabled(signing_in || form.system.is_none())
                .on_click(move |_, window, cx| {
                    save.update(cx, |terminal, terminal_cx| {
                        terminal.sign_in_rithmic(window, terminal_cx);
                    });
                }),
        )
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
        .variant(ButtonVariant::Secondary)
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
    form: &RithmicLoginForm,
    theme: &AerisTheme,
) -> MenuAnchor {
    let open = form.open_dropdown == Some(RithmicLoginDropdown::Server);
    let menu = open.then(|| {
        select_menu("rithmic_login_server_menu", theme).children(
            RithmicServer::all().enumerate().map(|(index, server)| {
                let choose = terminal.clone();
                MenuRow::compact(
                    ("rithmic_login_server_option", index),
                    server.label(),
                    theme,
                )
                .checked(form.server == server)
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
            form.server.label().to_string(),
            open,
            theme,
        ),
        menu,
    )
}

fn system_dropdown(
    terminal: &Entity<TerminalApp>,
    form: &RithmicLoginForm,
    theme: &AerisTheme,
) -> MenuAnchor {
    let systems = match &form.systems {
        RithmicSystems::Listed(systems) => systems.as_slice(),
        RithmicSystems::Discovering | RithmicSystems::Failed(_) => &[],
    };
    let open = form.open_dropdown == Some(RithmicLoginDropdown::System) && !systems.is_empty();
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
                .checked(form.system.as_ref() == Some(system))
                .on_click(move |_, _, cx| {
                    let chosen = chosen.clone();
                    choose.update(cx, |terminal, terminal_cx| {
                        terminal.select_rithmic_system(chosen, terminal_cx);
                    });
                })
            },
        ))
    });
    let label = match (&form.systems, &form.system) {
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
    .loading(matches!(form.systems, RithmicSystems::Discovering))
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
