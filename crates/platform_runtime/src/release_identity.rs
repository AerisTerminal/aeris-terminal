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
/// packaging sets both compile-time variables for every binary in one bundle.
#[must_use]
pub fn current_release_identity() -> ReleaseIdentity {
    ReleaseIdentity {
        release_identity: option_env!("AXIUSFLOW_RELEASE_IDENTITY")
            .unwrap_or(env!("CARGO_PKG_VERSION"))
            .to_string(),
        install_generation: option_env!("AXIUSFLOW_INSTALL_GENERATION")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::current_release_identity;

    #[test]
    fn build_identity_is_nonempty_and_bounded() {
        let identity = current_release_identity();
        assert!(!identity.release_identity.is_empty());
        assert!(identity.release_identity.len() <= 128);
    }
}
