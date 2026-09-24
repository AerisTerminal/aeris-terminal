//! Product-owned trust boundary for statically linked native Study SDK packages.
//!
//! Arbitrary native libraries are never discovered or loaded from disk. A reviewed study crate
//! becomes trusted only when the signed desktop build links it and adds its descriptor to this
//! bounded list.

use aeris_study_sdk::{TrustedStudyPackage, TrustedStudyRegistry, TrustedStudyRegistryError};

/// Build-time allowlist for reviewed external native study packages.
///
/// Built-in studies do not appear here; the SDK registry resolves them through its built-in
/// fallback. Product builds that approve an external package add its descriptor here together
/// with the corresponding Cargo dependency.
const TRUSTED_NATIVE_STUDY_PACKAGES: &[TrustedStudyPackage] = &[];

pub(crate) fn product_study_registry()
-> Result<TrustedStudyRegistry<'static>, TrustedStudyRegistryError> {
    TrustedStudyRegistry::from_packages(TRUSTED_NATIVE_STUDY_PACKAGES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipping_product_registry_is_static_bounded_and_valid() {
        let registry = product_study_registry().expect("shipping study package allowlist is valid");
        assert!(registry.is_empty());
    }
}
