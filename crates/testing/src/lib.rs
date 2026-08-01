//! Deterministic packet corpus and semantic-equivalence harness for Stage 1 ingest profiles.

mod binary_fixture;
mod binary_stream;
mod canonical_market_bar;
mod client_model;
mod direct_wire;
mod harness_error;
mod ingest_driver;
mod latest_state;
mod loopback_fixture;
mod market_bar_packet;
mod packet_corpus;
mod plain_loopback_lifecycle;
mod realtime_recovery;
mod replay_benchmark;
mod runtime_chart;
mod snapshot_chunk;
mod websocket_loopback;

pub use binary_stream::{BinaryMarketStreamConformance, run_binary_market_stream_conformance};
pub use client_model::{MarketBarClientModelConformance, run_market_bar_client_model_conformance};
pub use direct_wire::{DirectMarketBarWireConformance, run_direct_market_bar_wire_conformance};
pub use harness_error::{ConformanceHarnessError, FixtureDecodeError};
pub use ingest_driver::{
    ingest_outcomes_semantically_equivalent, run_ingest_conformance,
    run_ingest_conformance_after_start,
};
pub use latest_state::{LatestStateSnapshotConformance, run_latest_state_snapshot_conformance};
pub use market_bar_packet::{
    DeterministicMarketBarPacketCorpus, MarketBarPacketOriginConformance,
    deterministic_market_bar_packet_corpus,
    run_market_bar_packet_to_origin_conformance_after_start,
};
pub use packet_corpus::{
    ConformanceOutcome, FixtureProviderDecoder, FixtureTransport, deterministic_ingest_corpus,
};
pub use plain_loopback_lifecycle::{
    PlainLoopbackLifecycleConformance, run_plain_loopback_lifecycle_conformance,
};
pub use realtime_recovery::{
    RealtimeRecoveryReport, SnapshotIntegrityOutcome, run_realtime_recovery_conformance,
};
pub use runtime_chart::{
    PlainLoopbackRuntimeChartConformance, run_plain_loopback_runtime_chart_conformance,
};

pub use replay_benchmark::{
    ReplayBenchmarkEvidence, ReplayBenchmarkStage, ReplayBenchmarkStageReport,
    ReplayToGpuiBenchmarkReport, run_replay_to_gpui_host_benchmark,
};
pub use snapshot_chunk::{SnapshotChunkConformance, run_snapshot_chunk_conformance};
pub use websocket_loopback::{WebSocketLoopbackConformance, run_websocket_loopback_conformance};
