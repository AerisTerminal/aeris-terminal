//! Feed supervisor and binary market-bar WebSocket server.
//!
//! One supervised Coinbase session per product feeds the aggregator; a
//! sequence gap reconnects and resnapshots against the provider while the
//! plane's own downstream sequence never regresses. Every completed bar is
//! accepted by its product's fenced partition and published through the
//! realtime direct/durable fanout: the direct branch drives the client
//! broadcast, the durable branch feeds the Redpanda tap when one is
//! configured. Clients receive a bounded snapshot followed by live delta bars
//! in the same binary market-bar protocol the desktop already decodes.
//! Overloaded clients are disconnected, never buffered without bound.

use crate::aggregation::BarAggregator;
use crate::backfill::backfill_bars;
use crate::entitlement::{EntitlementError, EntitlementGuard};
use crate::fanout::ProductFanout;
#[cfg(all(target_os = "linux", feature = "redpanda"))]
use crate::fanout::{durable_envelope, durable_partition_key};
use crate::instruments::{ProductMapping, map_product};
use axiusflow_application::ReplayProvenance;
use axiusflow_coinbase_market_adapter::{CoinbaseConfig, CoinbaseSession};
use axiusflow_market_protocol_adapter::{
    DecimalConvention, encode_market_bar_stream_frame,
    try_encode_canonical_market_bar_delta_envelope, try_encode_replay_snapshot_chunk_envelopes,
    try_project_canonical_market_bar_snapshot,
};
use axiusflow_realtime::CanonicalMarketEvent;
use std::collections::HashMap;
use std::num::NonZeroUsize;
#[cfg(all(target_os = "linux", feature = "redpanda"))]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(all(target_os = "linux", feature = "redpanda"))]
use std::sync::mpsc::TrySendError;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SUBSCRIPTION_ID: &str = "coinbase_spot_one_minute";
const SNAPSHOT_CHUNK_ITEMS: usize = 64;
const CLIENT_QUEUE_CAPACITY: usize = 64;
const MAXIMUM_FRAME_BYTES: usize = 65_536;
const MAXIMUM_LATEST_STATE_ITEMS: usize = 1_024;

/// One connected client with its authorized principal when enforced.
struct ClientHandle {
    principal: Option<String>,
    sender: SyncSender<Vec<u8>>,
}

/// One product lane: mapping, aggregator, fenced fanout, and client broadcast.
struct ProductLane {
    mapping: ProductMapping,
    aggregator: BarAggregator,
    fanout: ProductFanout,
    clients: Vec<ClientHandle>,
}

/// The durable tap consuming the fanout's durable branch.
pub enum DurableTap {
    /// One Redpanda producer bound to the plane's durable bar topic.
    #[cfg(all(target_os = "linux", feature = "redpanda"))]
    Redpanda(RedpandaTap),
    /// No tap configured; durable events are counted and dropped, never
    /// silently presented as published.
    Inactive,
}

impl DurableTap {
    /// Whether a live durable tap is consuming the durable branch.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        match self {
            #[cfg(all(target_os = "linux", feature = "redpanda"))]
            Self::Redpanda(_) => true,
            Self::Inactive => false,
        }
    }

    #[cfg(all(target_os = "linux", feature = "redpanda"))]
    pub fn redpanda(producer: axiusflow_streaming::RedpandaProducer) -> Result<Self, String> {
        RedpandaTap::spawn(producer).map(Self::Redpanda)
    }

    fn worker_health(&self) -> (u64, u64, bool) {
        match self {
            #[cfg(all(target_os = "linux", feature = "redpanda"))]
            Self::Redpanda(tap) => tap.health(),
            Self::Inactive => (0, 0, false),
        }
    }
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
struct DurablePublication {
    key: Vec<u8>,
    envelope: axiusflow_streaming::DurableEventEnvelope,
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
pub struct RedpandaTap {
    sender: SyncSender<DurablePublication>,
    enqueue_lock: Mutex<()>,
    delivered: Arc<AtomicU64>,
    delivery_failures: Arc<AtomicU64>,
    recovery_required: Arc<AtomicBool>,
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
impl RedpandaTap {
    fn spawn(mut producer: axiusflow_streaming::RedpandaProducer) -> Result<Self, String> {
        let (sender, receiver) = sync_channel::<DurablePublication>(1_024);
        let delivered = Arc::new(AtomicU64::new(0));
        let delivery_failures = Arc::new(AtomicU64::new(0));
        let recovery_required = Arc::new(AtomicBool::new(false));
        let worker_delivered = Arc::clone(&delivered);
        let worker_failures = Arc::clone(&delivery_failures);
        let worker_recovery = Arc::clone(&recovery_required);
        std::thread::Builder::new()
            .name("axiusflow-redpanda-durable-tap".to_string())
            .spawn(move || {
                while let Ok(publication) = receiver.recv() {
                    let before = producer.health();
                    loop {
                        if producer
                            .publish(&publication.key, &publication.envelope)
                            .is_ok()
                        {
                            break;
                        }
                        worker_failures.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    loop {
                        let flush = producer.flush();
                        let after = producer.health();
                        let delivered_once = after.delivered == before.delivered.saturating_add(1)
                            && after.failed == before.failed;
                        if delivered_once {
                            worker_delivered.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        worker_failures.fetch_add(1, Ordering::Relaxed);
                        if after.failed > before.failed {
                            worker_recovery.store(true, Ordering::Release);
                            return;
                        }
                        let _ = flush;
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            })
            .map_err(|error| format!("cannot start durable tap worker: {error}"))?;
        Ok(Self {
            sender,
            enqueue_lock: Mutex::new(()),
            delivered,
            delivery_failures,
            recovery_required,
        })
    }

    fn enqueue(&self, event: &CanonicalMarketEvent) -> Result<(), String> {
        let _guard = self
            .enqueue_lock
            .lock()
            .map_err(|_| "durable tap enqueue lock poisoned".to_string())?;
        if self.recovery_required.load(Ordering::Acquire) {
            return Err(
                "durable tap recovery is required before later sequences can publish".to_string(),
            );
        }
        let publication = DurablePublication {
            key: durable_partition_key(event),
            envelope: durable_envelope(event)?,
        };
        self.sender.try_send(publication).map_err(|error| {
            self.recovery_required.store(true, Ordering::Release);
            match error {
                TrySendError::Full(_) => "durable tap queue is full".to_string(),
                TrySendError::Disconnected(_) => "durable tap worker stopped".to_string(),
            }
        })
    }

    fn health(&self) -> (u64, u64, bool) {
        (
            self.delivered.load(Ordering::Relaxed),
            self.delivery_failures.load(Ordering::Relaxed),
            self.recovery_required.load(Ordering::Acquire),
        )
    }
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
    pub fanout_rejected: u64,
    pub durable_queued: u64,
    pub durable_delivered: u64,
    pub durable_failed: u64,
    pub durable_dropped_without_tap: u64,
    pub durable_recovery_required: bool,
}

/// The supervised market data plane for the configured products.
pub struct MarketDataPlane {
    lanes: Arc<Mutex<HashMap<String, ProductLane>>>,
    convention: DecimalConvention,
    health: Arc<Mutex<PlaneHealth>>,
    entitlement: Option<std::sync::Arc<std::sync::RwLock<EntitlementGuard>>>,
    durable_tap: Arc<DurableTap>,
}

impl MarketDataPlane {
    /// Builds the plane for the configured Coinbase products.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid products or convention failures.
    #[allow(dead_code)]
    pub fn try_new(products: &[String]) -> Result<Self, String> {
        Self::try_new_with_entitlement(products, None, "dev", 1, DurableTap::Inactive)
    }

    /// Builds the plane with an optional entitlement guard and durable tap.
    ///
    /// Each product lane owns one fenced partition whose identifier is the
    /// product's one-based configuration index.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid products, partitions, topics, or
    /// convention failures.
    pub fn try_new_with_entitlement(
        products: &[String],
        entitlement: Option<EntitlementGuard>,
        environment: &str,
        ownership_epoch: u64,
        durable_tap: DurableTap,
    ) -> Result<Self, String> {
        let mut lanes = HashMap::new();
        for (index, product) in products.iter().enumerate() {
            let partition_id = u32::try_from(index + 1)
                .map_err(|_| "too many products for partition identity".to_string())?;
            lanes.insert(
                product.clone(),
                ProductLane {
                    mapping: map_product(product)?,
                    aggregator: BarAggregator::new(),
                    fanout: ProductFanout::try_new(
                        partition_id,
                        "market_data_plane",
                        environment,
                        ownership_epoch,
                    )?,
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
            durable_tap: Arc::new(durable_tap),
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

    /// Whether the durable tap is actively publishing the durable branch.
    #[must_use]
    pub fn durable_tap_active(&self) -> bool {
        self.durable_tap.is_active()
    }

    pub fn health(&self) -> PlaneHealth {
        let (durable_delivered, worker_failures, durable_recovery_required) =
            self.durable_tap.worker_health();
        self.health
            .lock()
            .map(|health| PlaneHealth {
                reconnects: health.reconnects,
                bars_completed: health.bars_completed,
                trades_applied: health.trades_applied,
                clients_disconnected_overload: health.clients_disconnected_overload,
                backfill_bars: health.backfill_bars,
                apply_failures: health.apply_failures,
                fanout_rejected: health.fanout_rejected,
                durable_queued: health.durable_queued,
                durable_delivered,
                durable_failed: health.durable_failed.saturating_add(worker_failures),
                durable_dropped_without_tap: health.durable_dropped_without_tap,
                durable_recovery_required,
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
            let history = lane.aggregator.history();
            lane.fanout
                .install_backfill(&lane.mapping, &history, unix_nanos_now())?;
            drop(lanes);
            self.health
                .lock()
                .map_err(|_| "health lock poisoned".to_string())?
                .backfill_bars += seeded as u64;
        }
        for product in products {
            let lanes = Arc::clone(&self.lanes);
            let health = Arc::clone(&self.health);
            let durable_tap = Arc::clone(&self.durable_tap);
            let product_name = product.clone();
            std::thread::Builder::new()
                .name(format!("axiusflow-coinbase-feed-{product}"))
                .spawn(move || supervise_feed(&product_name, &lanes, &health, &durable_tap))
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
        let canonical_snapshot = lane.fanout.latest_snapshot(
            NonZeroUsize::new(MAXIMUM_LATEST_STATE_ITEMS)
                .ok_or("latest-state item limit cannot be zero")?,
        )?;
        let snapshot = try_project_canonical_market_bar_snapshot(
            &canonical_snapshot,
            &lane.mapping.instrument,
            ReplayProvenance::LiveProvider,
            &lane.mapping.bar_definition,
        )
        .map_err(|error| format!("canonical snapshot projection failed: {error}"))?;
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
    durable_tap: &Arc<DurableTap>,
) {
    loop {
        let Ok(config) = CoinbaseConfig::try_new(vec![product.to_string()]) else {
            return;
        };
        let session = CoinbaseSession::new(config);
        let result = session.collect(Duration::from_hours(24), &mut |trade| {
            let durable = {
                let mut lanes = lanes.lock().expect("lane lock poisoned");
                let Some(lane) = lanes.get_mut(product) else {
                    return;
                };
                health.lock().expect("health lock poisoned").trades_applied += 1;
                let applied = lane.aggregator.apply_trade(trade);
                if applied.is_err() {
                    health.lock().expect("health lock poisoned").apply_failures += 1;
                }
                if let Ok(Some(bar)) = applied {
                    health.lock().expect("health lock poisoned").bars_completed += 1;
                    if let Ok(fanned) =
                        lane.fanout
                            .publish_bar(&lane.mapping, &bar, unix_nanos_now())
                    {
                        if fanned.direct.header().source_sequence == bar.source_sequence {
                            if let Ok(frame) = encode_delta(&lane.mapping, &fanned.direct) {
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
                            Some(fanned.durable)
                        } else {
                            health.lock().expect("health lock poisoned").fanout_rejected += 1;
                            None
                        }
                    } else {
                        eprintln!("fanout rejected {product} sequence {}", bar.source_sequence);
                        health.lock().expect("health lock poisoned").fanout_rejected += 1;
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(event) = durable {
                publish_durable(durable_tap, &event, health);
            }
        });
        health.lock().expect("health lock poisoned").reconnects += 1;
        if result.is_err() {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn publish_durable(
    durable_tap: &DurableTap,
    event: &CanonicalMarketEvent,
    health: &Arc<Mutex<PlaneHealth>>,
) {
    #[cfg(not(all(target_os = "linux", feature = "redpanda")))]
    let _ = event;
    match durable_tap {
        #[cfg(all(target_os = "linux", feature = "redpanda"))]
        DurableTap::Redpanda(producer) => {
            let result = producer.enqueue(event);
            let mut health = health.lock().expect("health lock poisoned");
            if result.is_ok() {
                health.durable_queued += 1;
            } else {
                eprintln!(
                    "durable tap rejected partition={} sequence={}; later durable publication remains latched until stream repair and restart",
                    event.header().partition_id,
                    event.header().source_sequence
                );
                health.durable_failed += 1;
            }
        }
        DurableTap::Inactive => {
            health
                .lock()
                .expect("health lock poisoned")
                .durable_dropped_without_tap += 1;
        }
    }
}

fn encode_delta(mapping: &ProductMapping, event: &CanonicalMarketEvent) -> Result<Vec<u8>, String> {
    let envelope = try_encode_canonical_market_bar_delta_envelope(
        SUBSCRIPTION_ID,
        &mapping.instrument,
        &mapping.bar_definition,
        event.header().source_sequence.saturating_sub(1).max(1),
        event,
        &DecimalConvention::try_new("usd", "base").map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("delta encode failed: {error}"))?;
    let maximum = NonZeroUsize::new(MAXIMUM_FRAME_BYTES).ok_or("frame limit cannot be zero")?;
    encode_market_bar_stream_frame(&envelope, maximum)
        .map_err(|error| format!("frame encode failed: {error}"))
}

fn unix_nanos_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
