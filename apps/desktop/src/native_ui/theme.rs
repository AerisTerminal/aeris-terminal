use axiusflow_design_system::{AxiusflowTheme, ThemeColor};
use gpui::{Hsla, Pixels, px};

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
    theme: &AxiusflowTheme,
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

pub(crate) fn input_appearance(theme: &AxiusflowTheme) -> (ThemeColor, ThemeColor, ThemeColor) {
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

pub(crate) fn platform_border_width(theme: &AxiusflowTheme) -> Pixels {
    px(theme.dimensions.border_width)
}

#[cfg(test)]
mod tests {
    use axiusflow_design_system::AxiusflowTheme;

    use super::{ButtonVariant, button_appearance, input_appearance};

    #[test]
    fn semantic_appearances_follow_platform_aliases_in_both_modes() {
        for theme in [AxiusflowTheme::light(), AxiusflowTheme::dark()] {
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
}
