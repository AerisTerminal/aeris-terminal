use gpui::{FontWeight, Hsla, Pixels, px};
use gpui_base::{ColorTokens, RadiusTokens, Theme, ThemeAppearance};
use tradingplot_design_system::{
    RadiusToken, ThemeColor, ThemeMode, TradingPlotTheme, TypographyRole, platform_font_family,
    platform_typography,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ButtonVariant {
    Filled,
    Secondary,
    Ghost,
    Destructive,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ButtonAppearance {
    pub(crate) fill: ThemeColor,
    pub(crate) foreground: ThemeColor,
    pub(crate) border: Option<ThemeColor>,
    pub(crate) hover: ThemeColor,
    pub(crate) active: ThemeColor,
}

pub(crate) fn button_appearance(
    theme: &TradingPlotTheme,
    variant: ButtonVariant,
) -> ButtonAppearance {
    let colors = theme.colors;
    let (fill, foreground, border) = match variant {
        ButtonVariant::Filled => (colors.button_fill, colors.surface, None),
        ButtonVariant::Secondary => (
            colors.surface_secondary,
            colors.text_primary,
            Some(colors.border_secondary),
        ),
        ButtonVariant::Ghost => (colors.surface, colors.text_secondary, None),
        ButtonVariant::Destructive => (colors.danger, colors.danger_foreground, None),
    };
    ButtonAppearance {
        fill,
        foreground,
        border,
        hover: colors.hover_bg.over(fill),
        active: colors.active_bg.over(fill),
    }
}

pub(crate) fn input_appearance(theme: &TradingPlotTheme) -> (ThemeColor, ThemeColor, ThemeColor) {
    (
        theme.colors.input_fill,
        theme.colors.input_border,
        theme.colors.ring,
    )
}

pub(crate) fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}

pub(crate) fn platform_border_width(theme: &TradingPlotTheme) -> Pixels {
    px(theme.dimensions.border_width)
}

/// Projects `TradingPlot`'s semantic design contract into the unstyled Base
/// foundation. Base behavior modules can then resolve unset semantics without
/// introducing a second palette or typography source.
pub(crate) fn base_theme(theme: &TradingPlotTheme) -> Theme {
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
        input: gpui_color(colors.input_border),
        ring: gpui_color(colors.ring),
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
    use tradingplot_design_system::TradingPlotTheme;

    use super::{ButtonVariant, base_theme, button_appearance, gpui_color, input_appearance};

    #[test]
    fn semantic_appearances_follow_platform_aliases_in_both_modes() {
        for theme in [TradingPlotTheme::light(), TradingPlotTheme::dark()] {
            let filled = button_appearance(&theme, ButtonVariant::Filled);
            assert_eq!(filled.fill, theme.colors.button_fill);
            assert_eq!(filled.foreground, theme.colors.surface);

            let secondary = button_appearance(&theme, ButtonVariant::Secondary);
            assert_eq!(secondary.fill, theme.colors.surface_secondary);
            assert_eq!(secondary.border, Some(theme.colors.border_secondary));

            let destructive = button_appearance(&theme, ButtonVariant::Destructive);
            assert_eq!(destructive.fill, theme.colors.danger);
            assert_eq!(destructive.foreground, theme.colors.danger_foreground);

            let (input_fill, input_border, focus) = input_appearance(&theme);
            assert_eq!(input_fill, secondary.fill);
            assert_eq!(input_border, secondary.border.unwrap());
            assert_eq!(focus, theme.colors.ring);
        }
    }

    #[test]
    fn base_semantics_are_projected_from_tradingplot_tokens() {
        for theme in [TradingPlotTheme::light(), TradingPlotTheme::dark()] {
            let base = base_theme(&theme);
            assert_eq!(
                base.tokens.colors.foreground,
                gpui_color(theme.colors.text_primary)
            );
            assert_eq!(base.tokens.colors.accent, gpui_color(theme.colors.primary));
            assert_eq!(base.tokens.colors.ring, gpui_color(theme.colors.ring));
            assert_eq!(
                base.tokens.radius.md,
                gpui::px(f32::from(
                    tradingplot_design_system::RadiusToken::Default.logical_pixels()
                ))
            );
            assert_eq!(
                base.tokens.typography.sans.as_ref(),
                tradingplot_design_system::platform_font_family()
            );
        }
    }
}
