use std::time::Duration;

use gpui::{
    App, Context, Entity, IntoElement, MouseButton, Render, RenderOnce, Role, Window, div,
    prelude::*, px, relative,
};
use gpui_base::{Easing, Transition, transition};
use tradingplot_design_system::{
    RadiusToken, TradingPlotTheme, TypographyRole, platform_font_family,
};

use crate::desktop::native_ui::platform_font_weight;
use crate::{
    desktop::native_ui::theme::gpui_color,
    desktop::terminal_chrome::{brand_mark_sized, onboarding_title_bar},
};

pub(super) struct OnboardingApp {
    theme: TradingPlotTheme,
    polling: bool,
    launch_error: Option<String>,
    terminal: Option<Entity<crate::desktop::TerminalApp>>,
    loading: bool,
}

impl OnboardingApp {
    pub(super) fn new() -> Self {
        Self {
            theme: TradingPlotTheme::dark(),
            polling: false,
            launch_error: None,
            terminal: None,
            loading: false,
        }
    }

    fn begin_sign_in(cx: &mut App) {
        if let Some(account) = tradingplot_desktop::account::DesktopAccount::shared()
            && let Err(error) = account.request_sign_in()
        {
            eprintln!("TradingPlot sign-in degraded: {error}");
        }
        cx.refresh_windows();
    }

    fn reopen_sign_in(cx: &mut App) {
        if let Some(account) = tradingplot_desktop::account::DesktopAccount::shared()
            && let Err(error) = account.reopen_browser()
        {
            eprintln!("TradingPlot sign-in browser reopen degraded: {error}");
        }
        cx.refresh_windows();
    }

    pub(super) fn has_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    fn start_account_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.polling || self.launch_error.is_some() || self.terminal.is_some() {
            return;
        }
        self.polling = true;
        cx.spawn_in(window, async move |screen, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let Ok(finished) = screen.update_in(cx, |screen, _, screen_cx| {
                    let Some(account) = tradingplot_desktop::account::DesktopAccount::shared()
                    else {
                        if screen.launch_error.is_none() {
                            screen.launch_error = Some(
                                "Sign-in service is unavailable. Restart TradingPlot.".to_string(),
                            );
                            screen.loading = false;
                            screen_cx.notify();
                        }
                        return false;
                    };
                    let changed = account.poll();
                    if account.authenticated() && screen.launch_error.is_none() {
                        screen.loading = true;
                        screen_cx.notify();
                        return true;
                    }
                    if changed {
                        screen_cx.notify();
                    }
                    false
                }) else {
                    break;
                };
                if !finished {
                    continue;
                }
                {
                    // runtime startup, workspace restore, preferences, and provider worker
                    // startup must never block the window's event loop.
                    let configured = cx
                        .background_executor()
                        .spawn(async { crate::desktop::configured_market_workers() })
                        .await;
                    // Retired completion fencing: sign-out, cancellation, or
                    // expiry may have landed while startup ran. Drop the
                    // just-built workers on this background task and resume
                    // waiting instead of attaching stale state to the window.
                    let still_authenticated = tradingplot_desktop::account::DesktopAccount::shared(
                    )
                    .is_some_and(|account| {
                        let _ = account.poll();
                        account.authenticated()
                    });
                    if !still_authenticated {
                        drop(configured);
                        let _ = screen.update_in(cx, |screen, _, screen_cx| {
                            screen.loading = false;
                            screen_cx.notify();
                        });
                        continue;
                    }
                    let mounted = screen.update_in(cx, |screen, window, screen_cx| {
                        screen.loading = false;
                        match configured {
                            Ok(Some(configured)) => {
                                let lifecycle = crate::desktop::DesktopLifecycle::new();
                                // The terminal keeps the same window and account
                                // client; a sign-out that races this mount is
                                // observed on the next poll and returns to
                                // onboarding without stale state.
                                screen.terminal = crate::desktop::mount_desktop(
                                    configured,
                                    lifecycle,
                                    Some(window),
                                    screen_cx,
                                );
                            }
                            Ok(None) => {
                                // Diagnostic commands (readiness/conformance)
                                // already ran on the background worker.
                                crate::desktop::quit_after_account_refresh_quiesce(screen_cx);
                            }
                            Err(error) => {
                                screen.launch_error = Some(error);
                                screen.polling = false;
                            }
                        }
                        screen_cx.notify();
                    });
                    if mounted.is_err() {
                        break;
                    }
                    break;
                }
            }
        })
        .detach();
    }
}

const ONBOARDING_BUTTON_HEIGHT: f32 = 40.0;
const ONBOARDING_BUTTON_PRESSED_SCALE: f32 = 0.97;
const ONBOARDING_BUTTON_PRESS_DURATION: Duration = Duration::from_millis(200);

#[derive(IntoElement)]
struct OnboardingButton {
    id: &'static str,
    label: &'static str,
    primary: bool,
    pending: bool,
    theme: TradingPlotTheme,
}

impl RenderOnce for OnboardingButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let fill = if self.primary {
            colors.primary
        } else {
            colors.surface_secondary
        };
        let foreground = if self.primary {
            colors.primary_foreground
        } else {
            colors.text_primary
        };
        let pressed_state =
            window.use_keyed_state((gpui::ElementId::from(self.id), "press"), cx, |_, _| false);
        let pressed = *pressed_state.read(cx);
        let scale = transition(
            (self.id, "press-scale"),
            if pressed {
                ONBOARDING_BUTTON_PRESSED_SCALE
            } else {
                1.0
            },
            Transition::new(ONBOARDING_BUTTON_PRESS_DURATION).easing(Easing::EaseOut),
            window,
            cx,
        );
        let press_state = pressed_state.clone();
        let release_state = pressed_state.clone();
        let release_out_state = pressed_state;

        div()
            .w_full()
            .h(px(ONBOARDING_BUTTON_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id(self.id)
                    .relative()
                    .w(relative(scale))
                    .h(px(ONBOARDING_BUTTON_HEIGHT * scale))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(f32::from(RadiusToken::Default.logical_pixels()) * scale))
                    .border_1()
                    .border_color(gpui_color(if self.primary {
                        colors.primary
                    } else {
                        colors.border
                    }))
                    .bg(gpui_color(fill))
                    .text_color(gpui_color(foreground))
                    .text_size(px(14.0 * scale))
                    .font_weight(platform_font_weight(TypographyRole::Normal))
                    .when(!self.pending, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |button| button.bg(gpui_color(colors.active_bg.over(fill))))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                press_state.update(cx, |pressed, cx| {
                                    *pressed = true;
                                    cx.notify();
                                });
                            })
                            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                                release_state.update(cx, |pressed, cx| {
                                    *pressed = false;
                                    cx.notify();
                                });
                            })
                            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                                release_out_state.update(cx, |pressed, cx| {
                                    *pressed = false;
                                    cx.notify();
                                });
                            })
                            .on_click(|_, _, cx| OnboardingApp::begin_sign_in(cx))
                    })
                    .when(self.pending, gpui::Styled::cursor_not_allowed)
                    .child(self.label),
            )
    }
}

fn onboarding_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    pending: bool,
    theme: &TradingPlotTheme,
) -> impl IntoElement {
    OnboardingButton {
        id,
        label,
        primary,
        pending,
        theme: *theme,
    }
}

impl Render for OnboardingApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(terminal) = &self.terminal {
            return terminal.clone().into_any_element();
        }
        self.start_account_poll(window, cx);
        if tradingplot_desktop::account::DesktopAccount::shared()
            .is_some_and(|account| account.verification_pending())
        {
            return session_verification_surface(window, &self.theme).into_any_element();
        }
        if let Some(error) = &self.launch_error {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .bg(gpui_color(self.theme.colors.surface))
                .text_color(gpui_color(self.theme.colors.text_primary))
                .child(error.clone())
                .child(
                    div()
                        .id("retry_workspace")
                        .cursor_pointer()
                        .mt_3()
                        .child("Retry")
                        .on_click(cx.listener(|screen, _, _, cx| {
                            screen.launch_error = None;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        if self.loading
            || tradingplot_desktop::account::DesktopAccount::shared()
                .is_some_and(|account| account.authenticated())
        {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_color(self.theme.colors.surface))
                .text_color(gpui_color(self.theme.colors.text_primary))
                .child("Signed in. Loading your workspace…")
                .into_any_element();
        }
        onboarding_surface(window, &self.theme, None).into_any_element()
    }
}

fn session_verification_surface(window: &Window, theme: &TradingPlotTheme) -> gpui::Div {
    let colors = theme.colors;
    div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_primary))
        .font_family(platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .child(onboarding_title_bar(window, theme))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .size(px(80.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(brand_mark_sized(px(72.0))),
                )
                .child(
                    div()
                        .mt_6()
                        .text_lg()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child("Verifying your session…"),
                )
                .child(
                    div()
                        .mt_2()
                        .text_sm()
                        .text_color(gpui_color(colors.text_secondary))
                        .child("Checking your existing TradingPlot credentials."),
                ),
        )
}

fn onboarding_status(
    theme: &TradingPlotTheme,
    launch_error: Option<&String>,
    account_error: Option<&str>,
    request_pending: bool,
    authorizing: bool,
) -> gpui::Div {
    let colors = theme.colors;
    let detail = launch_error
        .cloned()
        .or_else(|| account_error.map(str::to_string))
        .or_else(|| {
            (request_pending && !authorizing).then(|| "Opening secure sign-in…".to_string())
        });
    let can_reopen = authorizing && launch_error.is_none() && account_error.is_none();
    let content = if can_reopen {
        div()
            .id("onboarding_reopen_sign_in")
            .role(Role::Button)
            .aria_label("Open the browser to complete sign-in")
            .tab_index(0)
            .cursor_pointer()
            .text_color(gpui_color(colors.primary))
            .hover(move |status| status.text_color(gpui_color(colors.text_primary)))
            .on_key_down(|event, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    OnboardingApp::reopen_sign_in(cx);
                    cx.stop_propagation();
                }
            })
            .on_click(|_, _, cx| OnboardingApp::reopen_sign_in(cx))
            .child("Complete sign-in in your browser ↗")
            .into_any_element()
    } else {
        div()
            .text_color(gpui_color(
                if launch_error.is_some() || account_error.is_some() {
                    colors.danger
                } else {
                    colors.text_secondary
                },
            ))
            .child(detail.unwrap_or_default())
            .into_any_element()
    };
    div()
        .mt_3()
        .h(px(28.0))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_center()
        .child(content)
}

pub(super) fn onboarding_surface(
    window: &Window,
    theme: &TradingPlotTheme,
    launch_error: Option<&String>,
) -> gpui::Div {
    let colors = theme.colors;
    let account = tradingplot_desktop::account::DesktopAccount::shared();
    let presentation = account
        .as_ref()
        .map(tradingplot_desktop::account::DesktopAccount::presentation);
    let authorizing = presentation
        .as_ref()
        .is_some_and(|view| view.action == "Waiting for browser");
    let request_pending = presentation.as_ref().is_some_and(|view| view.pending);
    let pending = request_pending || authorizing;
    let account_error = account
        .as_ref()
        .and_then(tradingplot_desktop::account::DesktopAccount::error);
    let status = onboarding_status(
        theme,
        launch_error,
        account_error.as_deref(),
        request_pending,
        authorizing,
    );

    div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_color(colors.surface))
            .text_color(gpui_color(colors.text_primary))
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .child(onboarding_title_bar(window, theme))
            .child(
                div()
                    .w(px(384.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .size(px(96.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(brand_mark_sized(px(88.0))),
                    )
                    .child(
                        div()
                            .mt_8()
                            .text_3xl()
                            .font_weight(platform_font_weight(TypographyRole::Strong))
                            .child("TRADINGPLOT"),
                    )
                    .child(
                        div()
                            .mt_3()
                            .text_lg()
                            .text_color(gpui_color(colors.text_secondary))
                            .child("Trade with clarity. Built for speed."),
                    )
                    .child(
                        div()
                            .mt(px(56.0))
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(onboarding_button(
                                "onboarding_sign_in",
                                "Sign In",
                                true,
                                pending,
                                theme,
                            ))
                            .child(onboarding_button(
                                "onboarding_sign_up",
                                "Create Account",
                                false,
                                pending,
                                theme,
                            )),
                    )
                    .child(status)
                    .child(
                        div()
                            .mt_4()
                            .text_xs()
                            .text_center()
                            .text_color(gpui_color(colors.text_muted))
                            .child("Authentication is required. Market data remains off until sign-in completes."),
                    ),
            )
}
