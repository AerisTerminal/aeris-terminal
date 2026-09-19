use crate::{EngineError, MarketStream, ProviderGeneration, StreamRequirements};
use std::{collections::BTreeMap, time::Duration};
use tradingplot_market_data::MAXIMUM_MARKET_DATA_FIELD_BYTES;

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
    pub streams: StreamRequirements,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderConfig {
    pub account_id: String,
    pub capabilities: ProviderCapabilities,
    pub reconnect_delay: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderRequest {
    HistoricalBars,
    RealtimeBars,
    Trades,
    Quotes,
    Depth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderStatus {
    pub generation: Option<ProviderGeneration>,
    pub health: ProviderHealth,
    pub capabilities: ProviderCapabilities,
}

struct ProviderRecord {
    config: ProviderConfig,
    status: ProviderStatus,
    active: bool,
}

pub(crate) struct ProviderManager {
    providers: BTreeMap<String, ProviderRecord>,
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
        config: ProviderConfig,
    ) -> Result<(), EngineError> {
        validate_provider(&provider)?;
        validate_provider(&config.account_id)?;
        if config.reconnect_delay.is_zero() {
            return Err(EngineError::InvalidProviderReconnectPolicy);
        }
        if self.providers.contains_key(&provider) {
            return Err(EngineError::DuplicateProvider(provider));
        }
        self.providers.insert(
            provider,
            ProviderRecord {
                status: ProviderStatus {
                    generation: None,
                    health: ProviderHealth::Disconnected,
                    capabilities: config.capabilities,
                },
                active: false,
                config,
            },
        );
        Ok(())
    }

    pub(crate) fn begin_session(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        let record = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        if record
            .status
            .generation
            .is_some_and(|current| generation <= current)
        {
            return Err(EngineError::StaleProviderGeneration {
                current: record.status.generation,
                received: generation,
            });
        }
        record.status.generation = Some(generation);
        record.status.health = ProviderHealth::Connecting;
        record.active = true;
        Ok(())
    }

    pub(crate) fn end_session(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        self.verify_generation(provider, generation)?;
        let record = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        record.active = false;
        record.status.health = ProviderHealth::Disconnected;
        Ok(())
    }

    pub(crate) fn set_health(
        &mut self,
        provider: &str,
        generation: ProviderGeneration,
        health: ProviderHealth,
    ) -> Result<(), EngineError> {
        self.verify_generation(provider, generation)?;
        let record = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        if !record.active {
            return Err(EngineError::ProviderSessionUnavailable(
                provider.to_string(),
            ));
        }
        record.status.health = health;
        Ok(())
    }

    pub(crate) fn verify_generation(
        &self,
        provider: &str,
        generation: ProviderGeneration,
    ) -> Result<(), EngineError> {
        let record = self
            .providers
            .get(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        if record.status.generation != Some(generation) {
            return Err(EngineError::StaleProviderGeneration {
                current: record.status.generation,
                received: generation,
            });
        }
        Ok(())
    }

    pub(crate) fn verify_request(
        &self,
        provider: &str,
        request: ProviderRequest,
    ) -> Result<(), EngineError> {
        let record = self
            .providers
            .get(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        if !record.active || record.status.generation.is_none() {
            return Err(EngineError::ProviderSessionUnavailable(
                provider.to_string(),
            ));
        }
        if !supports(record.config.capabilities, request) {
            return Err(EngineError::UnsupportedProviderRequest {
                provider: provider.to_string(),
                request,
            });
        }
        Ok(())
    }

    pub(crate) fn verify_streams(
        &self,
        provider: &str,
        streams: StreamRequirements,
    ) -> Result<(), EngineError> {
        let record = self
            .providers
            .get(provider)
            .ok_or_else(|| EngineError::UnknownProvider(provider.to_string()))?;
        for (required, request) in [
            (
                streams.contains(MarketStream::Bars),
                ProviderRequest::HistoricalBars,
            ),
            (
                streams.contains(MarketStream::Trades),
                ProviderRequest::Trades,
            ),
            (
                streams.contains(MarketStream::Quotes),
                ProviderRequest::Quotes,
            ),
            (
                streams.contains(MarketStream::Depth),
                ProviderRequest::Depth,
            ),
        ] {
            if required && !supports(record.config.capabilities, request) {
                return Err(EngineError::UnsupportedProviderRequest {
                    provider: provider.to_string(),
                    request,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn status(&self, provider: &str) -> Option<ProviderStatus> {
        self.providers.get(provider).map(|record| record.status)
    }

    pub(crate) fn account_id(&self, provider: &str) -> Option<&str> {
        self.providers
            .get(provider)
            .map(|record| record.config.account_id.as_str())
    }

    pub(crate) fn reconnect_delay(&self, provider: &str) -> Option<Duration> {
        self.providers
            .get(provider)
            .map(|record| record.config.reconnect_delay)
    }

    pub(crate) fn len(&self) -> usize {
        self.providers.len()
    }
}

const fn supports(capabilities: ProviderCapabilities, request: ProviderRequest) -> bool {
    match request {
        ProviderRequest::HistoricalBars => capabilities.historical_bars,
        ProviderRequest::RealtimeBars => capabilities.realtime_bars,
        ProviderRequest::Trades => capabilities.streams.contains(MarketStream::Trades),
        ProviderRequest::Quotes => capabilities.streams.contains(MarketStream::Quotes),
        ProviderRequest::Depth => capabilities.streams.contains(MarketStream::Depth),
    }
}

fn validate_provider(provider: &str) -> Result<(), EngineError> {
    if provider.trim().is_empty() || provider.len() > MAXIMUM_MARKET_DATA_FIELD_BYTES {
        return Err(EngineError::InvalidProviderIdentity);
    }
    Ok(())
}
