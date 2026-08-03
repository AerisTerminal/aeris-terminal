//! Feed supervisor and binary market-bar WebSocket server.
//!
//! One supervised Coinbase session per product feeds the aggregator; a
//! sequence gap reconnects and resnapshots against the provider while the
//! plane's own downstream sequence never regresses. Clients receive a bounded
//! snapshot followed by live delta bars in the same binary market-bar protocol
//! the desktop already decodes. Overloaded clients are disconnected, never
//! buffered without bound.

use crate::aggregation::BarAggregator;
use crate::backfill::backfill_bars;
use crate::entitlement::{EntitlementError, EntitlementGuard};
use crate::instruments::{ProductMapping, map_product};
use crate::provenance::coinbase_bar_provenance;
use axiusflow_application::{ReplayProvenance, ReplaySnapshot, validate_provenanced_market_bar};
use axiusflow_coinbase_market_adapter::{CoinbaseConfig, CoinbaseSession};
use axiusflow_market_data::MarketBar;
use axiusflow_market_protocol_adapter::{
    DecimalConvention, encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes,
};
use axiusflow_protocols::{Provenanced, StreamDelta};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SUBSCRIPTION_ID: &str = "coinbase_spot_one_minute";
const SNAPSHOT_CHUNK_ITEMS: usize = 64;
const CLIENT_QUEUE_CAPACITY: usize = 64;
const MAXIMUM_FRAME_BYTES: usize = 65_536;
const OWNERSHIP_EPOCH: u64 = 1;

/// One connected client with its authorized principal when enforced.
struct ClientHandle {
    principal: Option<String>,
    sender: SyncSender<Vec<u8>>,
}

/// One product lane: mapping, aggregator, and its client broadcast.
struct ProductLane {
    mapping: ProductMapping,
    aggregator: BarAggregator,
    clients: Vec<ClientHandle>,
}

/// Health counters for evidence.
#[derive(Clone, Debug, Default)]
pub struct PlaneHealth {
    pub reconnects: u64,
    pub bars_completed: u64,
    pub trades_applied: u64,
    pub clients_disconnected_overload: u64,
    pub backfill_bars: u64,
    pub apply_failures: u64,
}

/// The supervised market data plane for the configured products.
pub struct MarketDataPlane {
    lanes: Arc<Mutex<HashMap<String, ProductLane>>>,
    convention: DecimalConvention,
    health: Arc<Mutex<PlaneHealth>>,
    entitlement: Option<std::sync::Arc<std::sync::RwLock<EntitlementGuard>>>,
}

impl MarketDataPlane {
    /// Builds the plane for the configured Coinbase products.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid products or convention failures.
    #[allow(dead_code)]
    pub fn try_new(products: &[String]) -> Result<Self, String> {
        Self::try_new_with_entitlement(products, None)
    }

    /// Builds the plane with an optional entitlement guard.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid products or convention failures.
    pub fn try_new_with_entitlement(
        products: &[String],
        entitlement: Option<EntitlementGuard>,
    ) -> Result<Self, String> {
        let mut lanes = HashMap::new();
        for product in products {
            lanes.insert(
                product.clone(),
                ProductLane {
                    mapping: map_product(product)?,
                    aggregator: BarAggregator::new(),
                    clients: Vec::new(),
                },
            );
        }
        Ok(Self {
            lanes: Arc::new(Mutex::new(lanes)),
            convention: DecimalConvention::try_new("usd", "base")
                .map_err(|error| error.to_string())?,
            health: Arc::new(Mutex::new(PlaneHealth::default())),
            entitlement: entitlement
                .map(|guard| std::sync::Arc::new(std::sync::RwLock::new(guard))),
        })
    }

    /// Current aggregate health.
    /// Authorizes one client token for one product's stream.
    ///
    /// # Errors
    ///
    /// Returns the denial reason when enforcement is active and the check
    /// fails; without enforcement every connection is authorized.
    pub fn authorize_client(
        &self,
        product: &str,
        token: Option<&str>,
    ) -> Result<Option<String>, EntitlementError> {
        let Some(entitlement) = &self.entitlement else {
            return Ok(None);
        };
        let guard = entitlement
            .read()
            .map_err(|_| EntitlementError::PolicyUnavailable)?;
        let token = token.ok_or(EntitlementError::TokenInvalid)?;
        guard.authorize(token, product).map(Some)
    }

    /// Swaps the entitlement guard and disconnects clients whose principal no
    /// longer holds a matching grant. Returns the number disconnected.
    ///
    /// # Errors
    ///
    /// Returns an error when the guard lock is poisoned.
    pub fn resnapshot_entitlement(&self, next: EntitlementGuard) -> Result<u64, String> {
        let Some(entitlement) = &self.entitlement else {
            return Ok(0);
        };
        let current_version = entitlement
            .read()
            .map_err(|_| "entitlement lock poisoned".to_string())?
            .policy_version();
        if next.policy_version() <= current_version {
            return Ok(0);
        }
        *entitlement
            .write()
            .map_err(|_| "entitlement lock poisoned".to_string())? = next;
        let mut disconnected = 0_u64;
        let mut lanes = self
            .lanes
            .lock()
            .map_err(|_| "lane lock poisoned".to_string())?;
        let guard = entitlement
            .read()
            .map_err(|_| "entitlement lock poisoned".to_string())?;
        for (product, lane) in lanes.iter_mut() {
            let resource = format!("stream:{}", product.to_ascii_lowercase());
            lane.clients.retain(|client| {
                let Some(principal) = &client.principal else {
                    return true;
                };
                let Ok(principal) =
                    axiusflow_authorization::PrincipalId::try_new(principal.clone())
                else {
                    disconnected += 1;
                    return false;
                };
                let Ok(resource_id) =
                    axiusflow_authorization::ResourceId::try_new(resource.clone())
                else {
                    return true;
                };
                let Ok(request) = axiusflow_authorization::AuthorizationRequest::try_new(
                    principal,
                    resource_id,
                    axiusflow_authorization::AuthorizationAction::Stream,
                    format!("resnapshot:{product}"),
                ) else {
                    return true;
                };
                let allowed = matches!(
                    guard.evaluator_outcome(&request),
                    axiusflow_authorization::AuthorizationOutcome::Allowed
                );
                if !allowed {
                    disconnected += 1;
                }
                allowed
            });
        }
        Ok(disconnected)
    }

    /// Health snapshot for evidence.
    #[allow(dead_code)]
    #[must_use]
    pub fn entitlement_revision(&self) -> Option<(u64, u64)> {
        self.entitlement.as_ref().and_then(|entitlement| {
            entitlement
                .read()
                .ok()
                .map(|guard| (guard.policy_version(), guard.key_revision()))
        })
    }

    pub fn health(&self) -> PlaneHealth {
        self.health
            .lock()
            .map(|health| PlaneHealth {
                reconnects: health.reconnects,
                bars_completed: health.bars_completed,
                trades_applied: health.trades_applied,
                clients_disconnected_overload: health.clients_disconnected_overload,
                backfill_bars: health.backfill_bars,
                apply_failures: health.apply_failures,
            })
            .unwrap_or_default()
    }

    /// Starts feed supervision and backfill; returns after setup completes.
    ///
    /// # Errors
    ///
    /// Returns an error when backfill or the first session fails.
    pub fn start(&self) -> Result<(), String> {
        let products: Vec<String> = self
            .lanes
            .lock()
            .map_err(|_| "lane lock poisoned".to_string())?
            .keys()
            .cloned()
            .collect();
        for product in &products {
            let bars = backfill_bars(product, 300)?;
            let mut lanes = self
                .lanes
                .lock()
                .map_err(|_| "lane lock poisoned".to_string())?;
            let lane = lanes
                .get_mut(product)
                .ok_or_else(|| "lane disappeared".to_string())?;
            let seeded = lane.aggregator.seed_backfill(bars)?;
            drop(lanes);
            self.health
                .lock()
                .map_err(|_| "health lock poisoned".to_string())?
                .backfill_bars += seeded as u64;
        }
        for product in products {
            let lanes = Arc::clone(&self.lanes);
            let health = Arc::clone(&self.health);
            let product_name = product.clone();
            std::thread::Builder::new()
                .name(format!("axiusflow-coinbase-feed-{product}"))
                .spawn(move || supervise_feed(&product_name, &lanes, &health))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Serves one client connection for one product: snapshot then live deltas.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown products, encoding failures, or overload.
    pub fn serve_client(
        &self,
        product: &str,
        principal: Option<String>,
        stream: &mut tungstenite::WebSocket<std::net::TcpStream>,
    ) -> Result<(), String> {
        let (sender, receiver) = sync_channel(CLIENT_QUEUE_CAPACITY);
        let snapshot_frames = self.register_client(product, principal, sender)?;
        for frame in &snapshot_frames {
            stream
                .send(tungstenite::Message::Binary(frame.clone().into()))
                .map_err(|error| error.to_string())?;
        }
        while let Ok(frame) = receiver.recv() {
            stream
                .send(tungstenite::Message::Binary(frame.into()))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn register_client(
        &self,
        product: &str,
        principal: Option<String>,
        sender: SyncSender<Vec<u8>>,
    ) -> Result<Vec<Vec<u8>>, String> {
        let mut lanes = self
            .lanes
            .lock()
            .map_err(|_| "lane lock poisoned".to_string())?;
        let lane = lanes
            .get_mut(product)
            .ok_or_else(|| format!("unknown product: {product}"))?;
        let history = lane.aggregator.history();
        let snapshot = build_snapshot(&lane.mapping, &history)?;
        let snapshot_id = format!(
            "coinbase_{}_bars_{}",
            product.to_ascii_lowercase(),
            snapshot.evidence().last_sequence
        );
        let envelopes = try_encode_replay_snapshot_chunk_envelopes(
            SUBSCRIPTION_ID,
            snapshot_id,
            &snapshot,
            &self.convention,
            NonZeroUsize::new(SNAPSHOT_CHUNK_ITEMS).ok_or("chunk limit cannot be zero")?,
        )
        .map_err(|error| format!("snapshot encode failed: {error}"))?;
        let maximum = NonZeroUsize::new(MAXIMUM_FRAME_BYTES).ok_or("frame limit cannot be zero")?;
        let mut frames = Vec::with_capacity(envelopes.len());
        for envelope in &envelopes {
            frames.push(
                encode_market_bar_stream_frame(envelope, maximum)
                    .map_err(|error| format!("frame encode failed: {error}"))?,
            );
        }
        lane.clients.push(ClientHandle { principal, sender });
        Ok(frames)
    }
}

fn supervise_feed(
    product: &str,
    lanes: &Arc<Mutex<HashMap<String, ProductLane>>>,
    health: &Arc<Mutex<PlaneHealth>>,
) {
    loop {
        let Ok(config) = CoinbaseConfig::try_new(vec![product.to_string()]) else {
            return;
        };
        let session = CoinbaseSession::new(config);
        let result = session.collect(Duration::from_hours(24), &mut |trade| {
            let mut lanes = lanes.lock().expect("lane lock poisoned");
            if let Some(lane) = lanes.get_mut(product) {
                health.lock().expect("health lock poisoned").trades_applied += 1;
                let applied = lane.aggregator.apply_trade(trade);
                if applied.is_err() {
                    health.lock().expect("health lock poisoned").apply_failures += 1;
                }
                if let Ok(Some(bar)) = applied {
                    health.lock().expect("health lock poisoned").bars_completed += 1;
                    if let Ok(frame) = encode_delta(&lane.mapping, &bar) {
                        let before = lane.clients.len();
                        lane.clients
                            .retain(|client| client.sender.try_send(frame.clone()).is_ok());
                        let dropped = before - lane.clients.len();
                        if dropped > 0 {
                            health
                                .lock()
                                .expect("health lock poisoned")
                                .clients_disconnected_overload += dropped as u64;
                        }
                    }
                }
            }
        });
        health.lock().expect("health lock poisoned").reconnects += 1;
        if result.is_err() {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn encode_delta(mapping: &ProductMapping, bar: &MarketBar) -> Result<Vec<u8>, String> {
    let item = Provenanced::new(
        *bar,
        coinbase_bar_provenance(bar, unix_nanos_now(), OWNERSHIP_EPOCH),
    );
    validate_provenanced_market_bar(&item).map_err(|error| format!("{error:?}"))?;
    let delta = StreamDelta::try_new(
        bar.source_sequence.saturating_sub(1).max(1),
        bar.source_sequence,
        item,
    )
    .map_err(|error| format!("{error:?}"))?;
    let envelope = try_encode_replay_delta_envelope(
        SUBSCRIPTION_ID,
        &mapping.instrument,
        &mapping.bar_definition,
        &delta,
        &DecimalConvention::try_new("usd", "base").map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("delta encode failed: {error}"))?;
    let maximum = NonZeroUsize::new(MAXIMUM_FRAME_BYTES).ok_or("frame limit cannot be zero")?;
    encode_market_bar_stream_frame(&envelope, maximum)
        .map_err(|error| format!("frame encode failed: {error}"))
}

fn build_snapshot(
    mapping: &ProductMapping,
    history: &[MarketBar],
) -> Result<ReplaySnapshot, String> {
    let mut bars = Vec::with_capacity(history.len());
    for bar in history {
        let item = Provenanced::new(
            *bar,
            coinbase_bar_provenance(bar, unix_nanos_now(), OWNERSHIP_EPOCH),
        );
        validate_provenanced_market_bar(&item).map_err(|error| format!("{error:?}"))?;
        bars.push(item);
    }
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Err("cannot snapshot an empty bar history".to_string());
    };
    let mut evidence = axiusflow_protocols::SnapshotEvidence {
        partition_id: 0,
        ownership_epoch: OWNERSHIP_EPOCH,
        generation: 1,
        first_sequence: first.value().source_sequence,
        last_sequence: last.value().source_sequence,
        schema_version: 1,
        checksum: [0; 32],
    };
    evidence.checksum = axiusflow_protocols::compute_market_snapshot_checksum(
        &evidence,
        axiusflow_protocols::MarketSnapshotIdentityRef {
            instrument_id: mapping.instrument.instrument_id.as_str(),
            instrument_revision: mapping.instrument.revision,
            bar_definition_id: &mapping.bar_definition.definition_id,
            bar_definition_version: mapping.bar_definition.version,
            bar_interval_seconds: mapping.bar_definition.interval_seconds,
        },
        bars.iter().map(|item| {
            let bar = item.value();
            axiusflow_protocols::MarketValueChecksumRef {
                source_sequence: bar.source_sequence,
                exchange_timestamp_seconds: bar.exchange_timestamp_seconds,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                provenance: item.provenance(),
            }
        }),
    );
    ReplaySnapshot::try_new_provenanced(
        mapping.instrument.clone(),
        ReplayProvenance::LiveProvider,
        mapping.bar_definition.clone(),
        evidence,
        bars,
    )
    .map_err(|error| format!("{error:?}"))
}

fn unix_nanos_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
