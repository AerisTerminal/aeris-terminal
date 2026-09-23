use std::collections::{BTreeMap, BTreeSet};

use asceify_contracts::{InstallProviderInstrument, PriceAlertCondition, PriceAlertFrequency};
use asceify_market_engine::ConsumerId;

use crate::{MAXIMUM_PRICE_ALERTS_PER_CONSUMER, MarketPriceAlert, MarketPriceAlertTrigger};

const MAXIMUM_PRICE_ALERTS: usize = 4_096;
const MAXIMUM_ALERT_INSTRUMENTS: usize = 64;
const MAXIMUM_ALERT_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct AlertInstrumentKey {
    provider: String,
    instrument_id: String,
    entitlement_id: String,
    price_scale: u32,
}

impl AlertInstrumentKey {
    pub(super) fn from_instrument(instrument: &InstallProviderInstrument) -> Self {
        Self {
            provider: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            price_scale: instrument.price_scale,
        }
    }

    pub(super) fn provider(&self) -> &str {
        &self.provider
    }
}

#[derive(Clone)]
struct RegisteredAlert {
    consumer_id: ConsumerId,
    alert: MarketPriceAlert,
    previous_price: Option<i64>,
}

#[derive(Clone, Copy)]
struct PriceObservation {
    session_generation: u64,
    source_sequence: u64,
    price: i64,
}

#[derive(Default)]
pub(super) struct PriceAlertRegistry {
    alerts: BTreeMap<AlertInstrumentKey, Vec<RegisteredAlert>>,
    observations: BTreeMap<AlertInstrumentKey, PriceObservation>,
}

impl PriceAlertRegistry {
    pub(super) fn replace_consumer_alerts(
        &mut self,
        consumer_id: ConsumerId,
        alerts: Vec<MarketPriceAlert>,
    ) -> Result<(), String> {
        validate_alerts(&alerts)?;
        let retained = self
            .alerts
            .values()
            .flat_map(|items| items.iter())
            .filter(|item| item.consumer_id != consumer_id)
            .count();
        if retained.saturating_add(alerts.len()) > MAXIMUM_PRICE_ALERTS {
            return Err("price alert capacity is exhausted".to_string());
        }
        let prospective_instruments = self
            .alerts
            .iter()
            .filter(|(_, items)| {
                items
                    .iter()
                    .any(|item| item.consumer_id != consumer_id && item.alert.active)
            })
            .map(|(key, _)| key.clone())
            .chain(
                alerts
                    .iter()
                    .filter(|alert| alert.active)
                    .map(|alert| AlertInstrumentKey::from_instrument(&alert.instrument)),
            )
            .collect::<BTreeSet<_>>();
        if prospective_instruments.len() > MAXIMUM_ALERT_INSTRUMENTS {
            return Err("price alert instrument capacity is exhausted".to_string());
        }

        let previous = self
            .alerts
            .iter()
            .flat_map(|(key, items)| {
                items
                    .iter()
                    .filter(move |item| item.consumer_id == consumer_id)
                    .map(move |item| (item.alert.id.clone(), (key.clone(), item.previous_price)))
            })
            .collect::<BTreeMap<_, _>>();
        self.remove_consumer(consumer_id);

        for alert in alerts {
            let key = AlertInstrumentKey::from_instrument(&alert.instrument);
            let previous_price = previous
                .get(&alert.id)
                .filter(|(previous_key, _)| previous_key == &key)
                .and_then(|(_, price)| *price)
                .or_else(|| self.observations.get(&key).map(|value| value.price));
            self.alerts.entry(key).or_default().push(RegisteredAlert {
                consumer_id,
                alert,
                previous_price,
            });
        }

        self.prune_observations();
        Ok(())
    }

    pub(super) fn remove_consumer(&mut self, consumer_id: ConsumerId) {
        self.alerts.retain(|_, items| {
            items.retain(|item| item.consumer_id != consumer_id);
            !items.is_empty()
        });
        self.prune_observations();
    }

    pub(super) fn reset_provider_baselines(&mut self, provider: &str) {
        self.observations
            .retain(|key, _| key.provider() != provider);
        for (key, items) in &mut self.alerts {
            if key.provider() == provider {
                for item in items {
                    item.previous_price = None;
                }
            }
        }
    }

    pub(super) fn has_active_provider(&self, provider: &str) -> bool {
        self.alerts.iter().any(|(key, items)| {
            key.provider() == provider && items.iter().any(|item| item.alert.active)
        })
    }

    pub(super) fn active_instruments(&self, provider: &str) -> Vec<InstallProviderInstrument> {
        self.alerts
            .iter()
            .filter(|(key, items)| {
                key.provider() == provider && items.iter().any(|item| item.alert.active)
            })
            .filter_map(|(_, items)| {
                items
                    .iter()
                    .filter(|item| item.alert.active)
                    .max_by_key(|item| item.alert.instrument.session_generation)
                    .map(|item| item.alert.instrument.clone())
            })
            .collect()
    }

    pub(super) fn evaluate(
        &mut self,
        instrument: &InstallProviderInstrument,
        session_generation: u64,
        source_sequence: u64,
        price: i64,
        observed_unix_nanos: i64,
    ) -> Vec<MarketPriceAlertTrigger> {
        let key = AlertInstrumentKey::from_instrument(instrument);
        let observation = PriceObservation {
            session_generation,
            source_sequence,
            price,
        };
        let Some(items) = self.alerts.get_mut(&key) else {
            return Vec::new();
        };
        let session_changed = match self.observations.get(&key) {
            Some(previous) if previous.session_generation > session_generation => {
                return Vec::new();
            }
            Some(previous)
                if previous.session_generation == session_generation
                    && previous.source_sequence >= source_sequence =>
            {
                return Vec::new();
            }
            Some(previous) => previous.session_generation != session_generation,
            None => true,
        };
        self.observations.insert(key, observation);
        if session_changed {
            for item in items {
                item.previous_price = Some(price);
            }
            return Vec::new();
        }

        let mut triggered = Vec::new();
        for item in items {
            let previous_price = item.previous_price.replace(price);
            if !item.alert.active
                || !previous_price.is_some_and(|previous| {
                    condition_satisfied(item.alert.condition, previous, price, item.alert.price)
                })
            {
                continue;
            }
            let remains_active = item.alert.frequency == PriceAlertFrequency::EveryTime;
            item.alert.active = remains_active;
            triggered.push(MarketPriceAlertTrigger {
                consumer_id: item.consumer_id,
                alert_id: item.alert.id.clone(),
                instrument: item.alert.instrument.clone(),
                threshold_price: item.alert.price,
                observed_price: price,
                condition: item.alert.condition,
                frequency: item.alert.frequency,
                observed_unix_nanos,
                remains_active,
            });
        }
        triggered
    }

    fn prune_observations(&mut self) {
        self.observations
            .retain(|key, _| self.alerts.contains_key(key));
    }
}

fn validate_alerts(alerts: &[MarketPriceAlert]) -> Result<(), String> {
    if alerts.len() > MAXIMUM_PRICE_ALERTS_PER_CONSUMER {
        return Err(format!(
            "a market consumer supports at most {MAXIMUM_PRICE_ALERTS_PER_CONSUMER} price alerts"
        ));
    }
    let mut ids = BTreeSet::new();
    for alert in alerts {
        if alert.id.is_empty() || alert.id.len() > MAXIMUM_ALERT_ID_BYTES {
            return Err("price alert identity is invalid".to_string());
        }
        if !ids.insert(alert.id.as_str()) {
            return Err("price alert identities must be unique per consumer".to_string());
        }
        if alert.instrument.price_scale > 18 {
            return Err("price alert scale exceeds fixed-point capacity".to_string());
        }
    }
    Ok(())
}

const fn condition_satisfied(
    condition: PriceAlertCondition,
    previous: i64,
    current: i64,
    threshold: i64,
) -> bool {
    match condition {
        PriceAlertCondition::Crossing => {
            (previous < threshold && current >= threshold)
                || (previous > threshold && current <= threshold)
        }
        PriceAlertCondition::CrossingUp => previous < threshold && current >= threshold,
        PriceAlertCondition::CrossingDown => previous > threshold && current <= threshold,
        PriceAlertCondition::GreaterThan => previous <= threshold && current > threshold,
        PriceAlertCondition::LessThan => previous >= threshold && current < threshold,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn consumer(value: u64) -> ConsumerId {
        ConsumerId(NonZeroU64::new(value).expect("non-zero consumer"))
    }

    fn instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "hyperliquid".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "hyperliquid:perp:BTC".to_string(),
            provider_symbol: "BTC".to_string(),
            display_symbol: "BTC-USDC".to_string(),
            venue_id: "Hyperliquid".to_string(),
            price_scale: 2,
            quantity_scale: 2,
            entitlement_id: "hyperliquid-public".to_string(),
            price_increment: None,
        }
    }

    fn alert(
        id: &str,
        condition: PriceAlertCondition,
        frequency: PriceAlertFrequency,
    ) -> MarketPriceAlert {
        MarketPriceAlert {
            id: id.to_string(),
            instrument: instrument(),
            price: 10_000,
            condition,
            frequency,
            active: true,
        }
    }

    #[test]
    fn conditions_require_an_actual_threshold_transition() {
        assert!(condition_satisfied(
            PriceAlertCondition::CrossingUp,
            9_999,
            10_000,
            10_000
        ));
        assert!(condition_satisfied(
            PriceAlertCondition::CrossingDown,
            10_001,
            10_000,
            10_000
        ));
        assert!(condition_satisfied(
            PriceAlertCondition::GreaterThan,
            10_000,
            10_001,
            10_000
        ));
        assert!(condition_satisfied(
            PriceAlertCondition::LessThan,
            10_000,
            9_999,
            10_000
        ));
        assert!(!condition_satisfied(
            PriceAlertCondition::GreaterThan,
            10_001,
            10_002,
            10_000
        ));
    }

    #[test]
    fn only_once_deactivates_while_every_time_rearms() {
        let mut registry = PriceAlertRegistry::default();
        registry
            .replace_consumer_alerts(
                consumer(7),
                vec![
                    alert(
                        "once",
                        PriceAlertCondition::CrossingUp,
                        PriceAlertFrequency::OnlyOnce,
                    ),
                    alert(
                        "repeat",
                        PriceAlertCondition::Crossing,
                        PriceAlertFrequency::EveryTime,
                    ),
                ],
            )
            .expect("alerts install");
        let instrument = instrument();
        assert!(registry.evaluate(&instrument, 1, 1, 9_900, 1).is_empty());
        let first = registry.evaluate(&instrument, 1, 2, 10_100, 2);
        assert_eq!(first.len(), 2);
        assert!(
            first
                .iter()
                .any(|item| item.alert_id == "once" && !item.remains_active)
        );
        assert!(
            first
                .iter()
                .any(|item| item.alert_id == "repeat" && item.remains_active)
        );
        let second = registry.evaluate(&instrument, 1, 3, 9_900, 3);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].alert_id, "repeat");
    }

    #[test]
    fn new_provider_session_seeds_without_a_false_crossing() {
        let mut registry = PriceAlertRegistry::default();
        registry
            .replace_consumer_alerts(
                consumer(9),
                vec![alert(
                    "repeat",
                    PriceAlertCondition::Crossing,
                    PriceAlertFrequency::EveryTime,
                )],
            )
            .expect("alert installs");
        let instrument = instrument();
        assert!(registry.evaluate(&instrument, 1, 1, 9_900, 1).is_empty());
        assert!(registry.evaluate(&instrument, 2, 1, 10_100, 2).is_empty());
        assert_eq!(registry.evaluate(&instrument, 2, 2, 9_900, 3).len(), 1);
    }
}
