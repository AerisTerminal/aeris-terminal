//! Redpanda durable producer behind bounded publish and flush calls.
//!
//! One producer owns one topic. Publication uses an idempotent producer with
//! `acks=all`; delivery evidence returns through a bounded context channel so a
//! stalled broker fails loudly instead of growing memory.

use crate::errors::StreamingError;
use crate::{DurableEventEnvelope, DurableTopic};
use rdkafka::client::ClientContext;
use rdkafka::config::ClientConfig;
use rdkafka::message::{DeliveryResult, Message};
use rdkafka::producer::{BaseRecord, ProducerContext, ThreadedProducer};
use rdkafka::util::IntoOpaque;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

/// Bounded producer configuration; every value is explicit, nothing is inherited
/// from a broker or environment default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedpandaProducerConfig {
    pub bootstrap_servers: String,
    pub client_id: String,
    pub topic: DurableTopic,
    pub maximum_in_flight: usize,
    pub delivery_capacity: usize,
    pub request_timeout: Duration,
}

/// Delivery evidence for one published envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryEvidence {
    pub partition: i32,
    pub offset: i64,
}

struct DeliveryTracker {
    sender: SyncSender<Result<DeliveryEvidence, String>>,
}

#[derive(Clone)]
struct SequenceOpaque;

// The rdkafka delivery callback round-trips this opaque value through C; the
// invariants (created by `as_ptr`, reclaimed exactly once in `from_ptr`) are
// stated at this boundary, mirroring rdkafka's own boxed-opaque example.
#[allow(unsafe_code)]
impl IntoOpaque for SequenceOpaque {
    fn into_ptr(self) -> *mut std::ffi::c_void {
        Box::into_raw(Box::new(self)).cast()
    }

    unsafe fn from_ptr(ptr: *mut std::ffi::c_void) -> Self {
        // SAFETY: the pointer was created by `into_ptr` and is reclaimed exactly
        // once by the delivery callback.
        unsafe { *Box::from_raw(ptr.cast::<SequenceOpaque>()) }
    }
}

impl ClientContext for DeliveryTracker {}

impl ProducerContext for DeliveryTracker {
    type DeliveryOpaque = SequenceOpaque;

    fn delivery(&self, result: &DeliveryResult<'_>, _opaque: SequenceOpaque) {
        let mapped = match result {
            Ok(message) => Ok(DeliveryEvidence {
                partition: message.partition(),
                offset: message.offset(),
            }),
            Err((error, _)) => Err(error.to_string()),
        };
        let _ = self.sender.send(mapped);
    }
}

/// Health counters for one producer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProducerHealth {
    pub published: u64,
    pub delivered: u64,
    pub failed: u64,
    pub in_flight: usize,
}

/// One Redpanda durable producer bound to one topic.
pub struct RedpandaProducer {
    producer: ThreadedProducer<DeliveryTracker>,
    topic: String,
    maximum_in_flight: usize,
    request_timeout: Duration,
    deliveries: Receiver<Result<DeliveryEvidence, String>>,
    published: u64,
    delivered: u64,
    failed: u64,
}

impl RedpandaProducer {
    /// Connects with an idempotent producer and `acks=all`.
    ///
    /// # Errors
    ///
    /// Returns an error when the client configuration is invalid.
    pub fn connect(config: &RedpandaProducerConfig) -> Result<Self, StreamingError> {
        let (sender, deliveries) = sync_channel(config.delivery_capacity.max(1));
        let producer = ClientConfig::new()
            .set("bootstrap.servers", &config.bootstrap_servers)
            .set("client.id", &config.client_id)
            .set("acks", "all")
            .set("enable.idempotence", "true")
            .set(
                "message.timeout.ms",
                u64::try_from(config.request_timeout.as_millis())
                    .unwrap_or(u64::MAX)
                    .to_string(),
            )
            .set(
                "queue.buffering.max.messages",
                config.maximum_in_flight.max(1).to_string(),
            )
            .create_with_context::<_, ThreadedProducer<DeliveryTracker>>(DeliveryTracker { sender })
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        Ok(Self {
            producer,
            topic: config.topic.name().to_string(),
            maximum_in_flight: config.maximum_in_flight.max(1),
            request_timeout: config.request_timeout,
            deliveries,
            published: 0,
            delivered: 0,
            failed: 0,
        })
    }

    /// Publishes one envelope with its Section 11.3 partition key.
    ///
    /// # Errors
    ///
    /// Returns an error when the bounded in-flight window is full or the client
    /// rejects the record.
    pub fn publish(
        &mut self,
        partition_key: &[u8],
        envelope: &DurableEventEnvelope,
    ) -> Result<(), StreamingError> {
        self.drain_deliveries();
        if self.in_flight() >= self.maximum_in_flight {
            return Err(StreamingError::QueueFull);
        }
        self.published += 1;
        let encoded = envelope.encode();
        let record = BaseRecord::with_opaque_to(&self.topic, SequenceOpaque)
            .key(partition_key)
            .payload(&encoded);
        self.producer
            .send(record)
            .map_err(|(error, _)| StreamingError::Client(error.to_string()))?;
        self.producer.poll(Duration::ZERO);
        Ok(())
    }

    /// Waits for every in-flight delivery up to the request timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when any delivery failed or the timeout expires first.
    pub fn flush(&mut self) -> Result<u64, StreamingError> {
        let deadline = Instant::now() + self.request_timeout;
        loop {
            self.drain_deliveries();
            if self.in_flight() == 0 {
                return Ok(self.delivered);
            }
            if Instant::now() >= deadline {
                return Err(StreamingError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Current health counters.
    #[must_use]
    pub fn health(&self) -> ProducerHealth {
        ProducerHealth {
            published: self.published,
            delivered: self.delivered,
            failed: self.failed,
            in_flight: self.in_flight(),
        }
    }

    fn in_flight(&self) -> usize {
        usize::try_from(self.published.saturating_sub(self.delivered + self.failed))
            .unwrap_or(usize::MAX)
    }

    fn drain_deliveries(&mut self) {
        self.producer.poll(Duration::ZERO);
        loop {
            match self.deliveries.try_recv() {
                Ok(Ok(_)) => self.delivered += 1,
                Ok(Err(_)) => self.failed += 1,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }
}

/// Receives envelopes back for conformance verification.
pub mod conformance {
    use crate::errors::StreamingError;

    /// One consumed message with its partition key.
    pub type KeyedPayload = (Vec<u8>, Vec<u8>);

    use rdkafka::config::ClientConfig;
    use rdkafka::consumer::{BaseConsumer, Consumer};
    use rdkafka::message::Message;
    use rdkafka::topic_partition_list::TopicPartitionList;
    use std::time::{Duration, Instant};

    /// Reads up to `maximum` messages from one topic partition.
    ///
    /// # Errors
    ///
    /// Returns an error when the consumer cannot be built or polling fails.
    pub fn read_partition(
        bootstrap_servers: &str,
        group_id: &str,
        topic: &str,
        partition: i32,
        maximum: usize,
        deadline: Duration,
    ) -> Result<Vec<KeyedPayload>, StreamingError> {
        let consumer: BaseConsumer = ClientConfig::new()
            .set("bootstrap.servers", bootstrap_servers)
            .set("group.id", group_id)
            .set("enable.auto.commit", "false")
            .set("auto.offset.reset", "earliest")
            .create()
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        let mut assignment = TopicPartitionList::new();
        assignment
            .add_partition(topic, partition)
            .set_offset(rdkafka::Offset::Beginning)
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        consumer
            .assign(&assignment)
            .map_err(|error| StreamingError::Client(error.to_string()))?;
        let expires = Instant::now() + deadline;
        let mut messages = Vec::new();
        while messages.len() < maximum && Instant::now() < expires {
            match consumer.poll(Duration::from_millis(100)) {
                Some(Ok(message)) => {
                    let key = message.key().map(<[u8]>::to_vec).unwrap_or_default();
                    let payload = message
                        .payload()
                        .map(<[u8]>::to_vec)
                        .ok_or_else(|| StreamingError::Delivery("empty payload".to_string()))?;
                    messages.push((key, payload));
                }
                Some(Err(error)) => return Err(StreamingError::Client(error.to_string())),
                None => {}
            }
        }
        Ok(messages)
    }
}
