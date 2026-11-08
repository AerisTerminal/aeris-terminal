use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, ThemeMode, TypographyRole, platform_font_family,
    platform_typography,
};
use gpui::{Div, FontWeight, Hsla, Pixels, Styled, div, px};
use gpui_base::{ColorTokens, RadiusTokens, Theme, ThemeAppearance};

/// Text-field tokens, exactly as the Theme System `Input` uses them: a `border` field on
/// `surface` (a `border-secondary` field on `surface-secondary` in dark mode) that takes
/// `hover-bg` on hover, the shared 2px
/// `ring-primary` focus outline, and a `danger` border with a 3px `danger-ring` halo while invalid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InputAppearance {
    pub(crate) fill: ThemeColor,
    pub(crate) hover_fill: ThemeColor,
    pub(crate) border: ThemeColor,
    pub(crate) focus_ring: ThemeColor,
    pub(crate) invalid_border: ThemeColor,
    pub(crate) invalid_ring: ThemeColor,
}

pub(crate) fn input_appearance(theme: &AerisTheme) -> InputAppearance {
    let colors = &theme.colors;
    InputAppearance {
        fill: field_surface(theme),
        hover_fill: colors.hover_bg,
        border: field_border(theme),
        focus_ring: colors.ring_primary,
        invalid_border: colors.danger,
        invalid_ring: colors.danger_ring,
    }
}

/// The Theme System `bg-surface dark:bg-surface-secondary` pair for fields and setting rows
/// inside a layer: plain `surface` in light mode, `surface-secondary` in dark mode.
pub(crate) const fn field_surface(theme: &AerisTheme) -> ThemeColor {
    match theme.mode {
        ThemeMode::Light => theme.colors.surface,
        ThemeMode::Dark => theme.colors.surface_secondary,
    }
}

/// The Theme System `border dark:border-border-secondary` pair for fields: `border` in light
/// mode, `border-secondary` in dark mode so the edge stays visible on `surface-secondary`.
pub(crate) const fn field_border(theme: &AerisTheme) -> ThemeColor {
    match theme.mode {
        ThemeMode::Light => theme.colors.border,
        ThemeMode::Dark => theme.colors.border_secondary,
    }
}

/// The sides of a docked workspace panel that face a `PANEL_GAP` to another panel. A side
/// on the window edge is not a gap, and the corners on it stay square.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PanelEdges(u8);

impl PanelEdges {
    pub(crate) const TOP: Self = Self(1);
    pub(crate) const RIGHT: Self = Self(1 << 1);
    pub(crate) const BOTTOM: Self = Self(1 << 2);
    pub(crate) const LEFT: Self = Self(1 << 3);
    /// A gap on every side.
    pub(crate) const INNER: Self =
        Self(Self::TOP.0 | Self::RIGHT.0 | Self::BOTTOM.0 | Self::LEFT.0);

    /// These edges with `side` facing a gap or not.
    pub(crate) const fn with(self, side: Self, gap: bool) -> Self {
        if gap {
            Self(self.0 | side.0)
        } else {
            Self(self.0 & !side.0)
        }
    }

    pub(crate) const fn has(self, side: Self) -> bool {
        self.0 & side.0 == side.0
    }

    /// Top-left, top-right, bottom-right and bottom-left: a corner is rounded only between
    /// two gap sides.
    const fn rounded_corners(self) -> [bool; 4] {
        [
            self.has(Self(Self::TOP.0 | Self::LEFT.0)),
            self.has(Self(Self::TOP.0 | Self::RIGHT.0)),
            self.has(Self(Self::BOTTOM.0 | Self::RIGHT.0)),
            self.has(Self(Self::BOTTOM.0 | Self::LEFT.0)),
        ]
    }
}

/// Rounds the corners of a docked workspace panel that face gaps to `--radius-small`; add it
/// as the panel's last painted child. GPUI clips children to rectangles only, so a chart
/// canvas or a filled row would still paint square corners. This ring paints
/// `surface-backdrop` over each rounded corner outside the radius: the shader draws a
/// border's inner corner with the outer radius minus the border width, so a ring one radius
/// wide sits exactly on the panel's rounded edge, and at a square corner (outer radius 0) it
/// stays outside the panel. It reaches one radius past the panel, never beyond the gap.
pub(crate) fn panel_corners(theme: &AerisTheme, edges: PanelEdges) -> Option<Div> {
    let radius = f32::from(RadiusToken::Sm.logical_pixels());
    let [top_left, top_right, bottom_right, bottom_left] = edges
        .rounded_corners()
        .map(|rounded| px(if rounded { radius * 2.0 } else { 0.0 }));
    edges.rounded_corners().contains(&true).then(|| {
        div()
            .absolute()
            .top(px(-radius))
            .left(px(-radius))
            .right(px(-radius))
            .bottom(px(-radius))
            .rounded_tl(top_left)
            .rounded_tr(top_right)
            .rounded_br(bottom_right)
            .rounded_bl(bottom_left)
            .border(px(radius))
            .border_color(gpui_color(theme.colors.surface_backdrop))
    })
}

/// The `primary` edge of the focused chart pane, one border width inside the pane; add it
/// before `panel_corners`. Where the corner ring is painted, the edge reaches one extra
/// pixel under it on a curve concentric with the ring's inner edge, so the ring's
/// anti-aliased edge blends only with `primary` and no pane content shows between them.
pub(crate) fn panel_focus_border(theme: &AerisTheme, edges: PanelEdges) -> Div {
    let radius = f32::from(RadiusToken::Sm.logical_pixels());
    let reach = if edges.rounded_corners().contains(&true) {
        1.0
    } else {
        0.0
    };
    let [top_left, top_right, bottom_right, bottom_left] = edges
        .rounded_corners()
        .map(|rounded| px(if rounded { radius + reach } else { 0.0 }));
    div()
        .absolute()
        .top(px(-reach))
        .left(px(-reach))
        .right(px(-reach))
        .bottom(px(-reach))
        .rounded_tl(top_left)
        .rounded_tr(top_right)
        .rounded_br(bottom_right)
        .rounded_bl(bottom_left)
        .border(platform_border_width(theme) + px(reach))
        .border_color(gpui_color(theme.colors.primary))
}

pub(crate) fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}

pub(crate) fn platform_border_width(theme: &AerisTheme) -> Pixels {
    px(theme.dimensions.border_width)
}

/// Floating-layer chrome (dialogs, popovers, toasts, hints and floating panels): the Theme
/// System `surface-secondary` fill with a 1px `border-secondary` edge, in every theme.
pub(crate) fn popover_surface<E: Styled>(element: E, theme: &AerisTheme, radius: Pixels) -> E {
    let colors = &theme.colors;
    element
        .rounded(radius)
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
}

/// Menu chrome (dropdowns, context menus, flyouts and select menus): the primary `surface`
/// with its 1px `border` edge.
pub(crate) fn menu_surface<E: Styled>(element: E, theme: &AerisTheme, radius: Pixels) -> E {
    let colors = &theme.colors;
    element
        .rounded(radius)
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
}

/// Projects `Aeris`'s semantic design contract into the unstyled Base
/// foundation. Base behavior modules can then resolve unset semantics without
/// introducing a second palette or typography source.
pub(crate) fn base_theme(theme: &AerisTheme) -> Theme {
    let colors = theme.colors;
    let mut base = Theme {
        appearance: match theme.mode {
            ThemeMode::Light => ThemeAppearance::Light,
            ThemeMode::Dark => ThemeAppearance::Dark,
        },
        ..Theme::default()
    };
    let mut selection = gpui_color(colors.primary);
    selection.a = 0.30;
    base.tokens.colors = ColorTokens {
        background: gpui_color(colors.surface),
        foreground: gpui_color(colors.text_default),
        surface: gpui_color(colors.surface),
        surface_foreground: gpui_color(colors.text_default),
        primary: gpui_color(colors.primary),
        primary_foreground: gpui_color(colors.primary_foreground),
        secondary: gpui_color(colors.surface_secondary),
        secondary_foreground: gpui_color(colors.text_default),
        muted: gpui_color(colors.surface_secondary),
        muted_foreground: gpui_color(colors.text_secondary),
        accent: gpui_color(colors.primary),
        accent_foreground: gpui_color(colors.primary_foreground),
        destructive: gpui_color(colors.danger),
        destructive_foreground: gpui_color(colors.danger_foreground),
        border: gpui_color(colors.border),
        input: gpui_color(colors.border_secondary),
        ring: gpui_color(colors.ring_primary),
        selection,
    };
    base.tokens.radius = RadiusTokens {
        none: px(0.0),
        sm: px(f32::from(RadiusToken::Sm.logical_pixels())),
        md: px(f32::from(RadiusToken::Default.logical_pixels())),
        lg: px(f32::from(RadiusToken::Default.logical_pixels())),
        xl: px(f32::from(RadiusToken::Default.logical_pixels())),
        full: px(f32::from(RadiusToken::Full.logical_pixels())),
    };
    base.tokens.typography.sans = platform_font_family().into();
    let normal_weight = FontWeight(f32::from(
        platform_typography().weight(TypographyRole::Normal),
    ));
    for text_style in [
        &mut base.tokens.typography.xs,
        &mut base.tokens.typography.sm,
        &mut base.tokens.typography.md,
        &mut base.tokens.typography.lg,
        &mut base.tokens.typography.xl,
    ] {
        text_style.weight = normal_weight;
    }
    base
}

#[cfg(test)]
mod tests {
    use aeris_design_system::{AerisTheme, ThemeMode};

    use gpui::Styled;

    use super::{
        PanelEdges, base_theme, gpui_color, input_appearance, menu_surface, popover_surface,
    };

    #[test]
    fn only_panel_corners_between_two_gaps_are_rounded() {
        assert_eq!(PanelEdges::INNER.rounded_corners(), [true; 4]);
        assert_eq!(PanelEdges::default().rounded_corners(), [false; 4]);
        // A panel against the window's left edge, like the drawing toolbar.
        let left_edge = PanelEdges::INNER.with(PanelEdges::LEFT, false);
        assert_eq!(left_edge.rounded_corners(), [false, true, true, false]);
        assert!(!left_edge.has(PanelEdges::LEFT) && left_edge.has(PanelEdges::RIGHT));
    }

    #[test]
    fn panel_corner_ring_stays_inside_the_gap_between_panels() {
        assert!(
            f32::from(aeris_design_system::RadiusToken::Sm.logical_pixels())
                <= super::super::super::PANEL_GAP
        );
    }

    #[test]
    fn floating_layers_sit_on_the_secondary_surface_and_menus_on_the_primary_one() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let colors = theme.colors;
            for (mut layer, fill, edge) in [
                (
                    popover_surface(gpui::div(), &theme, gpui::px(6.0)),
                    colors.surface_secondary,
                    colors.border_secondary,
                ),
                (
                    menu_surface(gpui::div(), &theme, gpui::px(6.0)),
                    colors.surface,
                    colors.border,
                ),
            ] {
                let style = layer.style();
                assert_eq!(style.background, Some(gpui_color(fill).into()));
                assert_eq!(style.border_color, Some(gpui_color(edge)));
                assert_eq!(style.box_shadow, None);
                assert_eq!(
                    style.border_widths.top,
                    Some(gpui::px(theme.dimensions.border_width).into())
                );
            }
        }
    }

    #[test]
    fn input_appearance_follows_platform_aliases_in_both_modes() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let field = input_appearance(&theme);
            assert_eq!(
                field.fill,
                match theme.mode {
                    ThemeMode::Light => theme.colors.surface,
                    ThemeMode::Dark => theme.colors.surface_secondary,
                }
            );
            assert_eq!(field.hover_fill, theme.colors.hover_bg);
            assert_eq!(
                field.border,
                match theme.mode {
                    ThemeMode::Light => theme.colors.border,
                    ThemeMode::Dark => theme.colors.border_secondary,
                }
            );
            assert_eq!(field.focus_ring, theme.colors.ring_primary);
            assert_eq!(field.invalid_border, theme.colors.danger);
            assert_eq!(field.invalid_ring, theme.colors.danger_ring);
        }
    }

    #[test]
    fn base_semantics_are_projected_from_aeris_tokens() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let base = base_theme(&theme);
            assert_eq!(
                base.tokens.colors.foreground,
                gpui_color(theme.colors.text_default)
            );
            assert_eq!(base.tokens.colors.accent, gpui_color(theme.colors.primary));
            assert_eq!(
                base.tokens.colors.ring,
                gpui_color(theme.colors.ring_primary)
            );
            assert_eq!(
                base.tokens.radius.md,
                gpui::px(f32::from(
                    aeris_design_system::RadiusToken::Default.logical_pixels()
                ))
            );
            assert_eq!(
                base.tokens.typography.sans.as_ref(),
                aeris_design_system::platform_font_family()
            );
        }
    }
}
