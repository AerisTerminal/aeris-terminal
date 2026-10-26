//! Descriptor-driven provider login form. Credentials cross only the market runtime's
//! provider-id-keyed API; the desktop never opens the vault or a provider adapter.

use super::big_trades_dialog::described_setting_row;
use super::*;
use aeris_market_runtime::{
    ConnectMethod, LoginFieldValue, ProviderLoginServer, SavedProviderLogin,
};
use zeroize::Zeroizing;

const SELECT_MENU_MAX_HEIGHT: f32 = 280.0;

#[derive(Clone, Default)]
pub(super) struct ProviderLoginView {
    pub(super) saved: SavedLogin,
    pub(super) operation: Option<LoginOperation>,
    pub(super) message: Option<String>,
    pub(super) failed: bool,
}

#[derive(Clone, Default)]
pub(super) enum SavedLogin {
    #[default]
    Unchecked,
    Missing,
    Saved(SavedProviderLogin),
}

impl ProviderLoginView {
    pub(super) const fn connected(&self) -> bool {
        matches!(self.saved, SavedLogin::Saved(_))
    }

    pub(super) const fn missing(&self) -> bool {
        matches!(self.saved, SavedLogin::Missing)
    }

    pub(super) const fn busy(&self) -> bool {
        match self.operation {
            Some(LoginOperation::SigningIn | LoginOperation::Forgetting) => true,
            Some(LoginOperation::Checking) => matches!(self.saved, SavedLogin::Unchecked),
            None => false,
        }
    }

    pub(super) fn detail(&self) -> String {
        match (self.operation, &self.saved) {
            (Some(LoginOperation::SigningIn), _) => "Signing in…".to_string(),
            (Some(LoginOperation::Forgetting), _) => "Disconnecting…".to_string(),
            (Some(LoginOperation::Checking), SavedLogin::Unchecked) => {
                "Checking saved login…".to_string()
            }
            (_, SavedLogin::Saved(login)) => {
                format!("{} · {}", login.system_name, login.server_label)
            }
            _ => "Credentials required".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoginOperation {
    Checking,
    SigningIn,
    Forgetting,
}

#[derive(Default)]
pub(super) struct ProviderLoginFlow {
    form: Option<ProviderLoginForm>,
    operations: std::collections::BTreeMap<&'static str, gpui::Task<()>>,
    discovery: Option<gpui::Task<()>>,
}

struct ProviderLoginForm {
    provider: ProviderId,
    servers: Vec<ProviderLoginServer>,
    server: &'static str,
    systems: Systems,
    system: Option<String>,
    dropdown: Option<LoginDropdown>,
    inputs: Vec<(&'static str, Entity<InputState>)>,
    message: Option<String>,
}

enum Systems {
    Discovering,
    Listed(Vec<String>),
    Failed(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoginDropdown {
    Server,
    System,
}

fn retained_system(chosen: Option<String>, systems: &[String]) -> Option<String> {
    chosen
        .filter(|system| systems.contains(system))
        .or_else(|| match systems {
            [only] => Some(only.clone()),
            _ => None,
        })
}

/// Values are wiped when the background runtime command completes or is cancelled.
struct SubmittedLogin {
    fields: Vec<(&'static str, Zeroizing<String>)>,
}

impl SubmittedLogin {
    fn values(&self) -> Vec<LoginFieldValue<'_>> {
        self.fields
            .iter()
            .map(|(id, value)| LoginFieldValue { id, value })
            .collect()
    }

    fn submit(
        &self,
        provider: ProviderId,
        sign_in: impl FnOnce(&str, &[LoginFieldValue<'_>]) -> Result<(), String>,
    ) -> Result<(), String> {
        sign_in(provider.id(), &self.values())
    }
}

impl TerminalApp {
    pub(super) fn connect_provider(
        &mut self,
        action: &ConnectProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let provider = action.0;
        let Some(descriptor) = provider.descriptor() else {
            return;
        };
        let ConnectMethod::VaultCredentials { schema } = descriptor.connect else {
            return;
        };
        if self.provider_login.form.is_some() {
            return;
        }
        let servers = match self
            .lifecycle
            .market()
            .provider_login_servers(provider.id())
        {
            Ok(servers) => servers,
            Err(error) => {
                let view = self.data_connections.view_mut(provider);
                view.message = Some(error);
                view.failed = true;
                cx.notify();
                return;
            }
        };
        let Some(first) = servers.first() else { return };
        let saved = match &self.data_connections.view(provider).map(|view| &view.saved) {
            Some(SavedLogin::Saved(saved)) => Some(saved.clone()),
            _ => None,
        };
        let server = saved
            .as_ref()
            .and_then(|saved| servers.iter().find(|server| server.id == saved.server_id))
            .map_or(first.id, |server| server.id);
        let inputs = schema
            .fields
            .iter()
            .filter(|field| field.id != "server" && field.id != "system")
            .map(|field| {
                let input = cx.new(|input_cx| {
                    InputState::new(window, input_cx)
                        .placeholder(field.label)
                        .masked(field.secret)
                });
                (field.id, input)
            })
            .collect::<Vec<_>>();
        if let Some((_, first)) = inputs.first() {
            first.update(cx, |input, input_cx| input.focus(window, input_cx));
        }
        self.provider_login.form = Some(ProviderLoginForm {
            provider,
            servers,
            server,
            systems: Systems::Discovering,
            system: saved.map(|saved| saved.system_name),
            dropdown: None,
            inputs,
            message: None,
        });
        if schema.discover_servers {
            self.discover_provider_systems(cx);
        }
        cx.notify();
    }

    pub(super) fn disconnect_provider(
        &mut self,
        action: &DisconnectProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_provider_login_operation(action.0, LoginOperation::Forgetting, None, window, cx);
    }

    pub(super) fn check_provider_logins(&mut self, window: &Window, cx: &mut Context<Self>) {
        for descriptor in aeris_market_runtime::published_provider_descriptors() {
            if matches!(descriptor.connect, ConnectMethod::VaultCredentials { .. }) {
                self.run_provider_login_operation(
                    ProviderId(descriptor.id),
                    LoginOperation::Checking,
                    None,
                    window,
                    cx,
                );
            }
        }
    }

    pub(super) fn cancel_provider_login(
        &mut self,
        _: &CancelProviderLogin,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_provider_login_form(cx);
    }

    fn close_provider_login_form(&mut self, cx: &mut Context<Self>) {
        if self.provider_login.form.take().is_some() {
            cx.notify();
        }
    }

    fn toggle_provider_login_dropdown(&mut self, dropdown: LoginDropdown, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.provider_login.form {
            form.dropdown = (form.dropdown != Some(dropdown)).then_some(dropdown);
            cx.notify();
        }
    }

    fn close_provider_login_dropdown(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.provider_login.form
            && form.dropdown.take().is_some()
        {
            cx.notify();
        }
    }

    fn select_provider_server(&mut self, server: &'static str, cx: &mut Context<Self>) {
        let Some(form) = &mut self.provider_login.form else {
            return;
        };
        form.dropdown = None;
        if form.server != server {
            form.server = server;
            form.system = None;
            form.message = None;
            self.discover_provider_systems(cx);
        }
        cx.notify();
    }

    fn select_provider_system(&mut self, system: String, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.provider_login.form {
            form.dropdown = None;
            form.system = Some(system);
            form.message = None;
            cx.notify();
        }
    }

    fn discover_provider_systems(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.provider_login.form else {
            return;
        };
        form.systems = Systems::Discovering;
        if self.provider_login.discovery.is_some() {
            return;
        }
        let provider = form.provider;
        let server = form.server;
        let market = self.lifecycle.market().clone();
        let work = cx
            .background_executor()
            .spawn(async move { market.discover_provider_servers(provider.id(), server) });
        self.provider_login.discovery = Some(cx.spawn(async move |terminal, cx| {
            let result = work.await;
            if let Err(error) = terminal.update(cx, |terminal, cx| {
                terminal.provider_login.discovery = None;
                terminal.apply_provider_systems(provider, server, result, cx);
            }) && terminal.upgrade().is_some()
            {
                diagnostic!("Workspace could not receive provider system discovery: {error}");
            }
        }));
    }

    fn apply_provider_systems(
        &mut self,
        provider: ProviderId,
        server: &'static str,
        result: Result<Vec<String>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.provider_login.form else {
            return;
        };
        if form.provider != provider || form.server != server {
            self.discover_provider_systems(cx);
            return;
        }
        form.systems = match result {
            Ok(systems) if systems.is_empty() => {
                Systems::Failed("This server offers no systems".to_string())
            }
            Ok(systems) => {
                form.system = retained_system(form.system.take(), &systems);
                Systems::Listed(systems)
            }
            Err(error) => Systems::Failed(error),
        };
        cx.notify();
    }

    fn sign_in_provider(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.provider_login.form else {
            return;
        };
        if self
            .provider_login
            .operations
            .contains_key(form.provider.id())
        {
            return;
        }
        let Some(system) = form.system.clone() else {
            form.message = Some("Choose a system".to_string());
            cx.notify();
            return;
        };
        let provider = form.provider;
        let Some(descriptor) = provider.descriptor() else {
            return;
        };
        let ConnectMethod::VaultCredentials { schema } = descriptor.connect else {
            return;
        };
        let fields = schema
            .fields
            .iter()
            .filter_map(|field| {
                let value = match field.id {
                    "server" => form.server.to_string(),
                    "system" => system.clone(),
                    id => form
                        .inputs
                        .iter()
                        .find(|(field_id, _)| *field_id == id)?
                        .1
                        .read(cx)
                        .value()
                        .to_string(),
                };
                Some((field.id, Zeroizing::new(value)))
            })
            .collect::<Vec<_>>();
        if fields.len() != schema.fields.len() {
            form.message = Some("Login form is incomplete".to_string());
            cx.notify();
            return;
        }
        form.message = None;
        self.run_provider_login_operation(
            provider,
            LoginOperation::SigningIn,
            Some(SubmittedLogin { fields }),
            window,
            cx,
        );
    }

    fn run_provider_login_operation(
        &mut self,
        provider: ProviderId,
        operation: LoginOperation,
        login: Option<SubmittedLogin>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.data_connections.view_mut(provider);
        if self.provider_login.operations.contains_key(provider.id()) {
            if operation != LoginOperation::Checking {
                view.message = Some("A login change is already running; wait for it.".to_string());
                view.failed = true;
                cx.notify();
            }
            return;
        }
        view.operation = Some(operation);
        if operation != LoginOperation::Checking {
            view.message = None;
            view.failed = false;
        }
        let market = self.lifecycle.market().clone();
        let work = cx.background_executor().spawn(async move {
            match (operation, login) {
                (LoginOperation::Checking, _) => market.saved_provider_login(provider.id()),
                (LoginOperation::Forgetting, _) => {
                    market.forget_provider_login(provider.id()).map(|()| None)
                }
                (LoginOperation::SigningIn, Some(login)) => {
                    login.submit(provider, |id, fields| market.sign_in_provider(id, fields))?;
                    market.saved_provider_login(provider.id())
                }
                (LoginOperation::SigningIn, None) => Err("Enter a login to sign in".to_string()),
            }
        });
        let task = cx.spawn_in(window, async move |terminal, cx| {
            let result = work.await;
            if let Err(error) = cx
                .update(|window, cx| {
                    terminal.update(cx, |terminal, cx| {
                        terminal.provider_login.operations.remove(provider.id());
                        terminal.finish_provider_login_operation(
                            provider, operation, result, window, cx,
                        );
                    })
                })
                .and_then(|result| result)
                && terminal.upgrade().is_some()
            {
                diagnostic!("Workspace could not receive provider login result: {error}");
            }
        });
        self.provider_login.operations.insert(provider.id(), task);
        cx.notify();
    }

    fn finish_provider_login_operation(
        &mut self,
        provider: ProviderId,
        operation: LoginOperation,
        result: Result<Option<SavedProviderLogin>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.data_connections.view_mut(provider);
        view.operation = None;
        match (operation, result) {
            (_, Ok(saved)) => {
                match (operation, &saved) {
                    (LoginOperation::Checking, _) | (LoginOperation::SigningIn, None) => {}
                    (LoginOperation::SigningIn, Some(login)) => {
                        view.message = Some(signed_in_message(login));
                        view.failed = false;
                    }
                    (LoginOperation::Forgetting, _) => {
                        view.message = Some("Login removed from this computer.".to_string());
                        view.failed = false;
                    }
                }
                view.saved = saved.map_or(SavedLogin::Missing, SavedLogin::Saved);
                if operation == LoginOperation::SigningIn
                    && self
                        .provider_login
                        .form
                        .as_ref()
                        .is_some_and(|form| form.provider == provider)
                {
                    self.provider_login.form = None;
                    if provider
                        .descriptor()
                        .is_some_and(|descriptor| descriptor.symbol_browser)
                    {
                        self.active_surface().update(cx, |surface, surface_cx| {
                            surface.choose_symbol_scope(
                                SymbolSearchScope::Provider(provider.id()),
                                window,
                                surface_cx,
                            );
                            surface.open_chrome_overlay(
                                ChromeOverlay::Instrument,
                                window,
                                surface_cx,
                            );
                        });
                    }
                }
            }
            (LoginOperation::SigningIn, Err(error)) => {
                if let Some(form) = &mut self.provider_login.form {
                    if form.provider == provider {
                        form.message = Some(error);
                    } else {
                        view.message = Some(error);
                        view.failed = true;
                    }
                } else {
                    view.message = Some(error);
                    view.failed = true;
                }
            }
            (LoginOperation::Checking, Err(error)) => {
                view.saved = SavedLogin::Unchecked;
                view.message = Some(error);
                view.failed = true;
            }
            (LoginOperation::Forgetting, Err(error)) => {
                view.message = Some(error);
                view.failed = true;
            }
        }
        cx.notify();
    }
}

fn signed_in_message(login: &SavedProviderLogin) -> String {
    format!(
        "Signed in to {} on {}. The login is saved on this computer.",
        login.system_name, login.server_label
    )
}

pub(super) fn provider_login_form_open(flow: &ProviderLoginFlow, provider: ProviderId) -> bool {
    flow.form
        .as_ref()
        .is_some_and(|form| form.provider == provider)
}

pub(super) fn provider_login_form(
    terminal: &Entity<TerminalApp>,
    flow: &ProviderLoginFlow,
    provider: ProviderId,
    view: &ProviderLoginView,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let form = flow
        .form
        .as_ref()
        .filter(|form| form.provider == provider)?;
    let ConnectMethod::VaultCredentials { schema } = provider.descriptor()?.connect else {
        return None;
    };
    let colors = theme.colors;
    let escape = terminal.clone();
    let signing_in = view.operation == Some(LoginOperation::SigningIn);
    let field = |control: AnyElement| {
        div()
            .w(px(accounts_panel::ACCOUNTS_FIELD_WIDTH))
            .child(control)
            .into_any_element()
    };
    let rows = schema
        .fields
        .iter()
        .filter_map(|definition| {
            let control = match definition.id {
                "server" => field(server_dropdown(terminal, form, theme).into_any_element()),
                "system" => field(system_dropdown(terminal, form, theme).into_any_element()),
                id => accounts_panel::accounts_field(
                    &form.inputs.iter().find(|(field_id, _)| *field_id == id)?.1,
                    theme,
                ),
            };
            let description = if definition.secret {
                "Kept only in this computer's protected credential storage"
            } else {
                ""
            };
            Some(described_setting_row(
                definition.label,
                [SharedString::from(description)],
                control,
                theme,
            ))
        })
        .collect::<Vec<_>>();
    Some(
        div()
            .id("provider_login_form")
            .flex()
            .flex_col()
            .on_key_down(move |event, _, cx| {
                if event.keystroke.key.eq_ignore_ascii_case("escape") {
                    escape.update(cx, TerminalApp::close_provider_login_form);
                    cx.stop_propagation();
                }
            })
            .children(rows)
            .children(discovery_failure(terminal, form, theme))
            .children(signing_in.then(|| {
                div()
                    .pb_2()
                    .text_sm()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(format!(
                        "Signing in to {}…",
                        form.system.as_deref().unwrap_or("provider")
                    ))
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

fn discovery_failure(
    terminal: &Entity<TerminalApp>,
    form: &ProviderLoginForm,
    theme: &AerisTheme,
) -> Option<impl IntoElement> {
    let Systems::Failed(error) = &form.systems else {
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
                Button::new("provider_login_retry_discovery", theme)
                    .variant(ButtonVariant::Secondary)
                    .label("Retry")
                    .on_click(move |_, _, cx| {
                        retry.update(cx, |terminal, terminal_cx| {
                            terminal.discover_provider_systems(terminal_cx);
                            terminal_cx.notify();
                        });
                    }),
            ),
    )
}

fn form_actions(
    terminal: &Entity<TerminalApp>,
    form: &ProviderLoginForm,
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
            Button::new("provider_login_cancel", theme)
                .variant(ButtonVariant::Secondary)
                .label("Cancel")
                .on_click(move |_, _, cx| {
                    cancel.update(cx, TerminalApp::close_provider_login_form);
                }),
        )
        .child(
            Button::new("provider_login_save", theme)
                .variant(ButtonVariant::Primary)
                .label(if signing_in { "Signing in" } else { "Connect" })
                .loading(signing_in)
                .disabled(signing_in || form.system.is_none())
                .on_click(move |_, window, cx| {
                    save.update(cx, |terminal, terminal_cx| {
                        terminal.sign_in_provider(window, terminal_cx);
                    });
                }),
        )
}

fn dropdown_trigger(
    terminal: &Entity<TerminalApp>,
    dropdown: LoginDropdown,
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
                terminal.toggle_provider_login_dropdown(dropdown, terminal_cx);
            });
        })
}

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
            dismiss.update(cx, TerminalApp::close_provider_login_dropdown);
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
    form: &ProviderLoginForm,
    theme: &AerisTheme,
) -> MenuAnchor {
    let open = form.dropdown == Some(LoginDropdown::Server);
    let menu = open.then(|| {
        select_menu("provider_login_server_menu", theme).children(
            form.servers.iter().enumerate().map(|(index, server)| {
                let choose = terminal.clone();
                let server_id = server.id;
                MenuRow::compact(("provider_login_server_option", index), server.label, theme)
                    .checked(form.server == server_id)
                    .on_click(move |_, _, cx| {
                        choose.update(cx, |terminal, terminal_cx| {
                            terminal.select_provider_server(server_id, terminal_cx);
                        });
                    })
            }),
        )
    });
    select_field(
        terminal,
        "provider_login_server_field",
        dropdown_trigger(
            terminal,
            LoginDropdown::Server,
            "provider_login_server_select",
            form.servers
                .iter()
                .find(|server| server.id == form.server)
                .map_or("Choose a server", |server| server.label)
                .to_string(),
            open,
            theme,
        ),
        menu,
    )
}

fn system_dropdown(
    terminal: &Entity<TerminalApp>,
    form: &ProviderLoginForm,
    theme: &AerisTheme,
) -> MenuAnchor {
    let systems = match &form.systems {
        Systems::Listed(systems) => systems.as_slice(),
        Systems::Discovering | Systems::Failed(_) => &[],
    };
    let open = form.dropdown == Some(LoginDropdown::System) && !systems.is_empty();
    let menu = open.then(|| {
        select_menu("provider_login_system_menu", theme).children(systems.iter().enumerate().map(
            |(index, system)| {
                let choose = terminal.clone();
                let chosen = system.clone();
                MenuRow::compact(
                    ("provider_login_system_option", index),
                    system.clone(),
                    theme,
                )
                .checked(form.system.as_ref() == Some(system))
                .on_click(move |_, _, cx| {
                    choose.update(cx, |terminal, terminal_cx| {
                        terminal.select_provider_system(chosen.clone(), terminal_cx);
                    });
                })
            },
        ))
    });
    let label = match (&form.systems, &form.system) {
        (Systems::Discovering, _) => "Finding systems on this server…".to_string(),
        (_, Some(system)) => system.clone(),
        (Systems::Listed(_), None) => "Choose a system".to_string(),
        (Systems::Failed(_), None) => "No systems found".to_string(),
    };
    let trigger = dropdown_trigger(
        terminal,
        LoginDropdown::System,
        "provider_login_system_select",
        label,
        open,
        theme,
    )
    .loading(matches!(form.systems, Systems::Discovering))
    .disabled(systems.is_empty());
    select_field(terminal, "provider_login_system_field", trigger, menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediscovery_keeps_an_offered_system_and_picks_a_lone_one() {
        let offered = vec!["Paper".to_string(), "Live".to_string()];
        assert_eq!(
            retained_system(Some("Live".to_string()), &offered),
            Some("Live".to_string())
        );
        assert_eq!(retained_system(Some("Gone".to_string()), &offered), None);
        assert_eq!(retained_system(None, &offered), None);
        assert_eq!(
            retained_system(None, &offered[..1]),
            Some("Paper".to_string())
        );
    }

    #[test]
    fn provider_login_form_writes_only_through_vault_seam() {
        let mut vault =
            std::collections::BTreeMap::from([("unrelated".to_string(), "preserved".to_string())]);
        let mut sign_in_calls = 0;
        for descriptor in aeris_market_runtime::published_provider_descriptors() {
            let ConnectMethod::VaultCredentials { schema } = descriptor.connect else {
                continue;
            };
            let login = SubmittedLogin {
                fields: schema
                    .fields
                    .iter()
                    .map(|field| (field.id, Zeroizing::new(format!("fixture-{}", field.id))))
                    .collect(),
            };
            assert_eq!(
                login
                    .values()
                    .iter()
                    .map(|field| field.id)
                    .collect::<Vec<_>>(),
                schema
                    .fields
                    .iter()
                    .map(|field| field.id)
                    .collect::<Vec<_>>()
            );
            let mut flow = ProviderLoginFlow::default();
            assert!(!provider_login_form_open(&flow, ProviderId(descriptor.id)));
            // Cancel takes the form without passing a submitted login to the command.
            assert!(flow.form.take().is_none());
            assert!(flow.operations.is_empty());
            assert_eq!(sign_in_calls, 0);
            assert_eq!(vault.len(), 1);
            login
                .submit(ProviderId(descriptor.id), |id, values| {
                    sign_in_calls += 1;
                    assert_eq!(values.len(), schema.fields.len());
                    assert!(
                        values
                            .iter()
                            .zip(schema.fields)
                            .all(|(value, field)| value.id == field.id)
                    );
                    vault.insert(id.to_string(), "vault-only fixture entry".to_string());
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                vault.get("unrelated").map(String::as_str),
                Some("preserved")
            );
            assert_eq!(
                vault.get(descriptor.id).map(String::as_str),
                Some("vault-only fixture entry")
            );
        }
        assert!(sign_in_calls > 0);
    }
}
