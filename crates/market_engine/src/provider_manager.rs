use crate::{EngineError, ProviderGeneration};
use axiusflow_market_data::MAXIMUM_MARKET_DATA_FIELD_BYTES;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderHealth {
    Disconnected,
    Connecting,
    Online,
    Recovering,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCapabilities {
    pub historical_bars: bool,
    pub realtime_bars: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderStatus {
    pub generation: Option<ProviderGeneration>,
    pub health: ProviderHealth,
    pub capabilities: ProviderCapabilities,
}

pub(crate) struct ProviderManager {
    providers: BTreeMap<String, ProviderStatus>,
}

impl ProviderManager {
    pub(crate) fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        provider: String,
        capabilities: ProviderCapabilities,
    ) -> Result<(), EngineError> {
        validate_provider(&provider)?;
        if self.providers.contains_key(&provider) {
            return Err(EngineError::DuplicateProvider(provider));
        }
        self.providers.insert(
            provider,
            ProviderStatus {
                generation: None,
                health: ProviderHealth::Disconnected,
                capabilities,
            },
        );
        Ok(())
    }

    pub(crate) fn begin_session(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        let status = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        if status
            .generation
            .is_some_and(|current| generation <= current)
        {
            return Err(EngineError::StaleProviderGeneration {
                current: status.generation,
                received: generation,
            });
        }
        status.generation = Some(generation);
        status.health = ProviderHealth::Connecting;
        Ok(())
    }

    pub(crate) fn set_health(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
        health: ProviderHealth,
    ) -> Result<(), EngineError> {
        self.verify_generation(provider, generation)?;
        let status = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        status.health = health;
        Ok(())
    }

    pub(crate) fn verify_generation(
        &self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        let status = self
            .providers
            .get(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        if status.generation != Some(generation) {
            return Err(EngineError::StaleProviderGeneration {
                current: status.generation,
                received: generation,
            });
        }
        Ok(())
    }

    pub(crate) fn status(&self, provider: &str) -> Option<ProviderStatus> {
        self.providers.get(provider).copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.providers.len()
    }
}

fn validate_provider(provider: &str) -> Result<(), EngineError> {
    if provider.trim().is_empty() || provider.len() > MAXIMUM_MARKET_DATA_FIELD_BYTES {
        return Err(EngineError::InvalidProviderIdentity);
    }
    Ok(())
}
