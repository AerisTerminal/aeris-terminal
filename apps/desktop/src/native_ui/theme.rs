use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, ThemeMode, TypographyRole, platform_font_family,
    platform_typography,
};
use gpui::{FontWeight, Hsla, Pixels, px};
use gpui_base::{ColorTokens, RadiusTokens, Theme, ThemeAppearance};

/// Text-field tokens, exactly as the Theme System `Input` uses them: a `border` field on
/// `surface` (`surface-secondary` in dark mode) that takes `hover-bg` on hover, the shared 2px
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
        fill: match theme.mode {
            ThemeMode::Light => colors.surface,
            ThemeMode::Dark => colors.surface_secondary,
        },
        hover_fill: colors.hover_bg,
        border: colors.border,
        focus_ring: colors.ring_primary,
        invalid_border: colors.danger,
        invalid_ring: colors.danger_ring,
    }
}

pub(crate) fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}

pub(crate) fn platform_border_width(theme: &AerisTheme) -> Pixels {
    px(theme.dimensions.border_width)
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
        foreground: gpui_color(colors.text_primary),
        surface: gpui_color(colors.surface),
        surface_foreground: gpui_color(colors.text_primary),
        primary: gpui_color(colors.primary),
        primary_foreground: gpui_color(colors.primary_foreground),
        secondary: gpui_color(colors.surface_secondary),
        secondary_foreground: gpui_color(colors.text_primary),
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

    use super::{base_theme, gpui_color, input_appearance};

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
            assert_eq!(field.border, theme.colors.border);
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
                gpui_color(theme.colors.text_primary)
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
