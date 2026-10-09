//! The platform menu: the one dropdown under the header avatar. It stacks every
//! application-level section in a single panel: account (only while sign-in exists), Theme
//! with a preview card per mode, Window with frameless mode, and About with version, system,
//! build mode and updates.

use aeris_observability::diagnostic;
use std::rc::Rc;

use super::*;
use aeris_design_system::ThemeMode;
use gpui_base::Button as BaseButton;

const PLATFORM_MENU_WIDTH: f32 = 320.0;
const PLATFORM_MENU_GAP: f32 = 4.0;
const IDENTITY_HEIGHT: f32 = 52.0;
const ACCOUNT_ERROR_HEIGHT: f32 = 24.0;
const SECTION_TITLE_HEIGHT: f32 = 28.0;
const SECTION_BOTTOM_PADDING: f32 = 12.0;
const THEME_PREVIEW_HEIGHT: f32 = 72.0;
const THEME_CARD_GAP: f32 = 6.0;
const THEME_CARD_LABEL_HEIGHT: f32 = 16.0;
const THEME_SECTION_HEIGHT: f32 = SECTION_TITLE_HEIGHT
    + THEME_PREVIEW_HEIGHT
    + THEME_CARD_GAP
    + THEME_CARD_LABEL_HEIGHT
    + SECTION_BOTTOM_PADDING;
const WINDOW_SETTING_HEIGHT: f32 = 40.0;
const WINDOW_SECTION_HEIGHT: f32 =
    SECTION_TITLE_HEIGHT + WINDOW_SETTING_HEIGHT + SECTION_BOTTOM_PADDING;
const ABOUT_BRAND_HEIGHT: f32 = 40.0;
const ABOUT_DETAIL_HEIGHT: f32 = 26.0;
const ABOUT_UPDATE_HEIGHT: f32 = 44.0;
/// Room for the four provider notices at the menu width; the block scrolls
/// rather than clipping if a font fallback wraps them onto more lines.
const ABOUT_NOTICES_HEIGHT: f32 = 200.0;
const THEME_MODES: [ThemeMode; 2] = [ThemeMode::Light, ThemeMode::Dark];

type ThemeSelect = Rc<dyn Fn(ThemeMode, &mut Window, &mut App)>;
type UpdateActivate = Rc<dyn Fn(UpdateAction, &mut Window, &mut App)>;
type FramelessToggle = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// Circular account avatar for the header toolbar. Signed-out sessions keep
/// the asset-free muted person glyph; signed-in sessions render verified
/// initials with the profile photo overlaid when the user record has one.
/// The photo loads asynchronously through GPUI's image cache: while loading
/// or on failure the element paints nothing and the initials underneath
/// stay visible, so image faults never break authentication or the avatar.
/// Stale loads cannot win: the source derives from the current photo URL
/// every frame, and the cache keys by URI.
pub(super) fn account_avatar_button(
    terminal: &Entity<TerminalApp>,
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let presentation = &account.presentation;
    let tooltip = if account.signed_in() && !presentation.display_name.is_empty() {
        format!("Account — {}", presentation.display_name)
    } else if account.signed_in() {
        "Account".to_string()
    } else {
        "Account — Sign in".to_string()
    };
    let toggle_terminal = terminal.clone();
    chrome_tooltip(
        "account_avatar",
        tooltip,
        Button::new("account_avatar", theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Lg)
            .aria_label("Profile menu")
            .on_click(move |event, _, cx| {
                let anchor = event.position();
                toggle_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.toggle_platform_menu_at(anchor, terminal_cx);
                });
            })
            .leading(
                div()
                    .size(px(26.0))
                    .flex_none()
                    .rounded_full()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(account_avatar_face(account, theme)),
            )
            .caret(header_icon(HugeIcon::ChevronDown)),
        theme,
    )
}

fn account_avatar_face(
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    if !account.signed_in() {
        let glyph = colors.text_muted;
        return div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .child(div().size(px(7.0)).rounded_full().bg(gpui_color(glyph)))
            .child(
                div()
                    .w(px(14.0))
                    .h(px(6.0))
                    .rounded_t(px(7.0))
                    .bg(gpui_color(glyph)),
            )
            .into_any_element();
    }
    let presentation = &account.presentation;
    div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Normal))
                .text_color(gpui_color(colors.text_primary))
                .child(aeris_desktop::account::profile_initials(
                    &presentation.display_name,
                    &presentation.email,
                )),
        )
        .children(
            aeris_desktop::account::has_profile_photo(&presentation.photo_url).then(|| {
                img(presentation.photo_url.as_str())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover)
            }),
        )
        .into_any_element()
}

/// The platform menu anchored under the avatar's click point. Every section
/// has a fixed height, so the panel clamps into the viewport without
/// measuring a frame first.
pub(super) fn platform_menu_layer(
    terminal: &Entity<TerminalApp>,
    account: &aeris_desktop::account::AccountMenuState,
    anchor: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    update: Option<&UpdatePresentation>,
    window_frame: chart_chrome::WindowFrame,
    theme: &AerisTheme,
) -> AnyElement {
    let header = identity_header(account, theme);
    let actions = account_actions(account);
    let details = about_details(account, update);
    let layout = PlatformMenuLayout {
        identity: header.is_some(),
        actions: actions.len(),
        error: account.error.is_some(),
        about_details: details.len(),
    };
    let header_bottom = WORKSPACE_TITLE_BAR_HEIGHT + PLATFORM_MENU_GAP;
    let panel_chrome = menu_panel_chrome_height(theme, MenuScale::BASE, px(ROOT_REM_PX));
    let panel_size = size(
        px(PLATFORM_MENU_WIDTH),
        px(layout.height(f32::from(panel_chrome))),
    );
    let origin = clamp_panel_origin(
        point(
            anchor.x,
            anchor.y.max(px(header_bottom)) + px(PLATFORM_MENU_GAP),
        ),
        viewport,
        panel_size,
    );
    let animation_origin =
        PopupAnimationOrigin::from_trigger(anchor, Bounds::new(origin, panel_size));
    let theme_terminal = terminal.clone();
    let select_theme: ThemeSelect = Rc::new(move |mode, window, cx| {
        theme_terminal.update(cx, |terminal, terminal_cx| {
            terminal.set_theme_mode(mode, window, terminal_cx);
        });
    });
    let frameless_terminal = terminal.clone();
    let toggle_frameless: FramelessToggle = Rc::new(move |frameless, _, cx| {
        let window_frame = if frameless {
            chart_chrome::WindowFrame::Frameless
        } else {
            chart_chrome::WindowFrame::Framed
        };
        frameless_terminal.update(cx, |terminal, terminal_cx| {
            terminal.set_window_frame(window_frame, terminal_cx);
        });
    });
    let update_terminal = terminal.clone();
    let run_update: UpdateActivate = Rc::new(move |action, _, cx| {
        update_terminal.update(cx, |terminal, terminal_cx| match action {
            UpdateAction::Restart => terminal.restart_to_update(terminal_cx),
            UpdateAction::Retry => terminal.retry_update_check(terminal_cx),
        });
    });
    let panel = MenuPanel::new("platform_menu", MenuPlacement::At(origin), theme)
        .width(px(PLATFORM_MENU_WIDTH))
        .animate_from(animation_origin)
        .children(header)
        .when(layout.identity, MenuPanel::separator)
        .children(account_action_rows(terminal, account, &actions, theme))
        .children(account.error.as_deref().map(|error| {
            div()
                .h(px(ACCOUNT_ERROR_HEIGHT))
                .flex()
                .items_center()
                .px_2()
                .text_xs()
                .text_color(gpui_color(theme.colors.danger))
                .child(error.to_string())
        }))
        .when(layout.has_account_rows(), MenuPanel::separator)
        .child(theme_section(theme, &select_theme))
        .separator()
        .child(window_section(
            window_frame.frameless(),
            &toggle_frameless,
            theme,
        ))
        .separator()
        .child(about_section(
            details,
            &about_update_view(update, theme),
            &run_update,
            theme,
        ));
    platform_menu_scrim(terminal, panel)
}

fn platform_menu_scrim(terminal: &Entity<TerminalApp>, panel: MenuPanel) -> AnyElement {
    let dismiss = terminal.clone();
    let dismiss_right = terminal.clone();
    div()
        .id("platform_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_click(move |_, window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_platform_menu(window, terminal_cx);
            });
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            dismiss_right.update(cx, |terminal, terminal_cx| {
                terminal.close_platform_menu(window, terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(panel)
        .into_any_element()
}

/// Keeps a panel of known size inside the viewport with the overlay edge margin.
fn clamp_panel_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    panel: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    let margin = px(OVERLAY_EDGE_MARGIN);
    let max_x = (viewport.width - panel.width - margin).max(margin);
    let max_y = (viewport.height - panel.height - margin).max(margin);
    point(
        origin.x.max(margin).min(max_x),
        origin.y.max(margin).min(max_y),
    )
}

#[derive(Clone, Copy, Debug)]
struct PlatformMenuLayout {
    identity: bool,
    actions: usize,
    error: bool,
    about_details: usize,
}

impl PlatformMenuLayout {
    const fn has_account_rows(self) -> bool {
        self.actions > 0 || self.error
    }

    /// Content height plus `panel_chrome`, the panel's inset and border.
    fn height(self, panel_chrome: f32) -> f32 {
        let separator = CHART_CONTEXT_MENU_SEPARATOR_HEIGHT;
        let identity = if self.identity {
            IDENTITY_HEIGHT + separator
        } else {
            0.0
        };
        let rows = self.actions.to_f32().unwrap_or_default() * CHART_CONTEXT_MENU_ROW_HEIGHT;
        let error = if self.error {
            ACCOUNT_ERROR_HEIGHT
        } else {
            0.0
        };
        let account_separator = if self.has_account_rows() {
            separator
        } else {
            0.0
        };
        panel_chrome
            + identity
            + rows
            + error
            + account_separator
            + THEME_SECTION_HEIGHT
            + separator
            + WINDOW_SECTION_HEIGHT
            + separator
            + about_section_height(self.about_details)
    }
}

fn about_section_height(details: usize) -> f32 {
    SECTION_TITLE_HEIGHT
        + ABOUT_BRAND_HEIGHT
        + details.to_f32().unwrap_or_default() * ABOUT_DETAIL_HEIGHT
        + ABOUT_NOTICES_HEIGHT
        + ABOUT_UPDATE_HEIGHT
        + SECTION_BOTTOM_PADDING
}

/// Identity header. Signed-in sessions show the verified name and avatar;
/// other visible states name themselves with their detail. Signed-out and
/// development sessions show none; development mode is reported under About.
fn identity_header(
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    if !aeris_desktop::account::AUTH_BACKEND_CONFIGURED || account.hides_identity() {
        return None;
    }
    let colors = theme.colors;
    let presentation = &account.presentation;
    let header = div().h(px(IDENTITY_HEIGHT)).flex().items_center().px_2();
    if account.signed_in() {
        let name = if presentation.display_name.is_empty() {
            presentation.state.to_string()
        } else {
            presentation.display_name.clone()
        };
        return Some(
            header
                .gap_3()
                .child(
                    div()
                        .size(px(28.0))
                        .flex_none()
                        .rounded_full()
                        .overflow_hidden()
                        .child(account_avatar_face(account, theme)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(gpui_color(colors.text_primary))
                        .child(name),
                )
                .into_any_element(),
        );
    }
    // The state names the session; the action rows below own the verbs.
    Some(
        header
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(gpui_color(colors.text_primary))
                    .child(presentation.state),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(presentation.detail.clone()),
            )
            .into_any_element(),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccountAction {
    SignIn,
    Cancel,
    SignOut,
    RetrySignOut,
    Reopen,
    ManageProfile,
}

impl AccountAction {
    const fn id(self) -> &'static str {
        match self {
            Self::SignIn => "platform_menu_sign_in",
            Self::Cancel => "platform_menu_cancel",
            Self::SignOut => "platform_menu_sign_out",
            Self::RetrySignOut => "platform_menu_retry_sign_out",
            Self::Reopen => "platform_menu_reopen",
            Self::ManageProfile => "platform_menu_manage_profile",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::SignIn => "Sign in",
            Self::Cancel => "Cancel sign-in",
            Self::SignOut => "Sign out",
            Self::RetrySignOut => "Retry sign-out",
            Self::Reopen => "Open browser page again",
            Self::ManageProfile => "Manage Profile",
        }
    }

    const fn icon(self) -> HugeIcon {
        match self {
            Self::SignIn => HugeIcon::ArrowRight,
            Self::Cancel => HugeIcon::Close,
            Self::SignOut | Self::RetrySignOut => HugeIcon::SignOut,
            Self::Reopen => HugeIcon::ArrowRightDouble,
            Self::ManageProfile => HugeIcon::User,
        }
    }

    const fn destructive(self) -> bool {
        matches!(self, Self::SignOut | Self::RetrySignOut)
    }

    /// Profile management opens the website and never races an account
    /// request; every authentication verb waits for the pending one.
    const fn waits_for_pending_request(self) -> bool {
        !matches!(self, Self::ManageProfile)
    }
}

fn account_actions(account: &aeris_desktop::account::AccountMenuState) -> Vec<AccountAction> {
    if !aeris_desktop::account::AUTH_BACKEND_CONFIGURED {
        Vec::new()
    } else if account.authorizing() {
        // While the browser holds the transaction there are two recovery exits.
        vec![AccountAction::Reopen, AccountAction::Cancel]
    } else if account.signed_in() {
        vec![AccountAction::ManageProfile, AccountAction::SignOut]
    } else if account.retry_sign_out {
        vec![AccountAction::RetrySignOut, AccountAction::SignIn]
    } else {
        vec![AccountAction::SignIn]
    }
}

fn account_action_rows(
    terminal: &Entity<TerminalApp>,
    account: &aeris_desktop::account::AccountMenuState,
    actions: &[AccountAction],
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let pending = account.presentation.pending;
    actions
        .iter()
        .map(|&action| {
            let enabled = !(pending && action.waits_for_pending_request());
            let icon_color = MenuRow::leading_icon_color(theme, action.destructive(), enabled);
            let terminal = terminal.clone();
            MenuRow::compact(action.id(), action.label(), theme)
                .leading(
                    header_icon(action.icon())
                        .with_size(px(16.0))
                        .color(gpui_color(icon_color)),
                )
                .disabled(!enabled)
                .destructive(action.destructive())
                .on_click(move |_, window, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        run_account_action(terminal, action, window, terminal_cx);
                    });
                })
                .into_any_element()
        })
        .collect()
}

fn run_account_action(
    terminal: &mut TerminalApp,
    action: AccountAction,
    window: &Window,
    cx: &mut Context<TerminalApp>,
) {
    match action {
        AccountAction::SignIn => TerminalApp::request_sign_in(cx),
        AccountAction::Cancel => TerminalApp::cancel_sign_in(cx),
        AccountAction::SignOut | AccountAction::RetrySignOut => TerminalApp::sign_out(cx),
        AccountAction::Reopen => TerminalApp::reopen_browser_page(cx),
        AccountAction::ManageProfile => {
            if let Err(error) = aeris_desktop::account::open_manage_profile() {
                diagnostic!("Aeris profile browser open degraded: {error}");
            } else {
                terminal.arm_profile_refresh_after_browser();
            }
        }
    }
    terminal.close_platform_menu(window, cx);
}

fn section_title(title: &'static str, theme: &AerisTheme) -> Div {
    div()
        .h(px(SECTION_TITLE_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(title)
}

const fn theme_mode_name(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => "Light",
        ThemeMode::Dark => "Dark",
    }
}

const fn theme_card_id(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => "platform_menu_theme_light",
        ThemeMode::Dark => "platform_menu_theme_dark",
    }
}

/// Theme section: one card per mode, each a miniature layout painted with
/// that mode's own tokens. The menu stays open after a pick so the whole
/// panel re-renders in the new mode.
fn theme_section(theme: &AerisTheme, on_select: &ThemeSelect) -> Div {
    div()
        .h(px(THEME_SECTION_HEIGHT))
        .flex_none()
        .flex()
        .flex_col()
        .px_2()
        .pb(px(SECTION_BOTTOM_PADDING))
        .child(section_title("Theme", theme))
        .child(
            div()
                .flex()
                .gap_2()
                .children(THEME_MODES.map(|mode| theme_card(mode, theme, on_select.clone()))),
        )
}

fn theme_card(mode: ThemeMode, theme: &AerisTheme, on_select: ThemeSelect) -> AnyElement {
    let colors = theme.colors;
    let selected = theme.mode == mode;
    let border_width = platform_border_width(theme);
    let radio = div()
        .size(px(12.0))
        .flex_none()
        .rounded_full()
        .border(border_width)
        .border_color(gpui_color(if selected {
            colors.ring_primary
        } else {
            colors.border_strong
        }))
        .flex()
        .items_center()
        .justify_center()
        .children(selected.then(|| {
            div()
                .size(px(6.0))
                .rounded_full()
                .bg(gpui_color(colors.ring_primary))
        }));
    BaseButton::new(theme_card_id(mode))
        .accessibility_label(format!("{} theme", theme_mode_name(mode)))
        .group(theme_card_id(mode))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(THEME_CARD_GAP))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            on_select(mode, window, cx);
            cx.stop_propagation();
        })
        .child(
            theme_preview(&AerisTheme::for_mode(mode))
                .border_color(gpui_color(if selected {
                    colors.ring_primary
                } else {
                    colors.border_secondary
                }))
                .when(!selected, |preview| {
                    preview.group_hover(theme_card_id(mode), move |style| {
                        style.border_color(gpui_color(colors.border_strong))
                    })
                }),
        )
        .child(
            div()
                .h(px(THEME_CARD_LABEL_HEIGHT))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_xs()
                .text_color(gpui_color(if selected {
                    colors.text_primary
                } else {
                    colors.text_secondary
                }))
                .child(radio)
                .child(theme_mode_name(mode)),
        )
        .into_any_element()
}

/// Miniature layout built only from plain panels: a title bar, a side panel,
/// the main panel with a bottom panel under it, and a right panel. The
/// rounded frame paints the preview background itself because GPUI's
/// `overflow_hidden` clips to a rectangle: a square inner fill would paint
/// over the rounded border corners. The title bar uses the inner radius so
/// it stays inside the curve.
fn theme_preview(preview: &AerisTheme) -> Div {
    let colors = preview.colors;
    let border_width = platform_border_width(preview);
    let radius = f32::from(RadiusToken::Default.logical_pixels());
    let inner_radius = px((radius - preview.dimensions.border_width).max(0.0));
    let panel = || {
        div()
            .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
            .border(border_width)
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface_secondary))
    };
    div()
        .h(px(THEME_PREVIEW_HEIGHT))
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(radius))
        .border(border_width)
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .h(px(10.0))
                .flex_none()
                .rounded_tl(inner_radius)
                .rounded_tr(inner_radius)
                .bg(gpui_color(colors.surface_secondary))
                .border_b(border_width)
                .border_color(gpui_color(colors.border_secondary)),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(3.0))
                .p(px(4.0))
                .child(panel().w(px(16.0)).flex_none())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(panel().flex_1())
                        .child(panel().h(px(14.0)).flex_none()),
                )
                .child(panel().w(px(22.0)).flex_none()),
        )
}

/// Window section: the frameless mode switch with a one-line description of what it does.
fn window_section(frameless: bool, on_toggle: &FramelessToggle, theme: &AerisTheme) -> Div {
    let on_toggle = on_toggle.clone();
    div()
        .h(px(WINDOW_SECTION_HEIGHT))
        .flex_none()
        .flex()
        .flex_col()
        .px_2()
        .pb(px(SECTION_BOTTOM_PADDING))
        .child(section_title("Window", theme))
        .child(
            div().h(px(WINDOW_SETTING_HEIGHT)).flex_none().child(
                SwitchRow::new(
                    "platform_menu_frameless_window",
                    "Frameless mode",
                    frameless,
                    theme,
                )
                .description("Show the tab bar when pointing at the top edge")
                .on_change(move |enabled, _, window, cx| {
                    on_toggle(enabled, window, cx);
                }),
            ),
        )
}

/// Label and value rows under About. The build mode appears only while no
/// sign-in backend exists, in place of a separate account panel.
fn about_details(
    account: &aeris_desktop::account::AccountMenuState,
    update: Option<&UpdatePresentation>,
) -> Vec<(&'static str, String)> {
    let system = update.map_or_else(
        || std::env::consts::OS.to_string(),
        |value| value.system_version.clone(),
    );
    let mut details = vec![("System", system)];
    if !aeris_desktop::account::AUTH_BACKEND_CONFIGURED {
        details.push(("Mode", account.presentation.state.to_string()));
    }
    details
}

fn about_section(
    details: Vec<(&'static str, String)>,
    update: &UpdateView,
    on_update: &UpdateActivate,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let height = about_section_height(details.len());
    div()
        .h(px(height))
        .flex_none()
        .flex()
        .flex_col()
        .px_2()
        .pb(px(SECTION_BOTTOM_PADDING))
        .child(section_title("About", theme))
        .child(
            div()
                .h(px(ABOUT_BRAND_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(super::terminal_chrome::brand_logo_sized(px(28.0)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(platform_font_weight(TypographyRole::Strong))
                                .text_color(gpui_color(colors.text_primary))
                                .child("Aeris Terminal"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                        ),
                ),
        )
        .children(details.into_iter().map(|(label, value)| {
            div()
                .h(px(ABOUT_DETAIL_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .text_xs()
                .child(div().text_color(gpui_color(colors.text_muted)).child(label))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(gpui_color(colors.text_primary))
                        .child(value),
                )
        }))
        .child(provider_notices(current_utc_year(), theme))
        .child(update_row(update, on_update, theme))
}

/// Copyright and trademark notices Rithmic requires wherever the terminal shows
/// its own; the year follows the clock as their wording asks.
fn provider_notice_texts(year: i64) -> [String; 3] {
    [
        format!(
            "The R | Protocol API™ software is Copyright © {year} by Rithmic, LLC. \
             All rights reserved."
        ),
        "Trading Platform by Rithmic™ is a trademark of Rithmic, LLC. All rights reserved."
            .to_string(),
        format!(
            "The OMNE™ software is Copyright © {year} by Omnesys, LLC and Omnesys \
             Technologies, Inc. All rights reserved."
        ),
    ]
}

const POWERED_BY_OMNE_NOTICE: &str =
    "is a trademark of Omnesys, LLC and Omnesys Technologies, Inc. All rights reserved.";

fn provider_notices(year: i64, theme: &AerisTheme) -> impl IntoElement {
    let muted = gpui_color(theme.colors.text_muted);
    div()
        .id("about_provider_notices")
        .h(px(ABOUT_NOTICES_HEIGHT))
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .py_1()
        .overflow_y_scroll()
        .text_xs()
        .text_color(muted)
        .children(
            provider_notice_texts(year)
                .into_iter()
                .map(|notice| div().flex_none().child(notice)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_start()
                .child(super::terminal_chrome::AttributionMarkImage {
                    mark: assets::AttributionMark::PoweredByOmne,
                    mode: theme.mode,
                })
                .child(POWERED_BY_OMNE_NOTICE),
        )
}

fn current_utc_year() -> i64 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_secs()).ok())
        .unwrap_or(0);
    utc_year(seconds)
}

/// Gregorian year of a Unix timestamp (Hinnant's days-to-civil algorithm).
fn utc_year(unix_seconds: i64) -> i64 {
    let days = unix_seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400;
    if month_index >= 10 { year + 1 } else { year }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UpdateAction {
    Restart,
    Retry,
}

struct UpdateView {
    status: String,
    status_color: ThemeColor,
    action: Option<UpdateAction>,
}

fn about_update_view(update: Option<&UpdatePresentation>, theme: &AerisTheme) -> UpdateView {
    let colors = theme.colors;
    let (status, status_color, action) = match update.map(|value| &value.state) {
        Some(UpdateState::Idle) => (
            "Updates are checked automatically.".to_string(),
            colors.text_muted,
            None,
        ),
        Some(UpdateState::Checking) => {
            ("Checking for updates…".to_string(), colors.text_muted, None)
        }
        Some(UpdateState::Current) => (
            "Aeris Terminal is up to date.".to_string(),
            colors.primary,
            None,
        ),
        Some(UpdateState::Downloading { latest_version }) => (
            format!("Downloading Aeris Terminal {latest_version}…"),
            colors.text_muted,
            None,
        ),
        Some(UpdateState::ReadyToRestart { latest_version }) => (
            format!("Aeris Terminal {latest_version} is ready. Restart to update."),
            colors.primary,
            Some(UpdateAction::Restart),
        ),
        Some(UpdateState::Error(error)) => {
            (error.clone(), colors.danger, Some(UpdateAction::Retry))
        }
        Some(UpdateState::PreparingRestart) => (
            "Preparing restart to install update…".to_string(),
            colors.text_muted,
            None,
        ),
        None => (
            "Update checking is unavailable in this session.".to_string(),
            colors.text_muted,
            None,
        ),
    };
    UpdateView {
        status,
        status_color,
        action,
    }
}

fn update_row(view: &UpdateView, on_update: &UpdateActivate, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let action_button = view.action.map(|action| {
        let on_update = on_update.clone();
        Button::new("platform_menu_update_action", theme)
            .variant(ButtonVariant::Default)
            .button_size(ButtonSize::Sm)
            .label(match action {
                UpdateAction::Restart => "Restart",
                UpdateAction::Retry => "Retry",
            })
            .on_click(move |_, window, cx| on_update(action, window, cx))
    });
    div()
        .h(px(ABOUT_UPDATE_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child("Updates"),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(gpui_color(view.status_color))
                        .child(view.status.clone()),
                ),
        )
        .children(action_button)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    struct SectionsHarness(AerisTheme);

    impl Render for SectionsHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let theme = self.0;
            let ignore_theme: ThemeSelect = Rc::new(|_, _, _| {});
            let ignore_update: UpdateActivate = Rc::new(|_, _, _| {});
            let ignore_frameless: FramelessToggle = Rc::new(|_, _, _| {});
            let account = aeris_desktop::account::unavailable_menu_state();
            div()
                .w(px(PLATFORM_MENU_WIDTH))
                .flex()
                .flex_col()
                .child(
                    theme_section(&theme, &ignore_theme).debug_selector(|| "theme_section".into()),
                )
                .child(
                    window_section(true, &ignore_frameless, &theme)
                        .debug_selector(|| "window_section".into()),
                )
                .child(
                    about_section(
                        about_details(&account, None),
                        &about_update_view(None, &theme),
                        &ignore_update,
                        &theme,
                    )
                    .debug_selector(|| "about_section".into()),
                )
        }
    }

    #[gpui::test]
    fn sections_render_at_the_heights_the_panel_clamps_with(cx: &mut TestAppContext) {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let (_, cx) = cx.add_window_view(move |_, cx| {
                gpui_base::init(cx);
                SectionsHarness(theme)
            });
            cx.run_until_parked();
            let theme_bounds = cx.debug_bounds("theme_section").expect("theme section");
            let window_bounds = cx.debug_bounds("window_section").expect("window section");
            let about_bounds = cx.debug_bounds("about_section").expect("about section");
            assert_eq!(theme_bounds.size.height, px(THEME_SECTION_HEIGHT));
            assert_eq!(window_bounds.size.height, px(WINDOW_SECTION_HEIGHT));
            assert_eq!(about_bounds.size.height, px(about_section_height(2)));
        }
    }

    #[test]
    fn development_menu_reports_mode_under_about_without_account_rows() {
        let account = aeris_desktop::account::unavailable_menu_state();
        assert_eq!(account_actions(&account), [] as [AccountAction; 0]);
        assert!(identity_header(&account, &AerisTheme::dark()).is_none());
        let details = about_details(&account, None);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].0, "System");
        assert_eq!(details[1], ("Mode", "Development mode".to_string()));
    }

    #[test]
    fn layout_height_counts_every_section_and_separator() {
        // The panel's inset plus border on both edges.
        let panel_chrome = 9.0;
        let development = PlatformMenuLayout {
            identity: false,
            actions: 0,
            error: false,
            about_details: 2,
        };
        let expected = panel_chrome
            + THEME_SECTION_HEIGHT
            + WINDOW_SECTION_HEIGHT
            + 2.0 * CHART_CONTEXT_MENU_SEPARATOR_HEIGHT
            + about_section_height(2);
        assert!((development.height(panel_chrome) - expected).abs() < f32::EPSILON);
        let signed_in = PlatformMenuLayout {
            identity: true,
            actions: 2,
            error: true,
            about_details: 1,
        };
        let expected = panel_chrome
            + IDENTITY_HEIGHT
            + 2.0 * CHART_CONTEXT_MENU_ROW_HEIGHT
            + ACCOUNT_ERROR_HEIGHT
            + THEME_SECTION_HEIGHT
            + WINDOW_SECTION_HEIGHT
            + about_section_height(1)
            + 4.0 * CHART_CONTEXT_MENU_SEPARATOR_HEIGHT;
        assert!((signed_in.height(panel_chrome) - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn utc_year_handles_year_boundaries_and_leap_days() {
        assert_eq!(utc_year(0), 1970);
        assert_eq!(utc_year(951_782_400), 2000); // 2000-02-29
        assert_eq!(utc_year(1_798_761_599), 2026); // 2026-12-31T23:59:59Z
        assert_eq!(utc_year(1_798_761_600), 2027); // 2027-01-01T00:00:00Z
        assert_eq!(utc_year(-1), 1969);
    }

    #[test]
    fn provider_notices_carry_the_requested_year_and_owners() {
        let notices = provider_notice_texts(2026);
        assert!(notices[0].contains("R | Protocol API™") && notices[0].contains("© 2026"));
        assert!(notices[1].starts_with("Trading Platform by Rithmic™ is a trademark"));
        assert!(notices[2].contains("OMNE™") && notices[2].contains("© 2026"));
        assert!(POWERED_BY_OMNE_NOTICE.contains("Omnesys Technologies, Inc."));
    }

    #[test]
    fn only_authentication_actions_wait_for_a_pending_request() {
        assert!(!AccountAction::ManageProfile.waits_for_pending_request());
        for action in [
            AccountAction::SignIn,
            AccountAction::Cancel,
            AccountAction::SignOut,
            AccountAction::RetrySignOut,
            AccountAction::Reopen,
        ] {
            assert!(action.waits_for_pending_request());
        }
        assert!(AccountAction::SignOut.destructive());
        assert!(AccountAction::RetrySignOut.destructive());
        assert!(!AccountAction::SignIn.destructive());
    }
}
