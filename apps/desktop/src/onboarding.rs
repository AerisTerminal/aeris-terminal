use std::time::Duration;

use axiusflow_design_system::AxiusflowTheme;
use gpui::{App, Context, FontWeight, Render, Window, div, prelude::*, px};

use crate::terminal_chrome::{brand_mark_sized, gpui_color, onboarding_title_bar};

pub(super) struct OnboardingApp {
    theme: AxiusflowTheme,
    polling: bool,
    launch_error: Option<String>,
}

impl OnboardingApp {
    pub(super) fn new() -> Self {
        Self {
            theme: AxiusflowTheme::dark(),
            polling: false,
            launch_error: None,
        }
    }

    fn begin_sign_in(cx: &mut App) {
        if let Some(account) = axiusflow_desktop::account::DesktopAccount::shared()
            && let Err(error) = account.request_sign_in()
        {
            eprintln!("Axiusflow sign-in degraded: {error}");
        }
        cx.refresh_windows();
    }

    fn start_account_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn_in(window, async move |screen, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let finished = screen
                    .update_in(cx, |screen, _, screen_cx| {
                        let Some(account) = axiusflow_desktop::account::DesktopAccount::shared()
                        else {
                            screen.launch_error = Some(
                                "Sign-in service is unavailable. Restart Axiusflow.".to_string(),
                            );
                            screen_cx.notify();
                            return false;
                        };
                        let changed = account.poll();
                        if account.authenticated() {
                            match crate::relaunch_authenticated_desktop() {
                                Ok(()) => {
                                    screen_cx.quit();
                                    return true;
                                }
                                Err(error) => {
                                    screen.launch_error = Some(error);
                                }
                            }
                        }
                        if changed {
                            screen_cx.notify();
                        }
                        false
                    })
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        })
        .detach();
    }
}

fn onboarding_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    pending: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let fill = if primary {
        colors.primary
    } else {
        colors.surface_secondary
    };
    let foreground = if primary {
        colors.primary_foreground
    } else {
        colors.text_primary
    };
    div()
        .id(id)
        .w_full()
        .h(px(40.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .border_1()
        .border_color(gpui_color(if primary {
            colors.primary
        } else {
            colors.border
        }))
        .bg(gpui_color(fill))
        .text_color(gpui_color(foreground))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .when(!pending, |button| {
            button
                .cursor_pointer()
                .hover(move |button| button.bg(gpui_color(colors.active_bg.over(fill))))
                .on_click(|_, _, cx| OnboardingApp::begin_sign_in(cx))
        })
        .when(pending, gpui::Styled::cursor_not_allowed)
        .child(label)
}

impl Render for OnboardingApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_account_poll(window, cx);
        onboarding_surface(window, &self.theme, self.launch_error.as_ref())
    }
}

pub(super) fn onboarding_surface(
    window: &Window,
    theme: &AxiusflowTheme,
    launch_error: Option<&String>,
) -> gpui::Div {
    let colors = theme.colors;
    let account = axiusflow_desktop::account::DesktopAccount::shared();
    let presentation = account
        .as_ref()
        .map(axiusflow_desktop::account::DesktopAccount::presentation);
    let pending = presentation.as_ref().is_some_and(|view| view.pending)
        || presentation
            .as_ref()
            .is_some_and(|view| view.action == "Waiting for browser");
    let detail = launch_error
        .cloned()
        .or_else(|| account.and_then(|account| account.error()))
        .or_else(|| pending.then(|| "Complete sign-in in your browser.".to_string()));

    div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_color(colors.surface))
            .text_color(gpui_color(colors.text_primary))
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
                            .font_weight(FontWeight::BOLD)
                            .child("AXIUSFLOW"),
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
                                if pending { "Waiting for browser…" } else { "Sign In" },
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
                    .children(detail.map(|detail| {
                        div()
                            .mt_4()
                            .text_sm()
                            .text_center()
                            .text_color(gpui_color(if launch_error.is_some() {
                                colors.danger
                            } else {
                                colors.text_secondary
                            }))
                            .child(detail)
                    }))
                    .child(
                        div()
                            .mt_8()
                            .text_xs()
                            .text_center()
                            .text_color(gpui_color(colors.text_muted))
                            .child("Authentication is required. Market data remains off until sign-in completes."),
                    ),
            )
}
