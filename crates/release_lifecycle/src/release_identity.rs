//! Immutable identity shared by desktop and engine binaries from one release.

/// Exact identity embedded by release packaging into one desktop release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseIdentity {
    pub release_identity: String,
    pub install_generation: u64,
}

/// Returns the release identity compiled into the current binary.
///
/// Development builds use the workspace version and generation zero. Release
/// builds use the identity and generation of their embedded release
/// configuration, which packaging sets for every binary in one bundle.
#[must_use]
pub fn current_release_identity() -> ReleaseIdentity {
    match crate::release_config() {
        Some(config) => ReleaseIdentity {
            release_identity: config.release_identity.to_string(),
            install_generation: config.install_generation,
        },
        None => ReleaseIdentity {
            release_identity: env!("CARGO_PKG_VERSION").to_string(),
            install_generation: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::current_release_identity;

    #[test]
    fn build_identity_is_nonempty_and_bounded() {
        let identity = current_release_identity();
        assert_ne!(identity.release_identity, "");
        assert!(identity.release_identity.len() <= 128);
    }
}
