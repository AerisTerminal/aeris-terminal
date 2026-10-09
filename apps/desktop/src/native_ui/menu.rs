use std::{rc::Rc, sync::Arc, time::Duration};

use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    Animation, AnimationElement, AnimationExt, AnyElement, App, Bounds, ClickEvent, Div, ElementId,
    IntoElement, Length, Pixels, Point, Rems, RenderOnce, ScrollHandle, SharedString, Size,
    Stateful, Toggled, Window, div, ease_out_quint, point, prelude::*, px, rems,
};
use gpui_base::Button as BaseButton;

use crate::desktop::assets::UiIcon;

use super::{
    icon::Icon,
    platform_font_weight,
    rem_scale::ROOT_REM_PX,
    theme::{gpui_color, platform_border_width},
};

type Activation = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type Hover = Rc<dyn Fn(&bool, &mut Window, &mut App)>;
type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

// Row geometry is in rems (32px / 36px at the root rem) so rows also follow an
// enclosing `RemScale` panel, not only an explicit `MenuRow::scale`.
const COMPACT_ROW_HEIGHT: Rems = Rems(2.0);
const SEARCH_ROW_HEIGHT: Rems = Rems(2.25);
/// A divider's full height: its 1px line plus clear space above and below, so the line never
/// touches a hovered or highlighted row.
pub(crate) const MENU_SEPARATOR_HEIGHT: f32 = 5.0;
const SEPARATOR_HEIGHT: Pixels = px(MENU_SEPARATOR_HEIGHT);
/// Vertical inset of a row's hover and highlight fill inside its row height, so two filled rows
/// next to each other (hover beside the current value) read as separate pills.
const ROW_FILL_INSET: Rems = Rems(0.125);
/// Group name tying a row's inset fill to hovering the whole row.
const MENU_ROW_GROUP: &str = "menu_row";
/// Gap between a menu panel's border and its rows: 4px, which clears the `r(1 - 1/√2)` ≈ 2.2px a
/// row's square corner needs to stay inside the panel's 8px rounded corner.
pub(crate) const MENU_PANEL_INSET: f32 = 4.0;
const PANEL_INSET: Rems = Rems(MENU_PANEL_INSET / ROOT_REM_PX);
const POPUP_ENTER_DURATION: Duration = Duration::from_millis(130);
const POPUP_ENTER_TRAVEL: f32 = 2.0;
/// Logical viewport at which scaled menus render at their 1x design size.
const MENU_SCALE_REFERENCE_WIDTH: f32 = 1440.0;
const MENU_SCALE_REFERENCE_HEIGHT: f32 = 900.0;
const MENU_SCALE_MAX: f32 = 1.5;

/// Viewport-derived size factor for menus that should grow on large screens.
///
/// GPUI already converts device pixels to logical pixels, so this only grows
/// menus when the logical viewport is larger than the reference, e.g. a 4K
/// monitor at 100% scaling. The factor never shrinks below the 1x design:
/// cramped viewports keep the clamping and scrolling the menus already have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MenuScale(f32);

impl MenuScale {
    pub(crate) const BASE: Self = Self(1.0);

    pub(crate) fn for_viewport(viewport: Size<Pixels>) -> Self {
        let fit = (f32::from(viewport.width) / MENU_SCALE_REFERENCE_WIDTH)
            .min(f32::from(viewport.height) / MENU_SCALE_REFERENCE_HEIGHT);
        Self::clamped(fit)
    }

    /// Keeps only `share` of this scale's growth above the 1x design, for small
    /// popups that should stay compact beside the panels that grow fully.
    pub(crate) fn with_growth_share(self, share: f32) -> Self {
        Self::clamped(1.0 + (self.0 - 1.0) * share.clamp(0.0, 1.0))
    }

    fn clamped(factor: f32) -> Self {
        Self(if factor.is_finite() {
            factor.clamp(1.0, MENU_SCALE_MAX)
        } else {
            1.0
        })
    }

    pub(crate) const fn factor(self) -> f32 {
        self.0
    }

    /// Scales a 1x logical length.
    pub(crate) fn len(self, logical: f32) -> f32 {
        logical * self.0
    }

    pub(crate) fn px(self, logical: f32) -> Pixels {
        px(self.len(logical))
    }

    pub(crate) fn rems(self, value: f32) -> Rems {
        rems(value * self.0)
    }
}

/// Normalized origin used to choose the direction of native menu motion.
///
/// GPUI 0.2 does not expose a general affine transform for element trees, so
/// `Aeris` uses a short vertical fade/translation. The vertical direction
/// follows the trigger so menus above and below it settle toward their anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PopupAnimationOrigin {
    x: f32,
    y: f32,
}

impl PopupAnimationOrigin {
    pub(crate) const TOP_LEFT: Self = Self { x: 0.0, y: 0.0 };
    pub(crate) const TOP_RIGHT: Self = Self { x: 1.0, y: 0.0 };
    pub(crate) const BOTTOM_LEFT: Self = Self { x: 0.0, y: 1.0 };

    pub(crate) fn new(x: f32, y: f32) -> Self {
        Self {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
        }
    }

    pub(crate) fn from_trigger(trigger: Point<Pixels>, popup: Bounds<Pixels>) -> Self {
        let width: f32 = popup.size.width.into();
        let height: f32 = popup.size.height.into();
        let left: f32 = popup.origin.x.into();
        let top: f32 = popup.origin.y.into();
        let trigger_x: f32 = trigger.x.into();
        let trigger_y: f32 = trigger.y.into();
        Self::new(
            if width > 0.0 {
                (trigger_x - left) / width
            } else {
                0.5
            },
            if height > 0.0 {
                (trigger_y - top) / height
            } else {
                0.5
            },
        )
    }

    pub(crate) fn enter_offset(self) -> Point<f32> {
        point(0.0, (self.y - 0.5) * 2.0 * POPUP_ENTER_TRAVEL)
    }
}

/// Applies the shared trigger-origin entry motion to an `Aeris` popup.
/// `with_animation` automatically collapses to its final frame for reduced
/// motion, so every caller gets the accessibility behavior for free.
pub(crate) fn animate_popup_from_origin(
    panel: Stateful<Div>,
    id: impl Into<ElementId>,
    origin: PopupAnimationOrigin,
) -> AnimationElement<Stateful<Div>> {
    let offset = origin.enter_offset();
    panel.with_animation(
        id,
        Animation::new(POPUP_ENTER_DURATION).with_easing(ease_out_quint()),
        move |panel, progress| {
            let remaining = 1.0 - progress;
            panel
                .opacity(0.3 + 0.7 * progress)
                .mt(px(offset.y * remaining))
        },
    )
}

#[derive(Clone, Copy)]
enum RowKind {
    Compact,
    SearchResult,
}

fn row_geometry(kind: RowKind, scale: MenuScale) -> (Rems, Rems) {
    let (height, padding) = match kind {
        RowKind::Compact => (COMPACT_ROW_HEIGHT, Rems(0.5)),
        RowKind::SearchResult => (SEARCH_ROW_HEIGHT, Rems(0.5)),
    };
    (scale.rems(height.0), scale.rems(padding.0))
}

const fn accepts_input(disabled: bool, has_activation: bool) -> bool {
    !disabled && has_activation
}

/// The label and leading-glyph colours of a row. A destructive row keeps the shared row fills
/// and colours its text and glyph from the danger tokens: `text-danger` at rest, `danger-hover`
/// while hovered or highlighted, and `danger-disabled` while disabled.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowInk {
    label: ThemeColor,
    icon: ThemeColor,
    /// Label and glyph colour while hovered or highlighted; `None` keeps the resting colours.
    hover: Option<ThemeColor>,
}

const fn row_ink(theme: &AerisTheme, destructive: bool, enabled: bool) -> RowInk {
    let colors = theme.colors;
    match (destructive, enabled) {
        (true, true) => RowInk {
            label: colors.text_danger,
            icon: colors.text_danger,
            hover: Some(colors.danger_hover),
        },
        (true, false) => RowInk {
            label: colors.danger_disabled,
            icon: colors.danger_disabled,
            hover: None,
        },
        (false, true) => RowInk {
            label: colors.text_primary,
            icon: colors.icon,
            hover: None,
        },
        (false, false) => RowInk {
            label: colors.text_muted,
            icon: colors.text_muted,
            hover: None,
        },
    }
}

/// `Aeris`'s shared selectable row for menus and search results.
///
/// A row is always full width with its own `--radius-small` corners, and sits inside a
/// [`MenuPanel`] (or the search-menu body) that insets it from the panel's rounded edge, so its
/// highlight never needs to know where in the panel it is.
#[derive(IntoElement)]
pub(crate) struct MenuRow {
    id: ElementId,
    kind: RowKind,
    scale: MenuScale,
    theme: AerisTheme,
    resting_fill: ThemeColor,
    label: SharedString,
    leading_icon: Option<Icon>,
    leading: Option<AnyElement>,
    detail: Option<SharedString>,
    trailing: Option<AnyElement>,
    activation: Option<Activation>,
    hover: Option<Hover>,
    behavior: MenuRowBehavior,
}

#[derive(Default)]
struct MenuRowBehavior {
    highlighted: bool,
    disabled: bool,
    destructive: bool,
    checked: Option<bool>,
}

#[derive(Clone, Copy)]
struct MenuRowPresentation {
    enabled: bool,
    label_ink: ThemeColor,
    icon_ink: ThemeColor,
    /// The label and glyph colour while the pointer is over an enabled, unhighlighted row.
    hover_ink: Option<ThemeColor>,
    highlighted_fill: ThemeColor,
    hover_fill: ThemeColor,
    height: Rems,
    horizontal_padding: Rems,
}

impl MenuRow {
    pub(crate) fn compact(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AerisTheme,
    ) -> Self {
        Self::new(id, label, theme, RowKind::Compact)
    }

    pub(crate) fn search_result(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AerisTheme,
    ) -> Self {
        let mut row = Self::new(id, label, theme, RowKind::SearchResult);
        row.resting_fill = theme.colors.surface;
        row
    }

    fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AerisTheme,
        kind: RowKind,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            scale: MenuScale::BASE,
            theme: *theme,
            resting_fill: theme.colors.surface_secondary,
            label: label.into(),
            leading_icon: None,
            leading: None,
            detail: None,
            trailing: None,
            activation: None,
            hover: None,
            behavior: MenuRowBehavior::default(),
        }
    }

    /// Sizes the row, its text and its spacing for a screen-aware menu.
    pub(crate) fn scale(mut self, scale: MenuScale) -> Self {
        self.scale = scale;
        self
    }

    pub(crate) fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    /// A leading glyph the row colours itself, so it always matches the label in every
    /// state, including a destructive row's `danger-hover` while hovered.
    pub(crate) fn leading_icon(mut self, icon: Icon) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    /// Muted secondary text after the label, such as an instrument's kind.
    pub(crate) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(crate) fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub(crate) fn highlighted(mut self, highlighted: bool) -> Self {
        self.behavior.highlighted = highlighted;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.behavior.disabled = disabled;
        self
    }

    pub(crate) fn destructive(mut self, destructive: bool) -> Self {
        self.behavior.destructive = destructive;
        self
    }

    /// A checkable option: a trailing check while `checked`, announced as toggled. The check
    /// takes the trailing slot, so a checkable row carries no other trailing content.
    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.behavior.checked = Some(checked);
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Rc::new(handler));
        self
    }

    pub(crate) fn on_hover(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover = Some(Rc::new(handler));
        self
    }

    /// The resting colour for a glyph a row cannot colour itself (a trailing arrow), matched to
    /// the label for the same `destructive` and enabled state.
    pub(crate) const fn leading_icon_color(
        theme: &AerisTheme,
        destructive: bool,
        enabled: bool,
    ) -> ThemeColor {
        row_ink(theme, destructive, enabled).icon
    }

    fn presentation(&self) -> MenuRowPresentation {
        let colors = self.theme.colors;
        let enabled = accepts_input(self.behavior.disabled, self.activation.is_some());
        let destructive = self.behavior.destructive;
        let highlighted = self.behavior.highlighted;
        let ink = row_ink(&self.theme, destructive, enabled);
        // A highlighted row is already in its hover look, so it takes the hover ink outright; an
        // enabled row switches to it only while the pointer is over the row.
        let (label_ink, icon_ink) = match ink.hover.filter(|_| highlighted) {
            Some(hover) => (hover, hover),
            None => (ink.label, ink.icon),
        };
        let hover_ink = ink.hover.filter(|_| enabled && !highlighted);
        let highlighted_fill = match self.kind {
            RowKind::Compact => colors.hover_bg,
            RowKind::SearchResult => colors.active_bg,
        }
        .over(self.resting_fill);
        let hover_fill = colors.hover_bg.over(self.resting_fill);
        let (height, horizontal_padding) = row_geometry(self.kind, self.scale);
        MenuRowPresentation {
            enabled,
            label_ink,
            icon_ink,
            hover_ink,
            highlighted_fill,
            hover_fill,
            height,
            horizontal_padding,
        }
    }

    fn check_mark(&self) -> Option<AnyElement> {
        self.behavior.checked.filter(|checked| *checked).map(|_| {
            Icon::new(UiIcon::CheckIcon.path())
                .with_size(px(16.0))
                .color(gpui_color(self.theme.colors.icon_active))
                .into_any_element()
        })
    }
}

impl RenderOnce for MenuRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let presentation = self.presentation();
        let check_mark = self.check_mark();
        let hover = self.hover;
        let scale = self.scale;
        let highlighted = self.behavior.highlighted;
        let (label_ink, hover_ink) = (presentation.label_ink, presentation.hover_ink);
        let leading_icon = self.leading_icon.map(|icon| {
            icon.color(gpui_color(presentation.icon_ink))
                .when_some(hover_ink, |icon, hover_ink| {
                    icon.group_hover_color(MENU_ROW_GROUP, gpui_color(hover_ink))
                })
        });
        // GPUI fixes a text run's colour at layout, before the row's group hitbox exists, so a
        // group-hover text colour only applies to an element that keeps hover state: one with an
        // id. Without it the glyph (coloured at paint) turns while the label never does.
        let label = div()
            .id("menu_row_label")
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(gpui_color(label_ink))
            .when_some(hover_ink, |label, hover_ink| {
                label.group_hover(MENU_ROW_GROUP, move |style| {
                    style.text_color(gpui_color(hover_ink))
                })
            })
            .child(self.label.clone());

        let radius = px(f32::from(RadiusToken::Sm.logical_pixels()));
        // The visible pill: inset inside the row so neighbouring fills and dividers never touch.
        let fill = div()
            .size_full()
            .flex()
            .items_center()
            .gap(scale.rems(0.5))
            .px(presentation.horizontal_padding)
            .rounded(radius)
            .when(highlighted, |fill| {
                fill.bg(gpui_color(presentation.highlighted_fill))
            })
            .when(presentation.enabled, |fill| {
                fill.group_hover(MENU_ROW_GROUP, |style| {
                    style.bg(gpui_color(presentation.hover_fill))
                })
            })
            .children(leading_icon)
            .children(self.leading)
            .child(label)
            .children(self.detail.map(|detail| {
                div()
                    .flex_none()
                    .max_w(scale.rems(10.0))
                    .truncate()
                    .text_size(scale.rems(0.75))
                    .text_color(gpui_color(colors.text_muted))
                    .child(detail)
            }))
            .children(check_mark.or(self.trailing));

        BaseButton::new(self.id)
            .group(MENU_ROW_GROUP)
            .disabled(!presentation.enabled)
            .accessibility_label(self.label.clone())
            .when_some(self.behavior.checked, |row, checked| {
                row.aria_toggled(if checked {
                    Toggled::True
                } else {
                    Toggled::False
                })
            })
            .block_mouse_except_scroll()
            .when_some(hover, |row, hover| {
                row.on_hover(move |hovered, window, cx| hover(hovered, window, cx))
            })
            .w_full()
            .h(presentation.height)
            .flex_none()
            .py(scale.rems(ROW_FILL_INSET.0))
            .rounded(radius)
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .text_size(scale.rems(0.875))
            .text_color(gpui_color(label_ink))
            .when(presentation.enabled, gpui::Styled::cursor_pointer)
            .when(!presentation.enabled, gpui::Styled::cursor_not_allowed)
            .focus_visible(move |row| row.border_2().border_color(gpui_color(colors.ring_primary)))
            .when_some(
                self.activation.filter(|_| presentation.enabled),
                |row, activation| {
                    row.on_click(move |event, window, cx| {
                        activation(event, window, cx);
                        cx.stop_propagation();
                    })
                },
            )
            .child(fill)
    }
}

/// Where a [`MenuPanel`] sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MenuPlacement {
    /// Top-left corner at a point in the positioned overlay layer that hosts the menu. The
    /// owner clamps the point into the viewport with [`menu_panel_height`].
    At(Point<Pixels>),
    /// Opens from a [`MenuAnchor`] trigger, below or above it.
    Anchored { side: MenuSide, align: MenuAlign },
    /// Laid out by its parent, e.g. a flex overlay that positions the menu itself.
    InFlow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MenuSide {
    Below,
    Above,
}

/// Which trigger edges an anchored menu lines up with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MenuAlign {
    Start,
    /// The menu's right edge on the trigger's, for triggers at a trailing edge.
    End,
    /// The trigger's full width.
    Stretch,
}

/// Space between a trigger and the menu it opens.
const ANCHOR_GAP: Pixels = px(4.0);

/// `Aeris`'s one menu surface: dropdowns, context menus, flyouts and select menus.
///
/// The panel owns the flat menu look (platform border, `--radius-default`, `surface-secondary`,
/// no shadow), its placement, scrolling, entry motion and click containment. Its content is
/// inset from the rounded border: GPUI clips to rectangles, and any inset of at least
/// `r(1 - 1/√2)` keeps a row's rectangular highlight inside the panel's rounded corner, so rows
/// never depend on their position in the panel.
#[derive(IntoElement)]
pub(crate) struct MenuPanel {
    id: ElementId,
    theme: AerisTheme,
    placement: MenuPlacement,
    scale: MenuScale,
    width: Option<Length>,
    max_height: Option<Length>,
    header: Option<AnyElement>,
    scroll: Option<ScrollHandle>,
    animation: Option<PopupAnimationOrigin>,
    children: Vec<AnyElement>,
}

impl MenuPanel {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        placement: MenuPlacement,
        theme: &AerisTheme,
    ) -> Self {
        Self {
            id: id.into(),
            theme: *theme,
            placement,
            scale: MenuScale::BASE,
            width: None,
            max_height: None,
            header: None,
            scroll: None,
            animation: None,
            children: Vec::new(),
        }
    }

    /// Grows the inset with a screen-aware menu whose rows use the same scale.
    pub(crate) fn scale(mut self, scale: MenuScale) -> Self {
        self.scale = scale;
        self
    }

    /// A fixed width; without it the panel sizes to its content, or to the trigger when
    /// stretched.
    pub(crate) fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = Some(width.into());
        self
    }

    /// Caps the panel height; the rows below the header scroll inside the cap.
    pub(crate) fn max_height(mut self, height: impl Into<Length>) -> Self {
        self.max_height = Some(height.into());
        self
    }

    /// Content that stays above the scrolling rows, such as a search field.
    pub(crate) fn header(mut self, header: impl IntoElement) -> Self {
        self.header = Some(header.into_any_element());
        self
    }

    /// Tracks the scrolling rows, e.g. to keep the keyboard selection in view.
    pub(crate) fn track_scroll(mut self, scroll: &ScrollHandle) -> Self {
        self.scroll = Some(scroll.clone());
        self
    }

    /// The shared entry motion, settling toward the edge the menu opened from.
    pub(crate) fn animate_from(mut self, origin: PopupAnimationOrigin) -> Self {
        self.animation = Some(origin);
        self
    }

    /// A divider between groups of rows. Its line runs the panel's full width, through the
    /// inset to both borders, while rows stay inset.
    pub(crate) fn separator(mut self) -> Self {
        let inset = self.scale.rems(PANEL_INSET.0);
        self.children
            .push(divider(&self.theme).mx(Rems(-inset.0)).into_any_element());
        self
    }
}

impl ParentElement for MenuPanel {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// The height a panel adds around its content: the inset and border on both edges.
pub(crate) fn menu_panel_chrome_height(
    theme: &AerisTheme,
    scale: MenuScale,
    rem_size: Pixels,
) -> Pixels {
    let inset = rem_size * scale.rems(PANEL_INSET.0).0;
    (inset + platform_border_width(theme)) * 2.0
}

/// Height of a panel holding `rows` compact rows and `separators` separators, so an owner can
/// clamp the panel into the viewport before it is laid out.
pub(crate) fn menu_panel_height(
    rows: usize,
    separators: usize,
    theme: &AerisTheme,
    scale: MenuScale,
    rem_size: Pixels,
) -> Pixels {
    let row = rem_size * scale.rems(COMPACT_ROW_HEIGHT.0).0;
    let rows = f32::from(u16::try_from(rows).unwrap_or(u16::MAX));
    let separators = f32::from(u16::try_from(separators).unwrap_or(u16::MAX));
    row * rows + SEPARATOR_HEIGHT * separators + menu_panel_chrome_height(theme, scale, rem_size)
}

impl RenderOnce for MenuPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let inset = self.scale.rems(PANEL_INSET.0);
        let body = div()
            .id("menu_panel_body")
            .flex()
            .flex_col()
            .p(inset)
            .children(self.children)
            .when(self.max_height.is_some(), |body| {
                body.flex_1().min_h_0().overflow_y_scroll()
            })
            .when_some(self.scroll, |body, scroll| body.track_scroll(&scroll));
        let panel = div()
            .id(self.id.clone())
            .occlude()
            .flex()
            .flex_col()
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
            .border(platform_border_width(&self.theme))
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface_secondary))
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .text_color(gpui_color(colors.text_primary))
            .when_some(self.width, Styled::w)
            .when_some(self.max_height, Styled::max_h)
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .map(|panel| match self.placement {
                MenuPlacement::At(origin) => panel.absolute().left(origin.x).top(origin.y),
                MenuPlacement::Anchored { side, align } => {
                    let panel = match side {
                        MenuSide::Below => panel.absolute().top_full().mt(ANCHOR_GAP),
                        MenuSide::Above => panel.absolute().bottom_full().mb(ANCHOR_GAP),
                    };
                    match align {
                        MenuAlign::Start => panel.left_0(),
                        MenuAlign::End => panel.right_0(),
                        MenuAlign::Stretch => panel.left_0().right_0(),
                    }
                }
                MenuPlacement::InFlow => panel,
            })
            .children(self.header)
            .child(body);
        match self.animation {
            Some(origin) => animate_popup_from_origin(
                panel,
                ElementId::NamedChild(Arc::new(self.id), "enter".into()),
                origin,
            )
            .into_any_element(),
            None => panel.into_any_element(),
        }
    }
}

/// A trigger and the menu it opens. The menu renders above the surrounding layout, and any
/// press outside the trigger and menu dismisses it, so pressing the trigger again toggles it
/// closed instead of reopening it.
#[derive(IntoElement)]
pub(crate) struct MenuAnchor {
    id: ElementId,
    trigger: AnyElement,
    menu: Option<MenuPanel>,
    on_dismiss: Option<Dismiss>,
    full_width: bool,
}

impl MenuAnchor {
    pub(crate) fn new(id: impl Into<ElementId>, trigger: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            trigger: trigger.into_any_element(),
            menu: None,
            on_dismiss: None,
            full_width: false,
        }
    }

    /// The open menu, placed with [`MenuPlacement::Anchored`]; `None` while closed.
    pub(crate) fn menu(mut self, menu: Option<MenuPanel>) -> Self {
        self.menu = menu;
        self
    }

    pub(crate) fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }

    /// Lets a full-width trigger, such as a form select, fill its row.
    pub(crate) fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
}

impl RenderOnce for MenuAnchor {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let open = self.menu.is_some();
        div()
            .id(self.id)
            .relative()
            .flex_none()
            .when(self.full_width, Styled::w_full)
            .child(self.trigger)
            .when_some(self.on_dismiss.filter(|_| open), |anchor, dismiss| {
                anchor.on_mouse_down_out(move |_, window, cx| dismiss(window, cx))
            })
            .children(self.menu.map(gpui::deferred))
    }
}

/// A divider's line with its clear space; [`MenuPanel::separator`] stretches it to full width.
fn divider(theme: &AerisTheme) -> Div {
    div()
        .h(SEPARATOR_HEIGHT)
        .flex_none()
        .flex()
        .items_center()
        .child(
            div()
                .h_px()
                .w_full()
                .bg(gpui_color(theme.colors.border_secondary)),
        )
}

#[cfg(test)]
mod tests {
    use aeris_design_system::{AerisTheme, RadiusToken};
    use gpui::{Bounds, Rems, point, px, size};

    use super::{
        COMPACT_ROW_HEIGHT, MENU_SCALE_MAX, MENU_SEPARATOR_HEIGHT, MenuRow, MenuScale, PANEL_INSET,
        POPUP_ENTER_TRAVEL, PopupAnimationOrigin, ROOT_REM_PX, ROW_FILL_INSET, RowInk, RowKind,
        SEARCH_ROW_HEIGHT, accepts_input, menu_panel_height, row_geometry, row_ink,
    };

    #[test]
    fn neighbouring_row_fills_and_dividers_never_touch() {
        // Each row insets its fill on both edges, so two filled rows leave twice the inset
        // between them, and a divider keeps clear space around its 1px line on top of that.
        let inset = ROW_FILL_INSET.0 * ROOT_REM_PX;
        assert!(2.0 * inset >= 4.0, "filled rows need a visible gap");
        let divider_clearance = (MENU_SEPARATOR_HEIGHT - 1.0) / 2.0 + inset;
        assert!(
            divider_clearance >= 4.0,
            "a divider needs clear space from row fills"
        );
    }

    #[test]
    fn disabled_rows_never_accept_activation() {
        assert!(accepts_input(false, true));
        assert!(!accepts_input(true, true));
        assert!(!accepts_input(false, false));
    }

    #[test]
    fn destructive_rows_follow_the_danger_ramp() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let c = theme.colors;
            let enabled = row_ink(&theme, true, true);
            assert_eq!(
                enabled,
                RowInk {
                    label: c.text_danger,
                    icon: c.text_danger,
                    hover: Some(c.danger_hover),
                }
            );
            assert_eq!(
                row_ink(&theme, true, false),
                RowInk {
                    label: c.danger_disabled,
                    icon: c.danger_disabled,
                    hover: None,
                }
            );
            let destructive = MenuRow::compact("remove", "Remove drawings", &theme)
                .destructive(true)
                .on_click(|_, _, _| {})
                .presentation();
            let plain = MenuRow::compact("reset", "Reset view", &theme)
                .on_click(|_, _, _| {})
                .presentation();
            // Only the text and glyph turn red; the row fills stay the shared neutral ones.
            assert_eq!(destructive.hover_fill, plain.hover_fill);
            assert_eq!(destructive.highlighted_fill, plain.highlighted_fill);
            assert_eq!(destructive.label_ink, c.text_danger);
            assert_eq!(destructive.icon_ink, c.text_danger);
            assert_eq!(destructive.hover_ink, Some(c.danger_hover));
            let highlighted = MenuRow::compact("remove", "Remove drawings", &theme)
                .destructive(true)
                .highlighted(true)
                .on_click(|_, _, _| {})
                .presentation();
            assert_eq!(highlighted.label_ink, c.danger_hover);
            assert_eq!(highlighted.icon_ink, c.danger_hover);
            assert_eq!(highlighted.hover_ink, None);
            assert_eq!(row_ink(&theme, false, false).label, c.text_muted);
            assert_eq!(row_ink(&theme, false, true).icon, c.icon);
            assert_eq!(row_ink(&theme, false, true).hover, None);
        }
    }

    #[test]
    fn row_kinds_preserve_menu_geometry() {
        assert_eq!(
            row_geometry(RowKind::Compact, MenuScale::BASE),
            (COMPACT_ROW_HEIGHT, Rems(0.5))
        );
        assert_eq!(
            row_geometry(RowKind::SearchResult, MenuScale::BASE),
            (SEARCH_ROW_HEIGHT, Rems(0.5))
        );
        assert_eq!(
            row_geometry(RowKind::SearchResult, MenuScale::clamped(1.5)),
            (Rems(3.375), Rems(0.75))
        );
    }

    #[test]
    fn panel_inset_keeps_square_row_corners_inside_the_rounded_border() {
        // A row corner at (inset, inset) from the panel's inner edge must lie inside the corner
        // circle of the inner radius, whatever row is highlighted or scrolled to the edge.
        let inner_radius = f32::from(RadiusToken::Default.logical_pixels())
            - AerisTheme::light().dimensions.border_width;
        let inset = PANEL_INSET.0 * ROOT_REM_PX;
        let offset = inner_radius - inset;
        assert!(
            (offset * offset * 2.0).sqrt() <= inner_radius,
            "a {inset}px inset leaves row corners outside the {inner_radius}px corner"
        );
    }

    #[test]
    fn panel_height_adds_rows_separators_inset_and_border() {
        let theme = AerisTheme::light();
        let height = menu_panel_height(3, 1, &theme, MenuScale::BASE, px(16.0));
        let expected =
            3.0 * 32.0 + MENU_SEPARATOR_HEIGHT + 2.0 * (4.0 + theme.dimensions.border_width);
        assert!((f32::from(height) - expected).abs() < 1e-4);
    }

    #[test]
    fn menu_scale_grows_with_large_viewports_and_stays_bounded() {
        let scale =
            |width: f32, height: f32| MenuScale::for_viewport(size(px(width), px(height))).factor();
        assert!((scale(1440.0, 900.0) - 1.0).abs() < f32::EPSILON);
        assert!((scale(1280.0, 720.0) - 1.0).abs() < f32::EPSILON);
        assert!((scale(1920.0, 1080.0) - 1.2).abs() < 1e-5);
        // The narrower axis wins so an ultrawide window does not inflate menus.
        assert!((scale(3440.0, 900.0) - 1.0).abs() < f32::EPSILON);
        assert!((scale(3840.0, 2160.0) - MENU_SCALE_MAX).abs() < f32::EPSILON);
        assert!((scale(0.0, 0.0) - 1.0).abs() < f32::EPSILON);
        let large = MenuScale::for_viewport(size(px(3840.0), px(2160.0)));
        assert!((large.with_growth_share(0.5).factor() - 1.25).abs() < 1e-5);
        assert!((large.with_growth_share(0.0).factor() - 1.0).abs() < f32::EPSILON);
        assert!((MenuScale::BASE.with_growth_share(0.5).factor() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn popup_origin_tracks_and_clamps_the_trigger_inside_popup_bounds() {
        let popup = Bounds::new(point(px(100.0), px(50.0)), size(px(200.0), px(100.0)));

        assert_eq!(
            PopupAnimationOrigin::from_trigger(point(px(100.0), px(50.0)), popup),
            PopupAnimationOrigin::TOP_LEFT
        );
        assert_eq!(
            PopupAnimationOrigin::from_trigger(point(px(300.0), px(50.0)), popup),
            PopupAnimationOrigin::TOP_RIGHT
        );
        assert_eq!(
            PopupAnimationOrigin::from_trigger(point(px(0.0), px(500.0)), popup),
            PopupAnimationOrigin::BOTTOM_LEFT
        );
    }

    #[test]
    fn popup_entry_offset_moves_only_toward_the_trigger_edge() {
        assert_eq!(
            PopupAnimationOrigin::TOP_LEFT.enter_offset(),
            point(0.0, -POPUP_ENTER_TRAVEL)
        );
        assert_eq!(
            PopupAnimationOrigin::TOP_RIGHT.enter_offset(),
            point(0.0, -POPUP_ENTER_TRAVEL)
        );
        assert_eq!(
            PopupAnimationOrigin::BOTTOM_LEFT.enter_offset(),
            point(0.0, POPUP_ENTER_TRAVEL)
        );
    }
}
