use gpui::Hsla;

use crate::SemanticThemeTokens;

/// Application-owned colors consumed by input painting.
#[derive(Clone, Default)]
pub struct InputEditorStyle {
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub selection: Hsla,
    pub caret: Hsla,
}

impl InputEditorStyle {
    /// Fills in every colour that was left unset, from the active palette.
    ///
    /// `Hsla::default()` is fully transparent, and every colour on `Default` is
    /// that — so an input nothing projected onto painted its glyphs, its caret
    /// and its selection in nothing at all. Transparent is not a colour anyone
    /// means for ink, which is what makes it usable as "unset" here.
    ///
    /// This is resolution, not assignment: whatever a consumer did project is
    /// kept exactly, so a consumer that projects once at construction gets the
    /// palette that is current now rather than the one that happened to be
    /// installed when the state was built.
    pub fn resolved(&self, tokens: &SemanticThemeTokens) -> Self {
        let colors = &tokens.colors;
        let unset = |value: Hsla| value.a == 0.;
        let or = |value: Hsla, fallback: Hsla| if unset(value) { fallback } else { value };

        let foreground = or(self.foreground, colors.foreground);
        let mut selection = self.selection;
        if unset(selection) {
            selection = colors.accent;
            // A selection must not hide the glyphs it selects.
            selection.a = 0.4;
        }

        Self {
            foreground,
            muted_foreground: or(self.muted_foreground, colors.muted_foreground),
            selection,
            caret: or(self.caret, foreground),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::hsla;

    use super::InputEditorStyle;
    use crate::SemanticThemeTokens;

    fn dark() -> SemanticThemeTokens {
        let mut tokens = SemanticThemeTokens::default();
        tokens.colors.foreground = hsla(0., 0., 0.98, 1.0);
        tokens.colors.muted_foreground = hsla(0., 0., 0.64, 1.0);
        tokens.colors.accent = hsla(0.6, 0.5, 0.5, 1.0);
        tokens
    }

    #[test]
    fn an_unprojected_style_takes_its_ink_from_the_palette() {
        let tokens = dark();
        let resolved = InputEditorStyle::default().resolved(&tokens);

        assert_eq!(resolved.foreground, tokens.colors.foreground);
        assert_eq!(resolved.caret, tokens.colors.foreground);
        assert_eq!(resolved.muted_foreground, tokens.colors.muted_foreground);
        // The point of the change: every one of these was transparent, so an
        // input nothing projected onto painted its text in nothing at all.
        for colour in [
            resolved.foreground,
            resolved.caret,
            resolved.muted_foreground,
            resolved.selection,
        ] {
            assert!(colour.a > 0., "{colour:?} is still invisible");
        }
    }

    #[test]
    fn a_selection_stays_translucent_enough_to_read_through() {
        let resolved = InputEditorStyle::default().resolved(&dark());
        assert_eq!(resolved.selection.a, 0.4);
    }

    #[test]
    fn projected_colours_are_kept_verbatim() {
        let chosen = hsla(0.3, 0.4, 0.5, 1.0);
        let style = InputEditorStyle {
            foreground: chosen,
            caret: chosen,
            ..Default::default()
        };
        let resolved = style.resolved(&dark());

        assert_eq!(resolved.foreground, chosen);
        assert_eq!(resolved.caret, chosen);
        // And what was not projected still comes from the palette.
        assert_eq!(resolved.muted_foreground, dark().colors.muted_foreground);
    }

    #[test]
    fn resolution_never_consumes_its_own_output() {
        // The projected style is kept verbatim precisely so that this holds:
        // resolving against a second palette must follow it, not stay on the
        // first. Resolving in place would have frozen after one pass.
        let projected = InputEditorStyle::default();
        let first = projected.resolved(&dark());

        let mut light = SemanticThemeTokens::default();
        light.colors.foreground = hsla(0., 0., 0.04, 1.0);
        let second = projected.resolved(&light);

        assert_ne!(first.foreground, second.foreground);
        assert_eq!(second.foreground, light.colors.foreground);
    }
}
