use crate::provider_runtime::{ConnectTrigger, RecoveryReason, SessionGeneration};
use aeris_market_data::{BarSeriesKey, MarketEvent};
use core::fmt;
use std::{collections::BTreeSet, error::Error, num::NonZeroUsize};

/// Maximum bytes accepted in one provider discovery identity field.
pub const MAXIMUM_DISCOVERY_FIELD_BYTES: usize = 256;

/// Validation failures for provider-neutral session contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderContractError {
    EmptyField(&'static str),
    ControlCharacter(&'static str),
    FieldTooLong { field: &'static str, maximum: usize },
    ScaleOutOfRange,
    InvalidPriceIncrement,
    SystemLimitExceeded { maximum: usize },
    InstrumentLimitExceeded { maximum: usize },
    SeriesLimitExceeded { maximum: usize },
    InvalidBarSeries,
    SubscriptionIdentityMismatch,
    InvalidTimestamp,
    GenerationMismatch,
    InvalidMarketEvent,
}

impl fmt::Display for ProviderContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "provider session contract failed: {self:?}")
    }
}

impl Error for ProviderContractError {}

/// Provider environment selected for one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderEnvironment {
    pub provider_id: String,
    pub system_id: String,
    pub environment: String,
}

impl ProviderEnvironment {
    /// Validates the non-secret provider environment identity.
    ///
    /// # Errors
    ///
    /// Returns an error when any required identity is empty or oversized.
    pub fn validate(&self) -> Result<(), ProviderContractError> {
        validate_discovery_field("provider_id", &self.provider_id)?;
        validate_discovery_field("system_id", &self.system_id)?;
        validate_discovery_field("environment", &self.environment)?;
        Ok(())
    }
}

/// Coarse authentication state without provider text or account details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationState {
    Required,
    Accepted,
    Rejected,
    AgreementRequired,
}

/// Bounded provider-neutral instrument discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentDescriptor {
    pub instrument_id: String,
    pub provider_symbol: String,
    pub display_symbol: String,
    pub venue_id: String,
    pub price_scale: u8,
    pub quantity_scale: u8,
    /// Authoritative minimum price increment in the same fixed-point units as
    /// canonical prices. `None` means the provider did not supply a safely
    /// representable trading increment; decimal scale alone is not a tick.
    pub price_increment: Option<i64>,
}

impl InstrumentDescriptor {
    /// Validates discovery identity and fixed-point scales.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized identity, or scale above 18.
    pub fn validate(&self) -> Result<(), ProviderContractError> {
        validate_discovery_field("instrument_id", &self.instrument_id)?;
        validate_discovery_field("provider_symbol", &self.provider_symbol)?;
        validate_discovery_field("display_symbol", &self.display_symbol)?;
        validate_discovery_field("venue_id", &self.venue_id)?;
        if self.price_scale > 18 || self.quantity_scale > 18 {
            return Err(ProviderContractError::ScaleOutOfRange);
        }
        if self.price_increment.is_some_and(|increment| increment <= 0) {
            return Err(ProviderContractError::InvalidPriceIncrement);
        }
        Ok(())
    }
}

/// Coarse invalidation reasons shared by every provider adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderInvalidationReason {
    Transport,
    Authentication,
    AgreementRequired,
    UnsupportedSystem,
    SchemaMismatch,
    HeartbeatSilence,
    MessageSilence,
    SequenceGap,
    QueueOverflow,
    MalformedMessage,
}

/// Provider-neutral session output. Raw frames and provider payloads are excluded.
#[derive(Clone, Eq, PartialEq)]
pub enum ProviderSessionEvent {
    DiscoveryStarted,
    SystemsDiscovered {
        environments: Vec<ProviderEnvironment>,
    },
    AuthenticationChanged {
        generation: SessionGeneration,
        state: AuthenticationState,
    },
    InstrumentsDiscovered {
        generation: SessionGeneration,
        instruments: Vec<InstrumentDescriptor>,
    },
    Market {
        generation: SessionGeneration,
        event: MarketEvent,
    },
    Heartbeat {
        generation: SessionGeneration,
        received_unix_nanos: i64,
        transport_rtt_nanos: Option<u64>,
    },
    Invalidated {
        generation: Option<SessionGeneration>,
        reason: ProviderInvalidationReason,
    },
    Stopped,
}

impl fmt::Debug for ProviderSessionEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiscoveryStarted => formatter.write_str("DiscoveryStarted"),
            Self::SystemsDiscovered { .. } => formatter
                .debug_struct("SystemsDiscovered")
                .field("environments", &"[REDACTED]")
                .finish(),
            Self::AuthenticationChanged { generation, state } => formatter
                .debug_struct("AuthenticationChanged")
                .field("generation", generation)
                .field("state", state)
                .finish(),
            Self::InstrumentsDiscovered { generation, .. } => formatter
                .debug_struct("InstrumentsDiscovered")
                .field("generation", generation)
                .field("instruments", &"[REDACTED]")
                .finish(),
            Self::Market { generation, .. } => formatter
                .debug_struct("Market")
                .field("generation", generation)
                .field("event", &"[REDACTED]")
                .finish(),
            Self::Heartbeat {
                generation,
                received_unix_nanos,
                transport_rtt_nanos,
            } => formatter
                .debug_struct("Heartbeat")
                .field("generation", generation)
                .field("received_unix_nanos", received_unix_nanos)
                .field("transport_rtt_nanos", transport_rtt_nanos)
                .finish(),
            Self::Invalidated { generation, reason } => formatter
                .debug_struct("Invalidated")
                .field("generation", generation)
                .field("reason", reason)
                .finish(),
            Self::Stopped => formatter.write_str("Stopped"),
        }
    }
}

impl ProviderSessionEvent {
    /// Validates event bounds and generation consistency.
    ///
    /// # Errors
    ///
    /// Returns an error when discovery output exceeds its bound, semantic data
    /// is invalid, or nested generation evidence disagrees.
    pub fn validate(
        &self,
        maximum_systems: NonZeroUsize,
        maximum_instruments: NonZeroUsize,
        maximum_depth_levels: usize,
    ) -> Result<(), ProviderContractError> {
        match self {
            Self::DiscoveryStarted
            | Self::AuthenticationChanged { .. }
            | Self::Invalidated { .. }
            | Self::Stopped => Ok(()),
            Self::SystemsDiscovered { environments } => {
                if environments.len() > maximum_systems.get() {
                    return Err(ProviderContractError::SystemLimitExceeded {
                        maximum: maximum_systems.get(),
                    });
                }
                environments
                    .iter()
                    .try_for_each(ProviderEnvironment::validate)
            }
            Self::InstrumentsDiscovered { instruments, .. } => {
                if instruments.len() > maximum_instruments.get() {
                    return Err(ProviderContractError::InstrumentLimitExceeded {
                        maximum: maximum_instruments.get(),
                    });
                }
                instruments
                    .iter()
                    .try_for_each(InstrumentDescriptor::validate)
            }
            Self::Market { generation, event } => {
                if event.metadata().session_generation != generation.get() {
                    return Err(ProviderContractError::GenerationMismatch);
                }
                event
                    .validate(maximum_depth_levels)
                    .map_err(|_| ProviderContractError::InvalidMarketEvent)
            }
            Self::Heartbeat {
                received_unix_nanos,
                ..
            } => {
                if *received_unix_nanos <= 0 {
                    return Err(ProviderContractError::InvalidTimestamp);
                }
                Ok(())
            }
        }
    }
}

/// Complete replacement intent for read-only market-data subscriptions.
#[derive(Clone, Eq, PartialEq)]
pub struct ProviderSubscription {
    instruments: BTreeSet<String>,
    bar_series: BTreeSet<BarSeriesKey>,
    depth_instruments: BTreeSet<String>,
}

impl fmt::Debug for ProviderSubscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSubscription")
            .field("instrument_count", &self.instruments.len())
            .field("bar_series_count", &self.bar_series.len())
            .field("depth_instrument_count", &self.depth_instruments.len())
            .finish()
    }
}

impl ProviderSubscription {
    /// Creates a bounded, deduplicated subscription replacement.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, series, or configured bounds.
    pub fn try_new(
        instruments: impl IntoIterator<Item = String>,
        bar_series: impl IntoIterator<Item = BarSeriesKey>,
        depth_instruments: impl IntoIterator<Item = String>,
        maximum_instruments: NonZeroUsize,
        maximum_series: NonZeroUsize,
    ) -> Result<Self, ProviderContractError> {
        let instruments = collect_bounded(
            instruments,
            maximum_instruments,
            ProviderContractError::InstrumentLimitExceeded {
                maximum: maximum_instruments.get(),
            },
            |value| validate_discovery_field("instrument_id", value),
        )?;
        let bar_series = collect_bounded(
            bar_series,
            maximum_series,
            ProviderContractError::SeriesLimitExceeded {
                maximum: maximum_series.get(),
            },
            validate_subscription_series,
        )?;
        let depth_instruments = collect_bounded(
            depth_instruments,
            maximum_instruments,
            ProviderContractError::InstrumentLimitExceeded {
                maximum: maximum_instruments.get(),
            },
            |value| validate_discovery_field("instrument_id", value),
        )?;
        if depth_instruments
            .iter()
            .any(|instrument| !instruments.contains(instrument))
            || bar_series
                .iter()
                .any(|series| !instruments.contains(&series.instrument_id))
        {
            return Err(ProviderContractError::SubscriptionIdentityMismatch);
        }
        Ok(Self {
            instruments,
            bar_series,
            depth_instruments,
        })
    }

    /// Selected instrument identities.
    #[must_use]
    pub const fn instruments(&self) -> &BTreeSet<String> {
        &self.instruments
    }

    /// Selected canonical bar series.
    #[must_use]
    pub const fn bar_series(&self) -> &BTreeSet<BarSeriesKey> {
        &self.bar_series
    }

    /// Selected instruments requiring read-only depth.
    #[must_use]
    pub const fn depth_instruments(&self) -> &BTreeSet<String> {
        &self.depth_instruments
    }
}

fn collect_bounded<T: Ord>(
    values: impl IntoIterator<Item = T>,
    maximum: NonZeroUsize,
    error: ProviderContractError,
    validate: impl Fn(&T) -> Result<(), ProviderContractError>,
) -> Result<BTreeSet<T>, ProviderContractError> {
    let mut collected = BTreeSet::new();
    for (index, value) in values.into_iter().enumerate() {
        if index >= maximum.get() {
            return Err(error);
        }
        validate(&value)?;
        collected.insert(value);
    }
    Ok(collected)
}

fn validate_subscription_series(key: &BarSeriesKey) -> Result<(), ProviderContractError> {
    validate_discovery_field("provider_id", &key.provider_id)?;
    validate_discovery_field("instrument_id", &key.instrument_id)?;
    validate_discovery_field("entitlement_id", &key.entitlement_id)?;
    key.validate()
        .map_err(|_| ProviderContractError::InvalidBarSeries)
}

fn validate_discovery_field(field: &'static str, value: &str) -> Result<(), ProviderContractError> {
    if value.trim().is_empty() {
        return Err(ProviderContractError::EmptyField(field));
    }
    if value.len() > MAXIMUM_DISCOVERY_FIELD_BYTES {
        return Err(ProviderContractError::FieldTooLong {
            field,
            maximum: MAXIMUM_DISCOVERY_FIELD_BYTES,
        });
    }
    Ok(())
}

/// Sealed application commands: no arbitrary payload or provider template can be sent.
#[derive(Clone, Eq, PartialEq)]
pub enum ProviderSessionCommand {
    Connect {
        trigger: ConnectTrigger,
        environment: ProviderEnvironment,
    },
    ReplaceSubscriptions(ProviderSubscription),
    RequestRecovery {
        reason: RecoveryReason,
    },
    Disconnect,
    Shutdown,
}

impl fmt::Debug for ProviderSessionCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { trigger, .. } => formatter
                .debug_struct("Connect")
                .field("trigger", trigger)
                .field("environment", &"[REDACTED]")
                .finish(),
            Self::ReplaceSubscriptions(_) => formatter
                .debug_tuple("ReplaceSubscriptions")
                .field(&"[REDACTED]")
                .finish(),
            Self::RequestRecovery { reason } => formatter
                .debug_struct("RequestRecovery")
                .field("reason", reason)
                .finish(),
            Self::Disconnect => formatter.write_str("Disconnect"),
            Self::Shutdown => formatter.write_str("Shutdown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_data::{
        AggressorSide, BarPeriod, BarSeriesKey, EventMetadata, MarketTrade, QualifiedTimestamp,
    };
    use std::{cell::Cell, num::NonZeroU64};

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
    }

    #[test]
    fn subscription_replacement_is_bounded_deduplicated_and_closed_over_identity() {
        let key = BarSeriesKey {
            provider_id: "fixture".to_string(),
            instrument_id: "instrument:fixture:es".to_string(),
            entitlement_id: "test".to_string(),
            period: BarPeriod::time(60).expect("standard period validates"),
            definition_version: 1,
        };
        let subscription = ProviderSubscription::try_new(
            [
                "instrument:fixture:es".to_string(),
                "instrument:fixture:es".to_string(),
            ],
            [key],
            ["instrument:fixture:es".to_string()],
            nonzero(2),
            nonzero(2),
        )
        .expect("bounded subscription validates");
        assert_eq!(subscription.instruments().len(), 1);
        assert_eq!(subscription.bar_series().len(), 1);
        assert_eq!(subscription.depth_instruments().len(), 1);
        let subscription_debug = format!("{subscription:?}");
        assert!(!subscription_debug.contains("instrument:fixture:es"));
        let command_debug = format!(
            "{:?}",
            ProviderSessionCommand::ReplaceSubscriptions(subscription)
        );
        assert!(command_debug.contains("[REDACTED]"));
        assert!(!command_debug.contains("instrument:fixture:es"));
    }

    #[test]
    fn subscription_rejects_series_outside_selected_instruments() {
        let key = BarSeriesKey {
            provider_id: "fixture".to_string(),
            instrument_id: "instrument:fixture:nq".to_string(),
            entitlement_id: "test".to_string(),
            period: BarPeriod::time(60).expect("standard period validates"),
            definition_version: 1,
        };
        assert_eq!(
            ProviderSubscription::try_new(
                ["instrument:fixture:es".to_string()],
                [key],
                [],
                nonzero(2),
                nonzero(2),
            ),
            Err(ProviderContractError::SubscriptionIdentityMismatch)
        );
    }

    #[test]
    fn subscription_collection_stops_after_the_bound_is_exceeded() {
        let consumed = Cell::new(0);
        let instruments = (0..10).map(|index| {
            consumed.set(consumed.get() + 1);
            format!("instrument:fixture:{index}")
        });
        assert_eq!(
            ProviderSubscription::try_new(instruments, [], [], nonzero(2), nonzero(2),),
            Err(ProviderContractError::InstrumentLimitExceeded { maximum: 2 })
        );
        assert_eq!(consumed.get(), 3);
    }

    #[test]
    fn subscription_collection_bounds_duplicate_input() {
        let consumed = Cell::new(0);
        let instruments = (0..).map(|_| {
            consumed.set(consumed.get() + 1);
            "instrument:fixture:es".to_string()
        });
        assert_eq!(
            ProviderSubscription::try_new(instruments, [], [], nonzero(2), nonzero(2)),
            Err(ProviderContractError::InstrumentLimitExceeded { maximum: 2 })
        );
        assert_eq!(consumed.get(), 3);
    }

    #[test]
    fn subscription_identity_fields_have_explicit_byte_bounds() {
        let oversized = "x".repeat(MAXIMUM_DISCOVERY_FIELD_BYTES + 1);
        assert_eq!(
            ProviderSubscription::try_new([oversized.clone()], [], [], nonzero(2), nonzero(2),),
            Err(ProviderContractError::FieldTooLong {
                field: "instrument_id",
                maximum: MAXIMUM_DISCOVERY_FIELD_BYTES,
            })
        );

        let key = BarSeriesKey {
            provider_id: oversized,
            instrument_id: "instrument:fixture:es".to_string(),
            entitlement_id: "test".to_string(),
            period: BarPeriod::time(60).expect("standard period validates"),
            definition_version: 1,
        };
        assert_eq!(
            ProviderSubscription::try_new(
                ["instrument:fixture:es".to_string()],
                [key],
                [],
                nonzero(2),
                nonzero(2),
            ),
            Err(ProviderContractError::FieldTooLong {
                field: "provider_id",
                maximum: MAXIMUM_DISCOVERY_FIELD_BYTES,
            })
        );
    }

    #[test]
    fn command_surface_contains_only_declared_read_only_operations() {
        let commands = [
            ProviderSessionCommand::Disconnect,
            ProviderSessionCommand::Shutdown,
            ProviderSessionCommand::RequestRecovery {
                reason: RecoveryReason::TransportInvalid,
            },
        ];
        assert_eq!(commands.len(), 3);
    }

    #[test]
    fn market_event_generation_must_match_the_session_envelope() {
        let generation = SessionGeneration::new(NonZeroU64::new(4).unwrap_or(NonZeroU64::MIN));
        let event = ProviderSessionEvent::Market {
            generation,
            event: MarketEvent::Trade(MarketTrade {
                metadata: EventMetadata {
                    provider_id: "fixture".to_string(),
                    instrument_id: "instrument:fixture:es".to_string(),
                    entitlement_id: "test".to_string(),
                    source_sequence: 1,
                    session_generation: 3,
                    timestamps: QualifiedTimestamp {
                        exchange_unix_nanos: Some(10),
                        provider_unix_nanos: Some(11),
                        received_unix_nanos: 12,
                    },
                },
                trade_id: "trade-1".to_string(),
                price: 10_000,
                quantity: 5,
                aggressor: AggressorSide::Buy,
            }),
        };
        let event_debug = format!("{event:?}");
        assert!(event_debug.contains("[REDACTED]"));
        assert!(!event_debug.contains("trade-1"));
        assert!(!event_debug.contains("instrument:fixture:es"));
        assert_eq!(
            event.validate(nonzero(2), nonzero(2), 10),
            Err(ProviderContractError::GenerationMismatch)
        );
    }

    #[test]
    fn discovery_output_is_rejected_before_exceeding_memory_bounds() {
        let environment = ProviderEnvironment {
            provider_id: "fixture".to_string(),
            system_id: "test".to_string(),
            environment: "test".to_string(),
        };
        let event = ProviderSessionEvent::SystemsDiscovered {
            environments: vec![environment.clone(), environment],
        };
        assert_eq!(
            event.validate(nonzero(1), nonzero(1), 10),
            Err(ProviderContractError::SystemLimitExceeded { maximum: 1 })
        );
    }

    #[test]
    fn discovery_fields_have_explicit_byte_bounds() {
        let oversized_environment = "x".repeat(MAXIMUM_DISCOVERY_FIELD_BYTES + 1);
        let environment = ProviderEnvironment {
            provider_id: oversized_environment,
            system_id: "test".to_string(),
            environment: "test".to_string(),
        };
        assert_eq!(
            environment.validate(),
            Err(ProviderContractError::FieldTooLong {
                field: "provider_id",
                maximum: MAXIMUM_DISCOVERY_FIELD_BYTES,
            })
        );

        let oversized = "x".repeat(MAXIMUM_DISCOVERY_FIELD_BYTES + 1);
        let instrument = InstrumentDescriptor {
            instrument_id: "instrument:fixture:es".to_string(),
            provider_symbol: oversized,
            display_symbol: "ES".to_string(),
            venue_id: "fixture".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(25),
        };
        assert_eq!(
            instrument.validate(),
            Err(ProviderContractError::FieldTooLong {
                field: "provider_symbol",
                maximum: MAXIMUM_DISCOVERY_FIELD_BYTES,
            })
        );
    }
}
