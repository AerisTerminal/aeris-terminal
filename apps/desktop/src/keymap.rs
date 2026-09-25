use std::collections::{BTreeMap, BTreeSet};

/// Product actions registered by the one desktop keymap owner.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KeymapAction {
    BuyMarket,
    SellMarket,
    CancelAll,
    FlattenAccount,
    KillSwitch,
}

impl KeymapAction {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::BuyMarket => "buy_market",
            Self::SellMarket => "sell_market",
            Self::CancelAll => "cancel_all",
            Self::FlattenAccount => "flatten_account",
            Self::KillSwitch => "kill_switch",
        }
    }
}

/// Normalized keyboard chord used for conflict detection before GPUI binding.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct KeyChord(String);

impl KeyChord {
    /// Parses and canonicalizes a keyboard chord.
    ///
    /// # Errors
    ///
    /// Returns an error when the key or a modifier is malformed or repeated.
    pub fn parse(value: &str) -> Result<Self, String> {
        let normalized = value.trim().to_ascii_lowercase();
        let parts = normalized.split('-').collect::<Vec<_>>();
        let Some(key) = parts.last().copied() else {
            return Err("key chord is empty".to_string());
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return Err("key chord key is invalid".to_string());
        }
        let mut modifiers = BTreeSet::new();
        for modifier in &parts[..parts.len().saturating_sub(1)] {
            if !matches!(*modifier, "alt" | "ctrl" | "shift" | "cmd" | "win") {
                return Err("key chord modifier is invalid".to_string());
            }
            if !modifiers.insert(*modifier) {
                return Err("key chord repeats a modifier".to_string());
            }
        }
        let mut canonical = modifiers.into_iter().collect::<Vec<_>>();
        canonical.push(key);
        Ok(Self(canonical.join("-")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The single owner of trading shortcuts and their conflict policy.
#[derive(Clone, Debug, Default)]
pub struct KeymapOwner {
    bindings: BTreeMap<KeyChord, KeymapAction>,
}

impl KeymapOwner {
    /// Builds the product defaults after checking them against reserved desktop shortcuts.
    ///
    /// # Errors
    ///
    /// Returns an error if a default binding is malformed, duplicated, or reserved.
    pub fn defaults() -> Result<Self, String> {
        let mut owner = Self::default();
        for (chord, action) in [
            ("ctrl-b", KeymapAction::BuyMarket),
            ("ctrl-s", KeymapAction::SellMarket),
            ("ctrl-shift-x", KeymapAction::CancelAll),
            ("ctrl-shift-f", KeymapAction::FlattenAccount),
            ("ctrl-shift-k", KeymapAction::KillSwitch),
        ] {
            owner.bind(chord, action)?;
        }
        owner.validate_against_reserved(&[
            "f11",
            "alt-enter",
            "alt-f9",
            "alt-f10",
            "alt-f4",
            "ctrl-t",
            "ctrl-tab",
            "ctrl-shift-tab",
            "ctrl-shift-pageup",
            "ctrl-shift-pagedown",
            "ctrl-w",
            "ctrl-alt-h",
            "ctrl-alt-v",
            "ctrl-shift-w",
        ])?;
        Ok(owner)
    }

    /// Adds a trading shortcut to the owner.
    ///
    /// # Errors
    ///
    /// Returns an error if the chord is malformed or already bound.
    pub fn bind(&mut self, chord: &str, action: KeymapAction) -> Result<(), String> {
        let chord = KeyChord::parse(chord)?;
        let canonical = chord.as_str().to_string();
        if self.bindings.insert(chord, action).is_some() {
            return Err(format!(
                "keymap chord is already bound to another action: {canonical}"
            ));
        }
        Ok(())
    }

    /// Rejects bindings that overlap the desktop's reserved shortcuts.
    ///
    /// # Errors
    ///
    /// Returns an error if a reserved chord is malformed or conflicts with a binding.
    pub fn validate_against_reserved(&self, reserved: &[&str]) -> Result<(), String> {
        for value in reserved {
            let chord = KeyChord::parse(value)?;
            if self.bindings.contains_key(&chord) {
                return Err(format!(
                    "trading keymap conflicts with reserved chord {value}"
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn action_for(&self, chord: &str) -> Option<KeymapAction> {
        KeyChord::parse(chord)
            .ok()
            .and_then(|chord| self.bindings.get(&chord).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unique_and_do_not_shadow_desktop_commands() {
        let owner = KeymapOwner::defaults().expect("defaults");
        assert_eq!(
            owner.action_for("ctrl-shift-k"),
            Some(KeymapAction::KillSwitch)
        );
        assert_eq!(
            owner.action_for("CTRL-SHIFT-K"),
            Some(KeymapAction::KillSwitch)
        );
        assert_eq!(owner.action_for("ctrl-t"), None);
    }

    #[test]
    fn duplicate_and_malformed_bindings_fail_closed() {
        let mut owner = KeymapOwner::default();
        owner.bind("ctrl-b", KeymapAction::BuyMarket).expect("bind");
        assert!(owner.bind("ctrl-b", KeymapAction::SellMarket).is_err());
        assert!(owner.bind("ctrl-ctrl-b", KeymapAction::SellMarket).is_err());
        assert!(owner.bind("meta-b", KeymapAction::SellMarket).is_err());
    }
}
