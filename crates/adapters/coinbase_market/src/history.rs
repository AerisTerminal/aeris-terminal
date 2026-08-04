use axiusflow_provider_history::{DatasetCapability, HistoryCapabilities};

/// Fail-closed capability adapter pending a desktop-local Coinbase fetch path.
pub struct CoinbaseHistoryCapabilityAdapter {
    capabilities: HistoryCapabilities,
}

impl CoinbaseHistoryCapabilityAdapter {
    /// Builds the implemented bars/ticks/depth matrix.
    ///
    /// # Errors
    ///
    /// Returns an error if the static capability profile violates shared bounds.
    pub fn try_new() -> Result<Self, axiusflow_provider_history::ProviderHistoryError> {
        let capabilities = HistoryCapabilities::try_new(
            "coinbase".to_string(),
            DatasetCapability::unsupported("desktop-local bar history fetch is not implemented"),
            DatasetCapability::unsupported("historical ticks are not implemented"),
            DatasetCapability::unsupported("historical depth is not implemented"),
        )?;
        Ok(Self { capabilities })
    }

    #[must_use]
    pub const fn capabilities(&self) -> &HistoryCapabilities {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::CoinbaseHistoryCapabilityAdapter;
    use axiusflow_provider_history::{DataClass, DatasetCapability};

    #[test]
    fn profile_rejects_every_history_lane_without_a_fetch_adapter() {
        let profile = CoinbaseHistoryCapabilityAdapter::try_new().expect("profile is valid");
        assert_eq!(profile.capabilities().provider_id(), "coinbase");
        for data_class in [DataClass::Bars, DataClass::Ticks, DataClass::Depth] {
            assert!(matches!(
                profile.capabilities().dataset(data_class),
                DatasetCapability::Unsupported { .. }
            ));
        }
    }
}
