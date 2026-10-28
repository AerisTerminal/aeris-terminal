//! The sign-in gate. Before the first sign-in it is its own window, and the
//! market runtime, provider sessions, broker venue and workspace do not exist
//! yet; [`SignInGate`] releases them once, on the first usable session. After
//! a later sign-out it covers the workspace window, which then renders nothing
//! and takes no input until the account runtime reports a verified session.

use super::*;
use aeris_desktop::account::DesktopAccount;
use aeris_observability::diagnostic;

const SIGN_IN_COLUMN_WIDTH: f32 = 360.0;
const SIGN_IN_STATUS_HEIGHT: f32 = 40.0;
const SIGN_IN_WINDOW_WIDTH: f32 = 480.0;
const SIGN_IN_WINDOW_HEIGHT: f32 = 560.0;

/// What the gate offers, derived from the account presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SignInStage {
    /// Startup restore has not settled whether a saved session is usable.
    Verifying,
    /// The browser holds an open sign-in transaction.
    Waiting,
    /// No session; the browser has not been opened yet or the last attempt ended.
    Ready,
}

/// Startup progress the sign-in window reports instead of account actions.
#[derive(Clone, Copy)]
enum StartupNotice {
    None,
    Opening,
    Failed(&'static str),
}

pub(super) fn sign_in_stage(
    account: &aeris_desktop::account::AccountMenuState,
    verifying: bool,
) -> SignInStage {
    if verifying && account.error.is_none() {
        SignInStage::Verifying
    } else if account.authorizing() {
        SignInStage::Waiting
    } else {
        SignInStage::Ready
    }
}

/// The status line under the actions: the actionable error first, otherwise the
/// current step, otherwise a named recovery state such as an expired sign-in.
pub(super) fn sign_in_status(
    account: &aeris_desktop::account::AccountMenuState,
    stage: SignInStage,
) -> Option<String> {
    if let Some(error) = &account.error {
        return Some(error.clone());
    }
    let presentation = &account.presentation;
    match stage {
        SignInStage::Verifying => Some("Checking your saved sign-in…".to_string()),
        SignInStage::Waiting => Some("Finish signing in in your browser.".to_string()),
        SignInStage::Ready if presentation.pending => Some("Opening your browser…".to_string()),
        SignInStage::Ready if account.hides_identity() => None,
        SignInStage::Ready => Some(presentation.state.to_string()),
    }
}

/// The gate over the workspace window after a sign-out.
pub(super) fn sign_in_page(
    terminal: &Entity<TerminalApp>,
    account: &DesktopAccount,
    window: &Window,
    theme: &AerisTheme,
) -> Div {
    sign_in_surface(theme)
        .child(sign_in_title_bar(terminal, window, theme))
        .child(sign_in_body(account, StartupNotice::None, theme))
}

fn sign_in_surface(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_default))
        .font_family(aeris_design_system::platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
}

fn sign_in_body(account: &DesktopAccount, notice: StartupNotice, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let menu = account.menu_state();
    let stage = sign_in_stage(&menu, account.verification_pending());
    let (status, failed, actions) = match notice {
        StartupNotice::None => (
            sign_in_status(&menu, stage),
            menu.error.is_some(),
            sign_in_actions(account, &menu, stage, theme),
        ),
        StartupNotice::Opening => (
            Some("Opening your workspace…".to_string()),
            false,
            Vec::new(),
        ),
        StartupNotice::Failed(message) => (
            Some(format!("{message} Close Aeris Terminal and open it again.")),
            true,
            Vec::new(),
        ),
    };
    div().flex_1().flex().items_center().justify_center().child(
        div()
            .w(px(SIGN_IN_COLUMN_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .child(terminal_chrome::brand_logo_sized(px(56.0)))
            .child(
                div()
                    .mt_6()
                    .text_xl()
                    .font_weight(platform_font_weight(TypographyRole::Strong))
                    .child("Sign in to Aeris Terminal"),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_center()
                    .text_color(gpui_color(colors.text_secondary))
                    .child("Your Aeris account opens in your default browser."),
            )
            .child(
                div()
                    .mt_8()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(actions),
            )
            .child(
                div()
                    .mt_3()
                    .h(px(SIGN_IN_STATUS_HEIGHT))
                    .w_full()
                    .flex()
                    .items_start()
                    .justify_center()
                    .text_xs()
                    .text_center()
                    .text_color(gpui_color(if failed {
                        colors.danger
                    } else {
                        colors.text_muted
                    }))
                    .children(status),
            ),
    )
}

fn sign_in_title_bar(terminal: &Entity<TerminalApp>, window: &Window, theme: &AerisTheme) -> Div {
    div()
        .w_full()
        .h(px(WORKSPACE_TITLE_BAR_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .child(terminal_chrome::workspace_window_drag_region(
            div().id("sign_in_window_drag_region").h_full().flex_1(),
            terminal,
        ))
        .child(terminal_chrome::workspace_window_controls(
            terminal, window, theme,
        ))
}

fn sign_in_actions(
    account: &DesktopAccount,
    menu: &aeris_desktop::account::AccountMenuState,
    stage: SignInStage,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let pending = menu.presentation.pending;
    let action = |id: &'static str,
                  label: &'static str,
                  variant: ButtonVariant,
                  run: fn(&DesktopAccount) -> Result<(), String>| {
        let account = account.clone();
        Button::new(id, theme)
            .variant(variant)
            .button_size(ButtonSize::Lg)
            .full_width()
            .label(label)
            .disabled(pending)
            .on_click(move |_, window, _| {
                if let Err(error) = run(&account) {
                    diagnostic!("Aeris sign-in action degraded: {error}");
                }
                window.refresh();
            })
            .into_any_element()
    };
    match stage {
        SignInStage::Verifying => Vec::new(),
        SignInStage::Waiting => vec![
            action(
                "sign_in_reopen",
                "Open sign-in page again",
                ButtonVariant::Primary,
                DesktopAccount::reopen_browser,
            ),
            action(
                "sign_in_cancel",
                "Cancel",
                ButtonVariant::Outline,
                DesktopAccount::request_cancel,
            ),
        ],
        SignInStage::Ready => {
            let mut actions = vec![action(
                "sign_in_start",
                if menu.error.is_some() {
                    "Try again"
                } else {
                    "Sign in with browser"
                },
                ButtonVariant::Primary,
                DesktopAccount::request_sign_in,
            )];
            if menu.retry_sign_out {
                actions.push(action(
                    "sign_in_retry_sign_out",
                    "Retry sign-out",
                    ButtonVariant::Outline,
                    DesktopAccount::request_sign_out,
                ));
            }
            actions
        }
    }
}

#[derive(Clone, Copy)]
enum GatePhase {
    SignedOut,
    Starting,
    Failed(&'static str),
    Mounted,
    Closing,
}

/// Owner of the pre-sign-in process: the held signed-in startup and the local
/// owners the gate must stop itself if the user quits before signing in.
pub(super) struct SignInGate {
    account: DesktopAccount,
    startup: aeris_desktop::services::SignedInStart<SignedInStartup>,
    trading: aeris_trading_runtime::TradingService,
    account_service: aeris_account_runtime::AccountService,
    instance_lock: Option<Arc<aeris_platform_runtime::InstanceLock>>,
    phase: GatePhase,
}

impl SignInGate {
    pub(super) fn new(
        account: DesktopAccount,
        startup: SignedInStartup,
        trading: aeris_trading_runtime::TradingService,
        account_service: aeris_account_runtime::AccountService,
        instance_lock: Arc<aeris_platform_runtime::InstanceLock>,
    ) -> Self {
        Self {
            account,
            startup: aeris_desktop::services::SignedInStart::new(startup),
            trading,
            account_service,
            instance_lock: Some(instance_lock),
            phase: GatePhase::SignedOut,
        }
    }

    /// Stops what the gate owns, plus a runtime that finished starting after
    /// the user closed the gate. Waits on the account refresh fence like the
    /// workspace shutdown does.
    fn shutdown(&mut self, started: Option<SignedInRuntime>, cx: &App) -> Task<()> {
        self.phase = GatePhase::Closing;
        let account_refresh = self.account_service.begin_refresh_quiesce();
        let trading = self.trading.clone();
        let instance_lock = self.instance_lock.take();
        cx.background_executor().spawn(async move {
            if let Some(started) = started {
                stop_signed_in_owners(Some(&started.services.market), &started.context);
            }
            if let Err(error) =
                retain_account_refresh_quiesce_for_exit(account_refresh, "desktop exit")
            {
                diagnostic!("Aeris desktop shutdown failed: {error}");
                flush_diagnostics_before_exit();
                std::process::exit(1);
            }
            if let Err(error) = trading.shutdown(Duration::from_secs(2)) {
                diagnostic!("Aeris trading owner could not stop: {error}");
            }
            drop(instance_lock);
        })
    }
}

/// Closing the gate before the workspace mounts stops the gate's owners. A
/// close during startup is finished by [`finish_signed_in_start`].
fn close_gate(gate: &Rc<RefCell<SignInGate>>, cx: &App) -> Option<Task<()>> {
    let mut state = gate.borrow_mut();
    match state.phase {
        GatePhase::Mounted | GatePhase::Closing => None,
        GatePhase::Starting => {
            state.phase = GatePhase::Closing;
            None
        }
        GatePhase::SignedOut | GatePhase::Failed(_) => Some(state.shutdown(None, cx)),
    }
}

fn quit_after(shutdown: Task<()>, cx: &mut App) {
    cx.spawn(async move |cx| {
        shutdown.await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

pub(super) fn open_sign_in_window(gate: SignInGate, cx: &mut App) {
    let account = gate.account.clone();
    let gate = Rc::new(RefCell::new(gate));
    let quit_gate = Rc::clone(&gate);
    cx.on_app_quit(move |cx| {
        let shutdown = close_gate(&quit_gate, cx);
        async move {
            if let Some(shutdown) = shutdown {
                shutdown.await;
            }
        }
    })
    .detach();
    let closed_gate = Rc::clone(&gate);
    cx.on_window_closed(move |cx, _| {
        if cx.windows().is_empty()
            && let Some(shutdown) = close_gate(&closed_gate, cx)
        {
            quit_after(shutdown, cx);
        }
    })
    .detach();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(SIGN_IN_WINDOW_WIDTH), px(SIGN_IN_WINDOW_HEIGHT)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Aeris Terminal".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|_| SignInWindow {
            gate: Rc::clone(&gate),
            account,
        });
        view.update(cx, |view, cx| view.start_account_listener(window, cx));
        view
    });
    if let Err(error) = opened {
        diagnostic!("Aeris sign-in window could not open: {error}");
        if let Some(shutdown) = close_gate(&gate, cx) {
            quit_after(shutdown, cx);
        }
        return;
    }
    cx.activate(true);
}

struct SignInWindow {
    gate: Rc<RefCell<SignInGate>>,
    account: DesktopAccount,
}

impl SignInWindow {
    /// Follows the account session's change wakes. The first pass runs at
    /// once so a session that settled before registration is not missed.
    fn start_account_listener(&self, window: &mut Window, cx: &mut Context<Self>) {
        let wake = UiWake::default();
        let registration = wake.callback();
        if let Err(error) = self.account.register_change_wake(&registration) {
            diagnostic!("Aeris sign-in change wake unavailable: {error}");
        }
        cx.spawn_in(window, async move |view, cx| {
            loop {
                if view
                    .update_in(cx, |view, window, cx| view.account_changed(window, cx))
                    .is_err()
                {
                    break;
                }
                wake.notified().await;
            }
            drop(registration);
        })
        .detach();
    }

    fn account_changed(&self, window: &Window, cx: &mut Context<Self>) {
        cx.notify();
        let startup = {
            let mut state = self.gate.borrow_mut();
            if !matches!(state.phase, GatePhase::SignedOut) {
                return;
            }
            let Some(startup) = state.startup.release(self.account.authenticated()) else {
                return;
            };
            state.phase = GatePhase::Starting;
            startup
        };
        cx.notify();
        let started = cx
            .background_executor()
            .spawn(async move { start_signed_in_runtime(startup) });
        let gate = Rc::clone(&self.gate);
        let gate_window = window.window_handle();
        // Detached at app level so a gate closed mid-start still stops what
        // the start produced.
        cx.spawn(async move |_, cx| {
            let result = started.await;
            cx.update(|cx| finish_signed_in_start(&gate, result, gate_window, cx));
        })
        .detach();
    }
}

fn finish_signed_in_start(
    gate: &Rc<RefCell<SignInGate>>,
    result: Result<SignedInRuntime, &'static str>,
    gate_window: gpui::AnyWindowHandle,
    cx: &mut App,
) {
    let mut state = gate.borrow_mut();
    let closing = matches!(state.phase, GatePhase::Closing);
    match result {
        Ok(started) if closing => {
            let shutdown = state.shutdown(Some(started), cx);
            drop(state);
            quit_after(shutdown, cx);
        }
        Ok(started) => {
            let Some(instance_lock) = state.instance_lock.clone() else {
                return;
            };
            state.phase = GatePhase::Mounted;
            drop(state);
            let SignedInRuntime {
                configured,
                services,
                context,
            } = started;
            let lifecycle = DesktopLifecycle::new(services, context, instance_lock);
            mount_desktop(configured, lifecycle, None, cx);
            gate.borrow_mut().instance_lock = None;
            if let Err(error) = gate_window.update(cx, |_, window, _| window.remove_window()) {
                diagnostic!("Aeris sign-in window could not close: {error}");
            }
        }
        Err(_) if closing => {
            let shutdown = state.shutdown(None, cx);
            drop(state);
            quit_after(shutdown, cx);
        }
        Err(message) => {
            state.phase = GatePhase::Failed(message);
            drop(state);
            if let Err(error) = gate_window.update(cx, |_, window, _| window.refresh()) {
                diagnostic!("Aeris sign-in window could not refresh: {error}");
            }
        }
    }
}

impl Render for SignInWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = AerisTheme::dark();
        let notice = match self.gate.borrow().phase {
            GatePhase::SignedOut => StartupNotice::None,
            GatePhase::Starting | GatePhase::Mounted | GatePhase::Closing => StartupNotice::Opening,
            GatePhase::Failed(message) => StartupNotice::Failed(message),
        };
        sign_in_surface(&theme).child(sign_in_body(&self.account, notice, &theme))
    }
}

#[cfg(test)]
mod tests {
    use super::{SignInStage, sign_in_stage, sign_in_status};
    use aeris_desktop::account::{AccountMenuState, AccountPresentation};

    fn menu(action: &'static str, state: &'static str) -> AccountMenuState {
        AccountMenuState {
            presentation: AccountPresentation {
                action,
                state,
                plan: "No plan",
                detail: String::new(),
                display_name: String::new(),
                email: String::new(),
                photo_url: String::new(),
                pending: false,
            },
            error: None,
            retry_sign_out: false,
            sign_in_enabled: true,
        }
    }

    #[test]
    fn startup_restore_shows_verification_without_actions() {
        let signed_out = menu("Sign in", "Signed out");
        assert_eq!(sign_in_stage(&signed_out, true), SignInStage::Verifying);
        assert_eq!(
            sign_in_status(&signed_out, SignInStage::Verifying).as_deref(),
            Some("Checking your saved sign-in…")
        );
    }

    #[test]
    fn failed_restore_offers_sign_in_with_its_error() {
        let mut failed = menu("Sign in", "Signed out");
        failed.error = Some("account request timed out; try again".to_string());
        assert_eq!(sign_in_stage(&failed, true), SignInStage::Ready);
        assert_eq!(
            sign_in_status(&failed, SignInStage::Ready).as_deref(),
            Some("account request timed out; try again")
        );
    }

    #[test]
    fn browser_wait_and_recovery_states_name_themselves() {
        let waiting = menu("Waiting for browser", "Waiting for browser sign-in");
        assert_eq!(sign_in_stage(&waiting, false), SignInStage::Waiting);
        assert_eq!(
            sign_in_status(&waiting, SignInStage::Waiting).as_deref(),
            Some("Finish signing in in your browser.")
        );
        let expired = menu("Sign in", "Sign-in required");
        assert_eq!(
            sign_in_status(&expired, SignInStage::Ready).as_deref(),
            Some("Sign-in required")
        );
        let signed_out = menu("Sign in", "Signed out");
        assert_eq!(sign_in_status(&signed_out, SignInStage::Ready), None);
    }
}
