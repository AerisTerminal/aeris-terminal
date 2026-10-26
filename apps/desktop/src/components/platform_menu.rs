//! The platform menu: the one dropdown under the header avatar. It stacks every
//! application-level section in a single panel: account (profile, plan and sign-in actions), Theme
//! with a preview card per mode and a chip per palette preset, Window with frameless mode and the drawing toolbar switch, the
//! keyboard shortcuts list, and
//! About with version, system, validation mode, provider notices while its session is held, and
//! updates.

use aeris_observability::diagnostic;
use std::rc::Rc;

use super::*;
use aeris_design_system::{ThemeMode, ThemePreset};
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
const PRESET_GRID_TOP: f32 = 10.0;
const PRESET_CHIP_HEIGHT: f32 = 28.0;
const PRESET_CHIP_GAP: f32 = 4.0;
const PRESET_SWATCH_SIZE: f32 = 14.0;
/// Space between the swatch ring and its primary-color dot.
const PRESET_SWATCH_INSET: f32 = 2.0;
/// Presets lay out two per row.
const PRESET_ROWS: usize = ThemePreset::ALL.len().div_ceil(2);
const _: () = assert!(
    PRESET_ROWS == 4,
    "PRESET_GRID_HEIGHT assumes four preset rows"
);
const PRESET_GRID_HEIGHT: f32 = 4.0 * PRESET_CHIP_HEIGHT + 3.0 * PRESET_CHIP_GAP;
const THEME_SECTION_HEIGHT: f32 = SECTION_TITLE_HEIGHT
    + THEME_PREVIEW_HEIGHT
    + THEME_CARD_GAP
    + THEME_CARD_LABEL_HEIGHT
    + PRESET_GRID_TOP
    + PRESET_GRID_HEIGHT
    + SECTION_BOTTOM_PADDING;
const WINDOW_SETTING_HEIGHT: f32 = 40.0;
const WINDOW_SECTION_HEIGHT: f32 =
    SECTION_TITLE_HEIGHT + 2.0 * WINDOW_SETTING_HEIGHT + SECTION_BOTTOM_PADDING;
const ABOUT_BRAND_HEIGHT: f32 = 40.0;
const ABOUT_DETAIL_HEIGHT: f32 = 26.0;
const ABOUT_UPDATE_HEIGHT: f32 = 44.0;
/// Room for the four provider notices at the menu width; the block scrolls
/// rather than clipping if a font fallback wraps them onto more lines.
const ABOUT_NOTICES_HEIGHT: f32 = 200.0;
const THEME_MODES: [ThemeMode; 2] = [ThemeMode::Light, ThemeMode::Dark];

type ThemeSelect = Rc<dyn Fn(AerisTheme, &mut Window, &mut App)>;
type UpdateActivate = Rc<dyn Fn(UpdateAction, &mut Window, &mut App)>;
type WindowToggle = Rc<dyn Fn(bool, &mut Window, &mut App)>;

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
                .text_color(gpui_color(colors.text_default))
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

/// The application state the platform menu shows below the account.
#[derive(Clone, Copy)]
pub(super) struct PlatformMenuContent<'a> {
    pub(super) update: Option<&'a UpdatePresentation>,
    pub(super) window_frame: chart_chrome::WindowFrame,
    pub(super) drawing_toolbar_visible: bool,
    /// Notices appear under About only while their provider's session is held.
    pub(super) attribution: Option<aeris_contracts::ProviderAttribution>,
    pub(super) now_unix_seconds: i64,
}

/// The platform menu anchored under the avatar's click point. Every section
/// has a fixed height, so the panel clamps into the viewport without
/// measuring a frame first.
pub(super) fn platform_menu_layer(
    terminal: &Entity<TerminalApp>,
    account: &aeris_desktop::account::AccountMenuState,
    anchor: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    content: PlatformMenuContent<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let PlatformMenuContent {
        update,
        window_frame,
        drawing_toolbar_visible,
        attribution,
        now_unix_seconds,
    } = content;
    let header = identity_header(account, theme);
    let actions = account_actions(account);
    let details = about_details(account, update);
    let layout = PlatformMenuLayout {
        identity: header.is_some(),
        actions: actions.len(),
        error: account.error.is_some(),
        about_details: details.len(),
        attribution,
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
    let select_theme: ThemeSelect = Rc::new(move |selected, window, cx| {
        theme_terminal.update(cx, |terminal, terminal_cx| {
            terminal.set_theme(&selected, window, terminal_cx);
        });
    });
    let (toggle_frameless, toggle_drawing_toolbar) = window_toggles(terminal);
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
            WindowSettings {
                frameless: window_frame.frameless(),
                drawing_toolbar_visible,
            },
            &toggle_frameless,
            &toggle_drawing_toolbar,
            theme,
        ))
        .separator()
        .child(keyboard_shortcuts_row(terminal, theme))
        .separator()
        .child(about_section(
            details,
            attribution,
            now_unix_seconds,
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
    attribution: Option<aeris_contracts::ProviderAttribution>,
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
            + CHART_CONTEXT_MENU_ROW_HEIGHT
            + separator
            + about_section_height(self.about_details, self.attribution)
    }
}

fn about_section_height(
    details: usize,
    attribution: Option<aeris_contracts::ProviderAttribution>,
) -> f32 {
    let notices = if attribution.is_some() {
        ABOUT_NOTICES_HEIGHT
    } else {
        0.0
    };
    SECTION_TITLE_HEIGHT
        + ABOUT_BRAND_HEIGHT
        + details.to_f32().unwrap_or_default() * ABOUT_DETAIL_HEIGHT
        + notices
        + ABOUT_UPDATE_HEIGHT
        + SECTION_BOTTOM_PADDING
}

/// Identity header. Signed-in sessions show the verified avatar, name, email
/// and plan; other visible states name themselves with their detail. Signed-out
/// and validation sessions show none; validation mode is reported under About.
fn identity_header(
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    if !account.sign_in_enabled || account.hides_identity() {
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
        let detail = if account.offline() {
            presentation.state.to_string()
        } else {
            presentation.email.clone()
        };
        return Some(
            header
                .gap_3()
                .child(
                    div()
                        .size(px(32.0))
                        .flex_none()
                        .rounded_full()
                        .overflow_hidden()
                        .child(account_avatar_face(account, theme)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(gpui_color(colors.text_default))
                                .child(name),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(detail),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .py(px(2.0))
                        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
                        .border(platform_border_width(theme))
                        .border_color(gpui_color(colors.border_secondary))
                        .text_xs()
                        .text_color(gpui_color(colors.text_secondary))
                        .child(presentation.plan),
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
                    .text_color(gpui_color(colors.text_default))
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
    if !account.sign_in_enabled {
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
            let terminal = terminal.clone();
            MenuRow::compact(action.id(), action.label(), theme)
                .leading_icon(header_icon(action.icon()).with_size(px(16.0)))
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
        AccountAction::SignIn => terminal.request_sign_in(cx),
        AccountAction::Cancel => terminal.cancel_sign_in(cx),
        AccountAction::SignOut | AccountAction::RetrySignOut => terminal.sign_out(cx),
        AccountAction::Reopen => terminal.reopen_browser_page(cx),
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

/// One section of the menu, drawn straight on the panel's `surface`.
fn section() -> Div {
    div().flex_none().flex().flex_col().px_2()
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

/// Theme section: one preview tile per mode in the selected preset's own tokens, then one
/// chip per palette preset. The menu stays open after a pick so the whole panel re-renders in
/// the new theme.
fn theme_section(theme: &AerisTheme, on_select: &ThemeSelect) -> Div {
    section()
        .h(px(THEME_SECTION_HEIGHT))
        .pb(px(SECTION_BOTTOM_PADDING))
        .child(section_title("Theme", theme))
        .child(
            div()
                .flex()
                .gap_2()
                .children(THEME_MODES.map(|mode| theme_card(mode, theme, on_select.clone()))),
        )
        .child(
            div()
                .mt(px(PRESET_GRID_TOP))
                .h(px(PRESET_GRID_HEIGHT))
                .flex()
                .flex_col()
                .gap(px(PRESET_CHIP_GAP))
                .children(ThemePreset::ALL.chunks(2).map(|row| {
                    div().flex().gap(px(PRESET_CHIP_GAP)).children(
                        row.iter()
                            .map(|preset| preset_chip(*preset, theme, on_select.clone())),
                    )
                })),
        )
}

const fn preset_chip_id(preset: ThemePreset) -> &'static str {
    match preset {
        ThemePreset::Aeris => "platform_menu_preset_aeris",
        ThemePreset::TokyoNight => "platform_menu_preset_tokyo_night",
        ThemePreset::Catppuccin => "platform_menu_preset_catppuccin",
        ThemePreset::Github => "platform_menu_preset_github",
        ThemePreset::One => "platform_menu_preset_one",
        ThemePreset::Dracula => "platform_menu_preset_dracula",
        ThemePreset::Solarized => "platform_menu_preset_solarized",
        ThemePreset::Gruvbox => "platform_menu_preset_gruvbox",
    }
}

/// One palette preset in the current mode: a swatch of that preset's surface and primary
/// color beside its name, filled with `active-bg` while selected.
fn preset_chip(preset: ThemePreset, theme: &AerisTheme, on_select: ThemeSelect) -> AnyElement {
    let colors = theme.colors;
    let selected = theme.preset == preset;
    let preview = AerisTheme::resolve(preset, theme.mode);
    let id = preset_chip_id(preset);
    BaseButton::new(id)
        .accessibility_label(format!("{} palette", preset.label()))
        .group(id)
        .flex_1()
        .min_w_0()
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            on_select(preview, window, cx);
            cx.stop_propagation();
        })
        .child(
            div()
                .h(px(PRESET_CHIP_HEIGHT))
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .when(selected, |chip| chip.bg(gpui_color(colors.active_bg)))
                .when(!selected, |chip| {
                    chip.group_hover(id, |style| style.bg(gpui_color(colors.hover_bg)))
                })
                .child(
                    div()
                        .size(px(PRESET_SWATCH_SIZE))
                        .flex_none()
                        .rounded_full()
                        .border(platform_border_width(theme))
                        .border_color(gpui_color(colors.border_secondary))
                        .bg(gpui_color(preview.colors.surface))
                        // An equal inset inside the ring keeps the dot concentric; flex
                        // centering a fixed-size dot lands on half pixels and drifts.
                        .p(px(PRESET_SWATCH_INSET))
                        .child(
                            div()
                                .size_full()
                                .rounded_full()
                                .bg(gpui_color(preview.colors.primary)),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(gpui_color(if selected {
                            colors.text_default
                        } else {
                            colors.text_secondary
                        }))
                        .child(preset.label()),
                ),
        )
        .into_any_element()
}

fn theme_card(mode: ThemeMode, theme: &AerisTheme, on_select: ThemeSelect) -> AnyElement {
    let colors = theme.colors;
    let selected = theme.mode == mode;
    let preview = AerisTheme::resolve(theme.preset, mode);
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
            on_select(preview, window, cx);
            cx.stop_propagation();
        })
        .child(theme_preview(
            &preview,
            theme,
            selected,
            theme_card_id(mode),
        ))
        .child(
            div()
                .h(px(THEME_CARD_LABEL_HEIGHT))
                .flex()
                .items_center()
                .text_xs()
                .text_color(gpui_color(if selected {
                    colors.text_default
                } else {
                    colors.text_secondary
                }))
                .child(theme_mode_name(mode)),
        )
        .into_any_element()
}

/// Inset of the preview's sample window from the tile's top and left edges; it runs off the
/// right and bottom edges.
const THEME_PREVIEW_WINDOW_TOP: f32 = 22.0;
const THEME_PREVIEW_WINDOW_LEFT: f32 = 16.0;
const THEME_PREVIEW_BADGE_SIZE: f32 = 18.0;

/// A tile in the previewed mode's own tokens: a `surface-secondary` backdrop with one sample
/// window on `surface` showing "Aa" in that mode's text color, and a `primary` check badge on
/// the selected mode. Colors resolve over the mode's `surface` because its dark tints are
/// translucent and the tile must look the same in either menu. GPUI clips to rectangles, so
/// the window takes the tile's radius at the corner it shares with it.
fn theme_preview(
    preview: &AerisTheme,
    theme: &AerisTheme,
    selected: bool,
    group: &'static str,
) -> Div {
    let colors = preview.colors;
    let radius = px(f32::from(RadiusToken::Default.logical_pixels()));
    let tile = colors.surface_secondary.over(colors.surface);
    let hovered = colors.hover_bg.over(tile);
    let badge = selected.then(|| {
        div()
            .absolute()
            .right(px(8.0))
            .bottom(px(8.0))
            .size(px(THEME_PREVIEW_BADGE_SIZE))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_color(theme.colors.primary))
            .child(
                header_icon(HugeIcon::Tick)
                    .with_size(px(12.0))
                    .color(gpui_color(theme.colors.primary_foreground)),
            )
    });
    div()
        .relative()
        .h(px(THEME_PREVIEW_HEIGHT))
        .w_full()
        .rounded(radius)
        .bg(gpui_color(tile))
        .when(!selected, |preview| {
            preview.group_hover(group, move |style| style.bg(gpui_color(hovered)))
        })
        .child(
            div()
                .absolute()
                .top(px(THEME_PREVIEW_WINDOW_TOP))
                .left(px(THEME_PREVIEW_WINDOW_LEFT))
                .right_0()
                .bottom_0()
                .rounded_tl(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .rounded_br(radius)
                .bg(gpui_color(colors.surface))
                .pl(px(8.0))
                .pt(px(4.0))
                .text_lg()
                .text_color(gpui_color(colors.text_default))
                .child("Aa")
                .children(badge),
        )
}

/// The frameless mode and drawing toolbar switch callbacks.
fn window_toggles(terminal: &Entity<TerminalApp>) -> (WindowToggle, WindowToggle) {
    let frameless_terminal = terminal.clone();
    let toggle_frameless: WindowToggle = Rc::new(move |frameless, _, cx| {
        let window_frame = if frameless {
            chart_chrome::WindowFrame::Frameless
        } else {
            chart_chrome::WindowFrame::Framed
        };
        frameless_terminal.update(cx, |terminal, terminal_cx| {
            terminal.set_window_frame(window_frame, terminal_cx);
        });
    });
    let drawing_toolbar_terminal = terminal.clone();
    let toggle_drawing_toolbar: WindowToggle = Rc::new(move |visible, _, cx| {
        drawing_toolbar_terminal.update(cx, |terminal, terminal_cx| {
            terminal.set_drawing_toolbar_visible(visible, terminal_cx);
        });
    });
    (toggle_frameless, toggle_drawing_toolbar)
}

#[derive(Clone, Copy)]
struct WindowSettings {
    frameless: bool,
    drawing_toolbar_visible: bool,
}

/// Window section: the frameless mode and drawing toolbar switches, each with a one-line
/// description of what it does.
fn window_section(
    settings: WindowSettings,
    on_frameless: &WindowToggle,
    on_drawing_toolbar: &WindowToggle,
    theme: &AerisTheme,
) -> Div {
    section()
        .h(px(WINDOW_SECTION_HEIGHT))
        .pb(px(SECTION_BOTTOM_PADDING))
        .child(section_title("Window", theme))
        .child(window_switch(
            SwitchRow::new(
                "platform_menu_frameless_window",
                "Frameless mode",
                settings.frameless,
                theme,
            )
            .description("Show the tab bar when pointing at the top edge"),
            on_frameless,
        ))
        .child(window_switch(
            SwitchRow::new(
                "platform_menu_drawing_toolbar",
                "Drawing toolbar",
                settings.drawing_toolbar_visible,
                theme,
            )
            .description("Show the drawing tools beside the charts"),
            on_drawing_toolbar,
        ))
}

fn window_switch(row: SwitchRow, on_toggle: &WindowToggle) -> Div {
    let on_toggle = on_toggle.clone();
    div()
        .h(px(WINDOW_SETTING_HEIGHT))
        .flex_none()
        .child(row.on_change(move |enabled, _, window, cx| {
            on_toggle(enabled, window, cx);
        }))
}

fn keyboard_shortcuts_row(terminal: &Entity<TerminalApp>, theme: &AerisTheme) -> AnyElement {
    let open = terminal.clone();
    MenuRow::compact(
        "platform_menu_keyboard_shortcuts",
        "Keyboard shortcuts",
        theme,
    )
    .leading_icon(header_icon(HugeIcon::Keyboard).with_size(px(16.0)))
    .on_click(move |_, window, cx| {
        open.update(cx, |terminal, terminal_cx| {
            terminal.open_keyboard_shortcuts(window, terminal_cx);
        });
    })
    .into_any_element()
}

/// Label and value rows under About. The mode appears only for a validation
/// run, which has no account session.
fn about_details(
    account: &aeris_desktop::account::AccountMenuState,
    update: Option<&UpdatePresentation>,
) -> Vec<(&'static str, String)> {
    let system = update.map_or_else(
        || std::env::consts::OS.to_string(),
        |value| value.system_version.clone(),
    );
    let mut details = vec![("System", system)];
    if !account.sign_in_enabled {
        details.push(("Mode", account.presentation.state.to_string()));
    }
    details
}

fn about_section(
    details: Vec<(&'static str, String)>,
    attribution: Option<aeris_contracts::ProviderAttribution>,
    now_unix_seconds: i64,
    update: &UpdateView,
    on_update: &UpdateActivate,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let height = about_section_height(details.len(), attribution);
    section()
        .h(px(height))
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
                                .text_color(gpui_color(colors.text_default))
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
                        .text_color(gpui_color(colors.text_default))
                        .child(value),
                )
        }))
        .when_some(attribution, |about, attribution| {
            about.child(provider_notices(
                attribution,
                utc_year(now_unix_seconds),
                theme,
            ))
        })
        .child(update_row(update, on_update, theme))
}

fn provider_notice_texts(
    attribution: aeris_contracts::ProviderAttribution,
    year: i64,
) -> Vec<String> {
    attribution
        .notices
        .iter()
        .map(|notice| notice.replace("{year}", &year.to_string()))
        .collect()
}

fn provider_notices(
    attribution: aeris_contracts::ProviderAttribution,
    year: i64,
    theme: &AerisTheme,
) -> impl IntoElement {
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
            provider_notice_texts(attribution, year)
                .into_iter()
                .map(|notice| div().flex_none().child(notice)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_start()
                .children(
                    assets::AttributionMark::for_key(attribution.powered_by_mark_key).map(|mark| {
                        super::terminal_chrome::AttributionMarkImage {
                            mark,
                            mode: theme.mode,
                        }
                    }),
                )
                .child(attribution.powered_by_notice),
        )
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
            let ignore_toggle: WindowToggle = Rc::new(|_, _, _| {});
            let account = aeris_desktop::account::unavailable_menu_state();
            div()
                .w(px(PLATFORM_MENU_WIDTH))
                .flex()
                .flex_col()
                .child(
                    theme_section(&theme, &ignore_theme).debug_selector(|| "theme_section".into()),
                )
                .child(
                    window_section(
                        WindowSettings {
                            frameless: true,
                            drawing_toolbar_visible: true,
                        },
                        &ignore_toggle,
                        &ignore_toggle,
                        &theme,
                    )
                    .debug_selector(|| "window_section".into()),
                )
                .child(
                    about_section(
                        about_details(&account, None),
                        None,
                        1_780_000_000,
                        &about_update_view(None, &theme),
                        &ignore_update,
                        &theme,
                    )
                    .debug_selector(|| "about_section".into()),
                )
                .child(
                    about_section(
                        about_details(&account, None),
                        aeris_market_runtime::built_in_provider_presentations()
                            .iter()
                            .find_map(|p| p.attribution),
                        1_780_000_000,
                        &about_update_view(None, &theme),
                        &ignore_update,
                        &theme,
                    )
                    .debug_selector(|| "about_section_attributed".into()),
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
            let attributed_bounds = cx
                .debug_bounds("about_section_attributed")
                .expect("about section with provider notices");
            let attributed = aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .find_map(|p| p.attribution);
            assert_eq!(theme_bounds.size.height, px(THEME_SECTION_HEIGHT));
            assert_eq!(window_bounds.size.height, px(WINDOW_SECTION_HEIGHT));
            assert_eq!(about_bounds.size.height, px(about_section_height(2, None)));
            assert_eq!(
                attributed_bounds.size.height,
                px(about_section_height(2, attributed))
            );
            assert!(
                (about_section_height(2, attributed)
                    - about_section_height(2, None)
                    - ABOUT_NOTICES_HEIGHT)
                    .abs()
                    < f32::EPSILON,
                "without a held provider session the notices take no room"
            );
        }
    }

    #[test]
    fn validation_menu_reports_mode_under_about_without_account_rows() {
        let account = aeris_desktop::account::unavailable_menu_state();
        assert_eq!(account_actions(&account), [] as [AccountAction; 0]);
        assert!(identity_header(&account, &AerisTheme::dark()).is_none());
        let details = about_details(&account, None);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].0, "System");
        assert_eq!(
            details[1],
            ("Mode", "Validation (sign-in disabled)".to_string())
        );
    }

    #[test]
    fn layout_height_counts_every_section_and_separator() {
        // The panel's inset plus border on both edges.
        let panel_chrome = 9.0;
        let attributed = aeris_market_runtime::built_in_provider_presentations()
            .iter()
            .find_map(|p| p.attribution);
        let development = PlatformMenuLayout {
            identity: false,
            actions: 0,
            error: false,
            about_details: 2,
            attribution: None,
        };
        let expected = panel_chrome
            + THEME_SECTION_HEIGHT
            + WINDOW_SECTION_HEIGHT
            + CHART_CONTEXT_MENU_ROW_HEIGHT
            + 3.0 * CHART_CONTEXT_MENU_SEPARATOR_HEIGHT
            + about_section_height(2, None);
        assert!((development.height(panel_chrome) - expected).abs() < f32::EPSILON);
        let signed_in = PlatformMenuLayout {
            identity: true,
            actions: 2,
            error: true,
            about_details: 1,
            attribution: attributed,
        };
        let expected = panel_chrome
            + IDENTITY_HEIGHT
            + 3.0 * CHART_CONTEXT_MENU_ROW_HEIGHT
            + ACCOUNT_ERROR_HEIGHT
            + THEME_SECTION_HEIGHT
            + WINDOW_SECTION_HEIGHT
            + about_section_height(1, attributed)
            + 5.0 * CHART_CONTEXT_MENU_SEPARATOR_HEIGHT;
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
        let attribution = aeris_market_runtime::built_in_provider_presentations()
            .iter()
            .find_map(|p| p.attribution)
            .unwrap();
        let notices = provider_notice_texts(attribution, 2026);
        assert!(notices[0].contains("R | Protocol API™") && notices[0].contains("© 2026"));
        assert!(notices[1].starts_with("Trading Platform by Rithmic™ is a trademark"));
        assert!(notices[2].contains("OMNE™") && notices[2].contains("© 2026"));
        assert!(
            attribution
                .powered_by_notice
                .contains("Omnesys Technologies, Inc.")
        );
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
