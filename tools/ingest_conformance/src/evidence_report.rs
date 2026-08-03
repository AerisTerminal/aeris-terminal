use axiusflow_testing::{
    ConformanceOutcome, MarketBarPacketOriginConformance, ReplayBenchmarkStageReport,
    ReplayToGpuiBenchmarkReport,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    env,
    error::Error,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

const EVIDENCE_SCHEMA_VERSION: u32 = 5;
const EVIDENCE_SCOPE: &str = "stage_1_software_conformance";
const GENERIC_CORPUS_OUTCOMES: usize = 6;
const GENERIC_CORPUS_ACCEPTED_EVENTS: usize = 2;
const MARKET_BAR_CORPUS_OUTCOMES: usize = 7;
const MARKET_BAR_CORPUS_ACCEPTED_EVENTS: usize = 3;
const MARKET_BAR_ORIGIN_LAST_SEQUENCE: u64 = 3;
const MARKET_BAR_PARTITION_FANOUT_LAST_SEQUENCE: u64 = 3;
const SOFTWARE_FIXTURE_TARGETS: usize = 6;
const BENCHMARK_WARMUP_ITERATIONS: usize = 1;
const BENCHMARK_MEASUREMENT_ITERATIONS: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceState {
    Passed,
    NotApplicable,
    ExplicitlyUnavailable,
    NotExercised,
    NotMeasured,
    NotClaimed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReadinessEvidence {
    ContractOnly,
    Implemented,
    FixtureValidated,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ActiveModeEvidence {
    PortableSocket,
    TunedLinuxSocket,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceLimitation {
    AfXdpNativeLifecycleNotExercised,
    DpdkNativeLifecycleNotExercised,
    RendererSubmissionNotMeasured,
    PhysicalPresentationNotMeasured,
    HardwareAndProviderNotClaimed,
}

#[derive(Deserialize, Serialize)]
struct TargetEvidence {
    os: String,
    architecture: String,
    family: String,
}

#[derive(Deserialize, Serialize)]
struct PortableProfileEvidence {
    readiness: ReadinessEvidence,
    active_mode: ActiveModeEvidence,
    native_loopback: EvidenceState,
    semantic_equivalence: EvidenceState,
    lifecycle_and_overflow: EvidenceState,
    packet_to_origin_equivalence: EvidenceState,
    packet_partition_fanout_origin_equivalence: EvidenceState,
}

#[derive(Deserialize, Serialize)]
struct TunedLinuxProfileEvidence {
    readiness: ReadinessEvidence,
    active_mode: ActiveModeEvidence,
    native_loopback: EvidenceState,
    semantic_equivalence: EvidenceState,
    lifecycle_and_overflow: EvidenceState,
    packet_to_origin_equivalence: EvidenceState,
    packet_partition_fanout_origin_equivalence: EvidenceState,
}

#[derive(Deserialize, Serialize)]
struct AcceleratedProfileEvidence {
    readiness: ReadinessEvidence,
    active_mode: ActiveModeEvidence,
    activation: EvidenceState,
    native_lifecycle: EvidenceState,
    hardware: EvidenceState,
}

#[derive(Deserialize, Serialize)]
struct ProfileEvidence {
    portable_socket: PortableProfileEvidence,
    tuned_linux_socket: TunedLinuxProfileEvidence,
    linux_af_xdp: AcceleratedProfileEvidence,
    linux_dpdk: AcceleratedProfileEvidence,
}

#[derive(Deserialize, Serialize)]
struct BoundaryEvidence {
    deterministic_packet_corpus: EvidenceState,
    fixture_target_equivalence: EvidenceState,
    latency_recorder: EvidenceState,
    replay_to_gpui_host: EvidenceState,
    same_corpus_portable_to_origin: EvidenceState,
    same_corpus_tuned_linux_to_origin: EvidenceState,
    same_corpus_portable_partition_fanout_origin: EvidenceState,
    same_corpus_tuned_linux_partition_fanout_origin: EvidenceState,
    renderer_submission: EvidenceState,
    physical_presentation: EvidenceState,
}

#[derive(Deserialize, Serialize)]
struct ClaimEvidence {
    connected_live: EvidenceState,
    zero_copy: EvidenceState,
    hardware: EvidenceState,
    provider: EvidenceState,
    production: EvidenceState,
}

#[derive(Deserialize, Serialize)]
struct CorpusEvidence {
    outcomes: usize,
    accepted_canonical_events: usize,
    market_bar_packet_outcomes: usize,
    market_bar_packet_accepted_canonical_events: usize,
    market_bar_packet_origin_last_source_sequence: u64,
    market_bar_packet_partition_fanout_last_source_sequence: u64,
    actual_native_targets: usize,
    software_fixture_targets: usize,
}

#[derive(Deserialize, Serialize)]
struct BenchmarkStageEvidence {
    samples: usize,
    p50_nanos: u64,
    p95_nanos: u64,
    p99_nanos: u64,
    p99_9_nanos: u64,
    maximum_nanos: u64,
}

#[derive(Deserialize, Serialize)]
struct BenchmarkEvidence {
    warmup_iterations: usize,
    measurement_iterations: usize,
    decoder_and_model: BenchmarkStageEvidence,
    origin_frame_construction: BenchmarkStageEvidence,
    gpui_host_preparation: BenchmarkStageEvidence,
}

#[derive(Deserialize, Serialize)]
struct Stage1EvidenceReport {
    schema_version: u32,
    evidence_scope: String,
    source_revision: Option<String>,
    target: TargetEvidence,
    profiles: ProfileEvidence,
    boundaries: BoundaryEvidence,
    corpus: CorpusEvidence,
    benchmark: BenchmarkEvidence,
    claims: ClaimEvidence,
    limitations: [EvidenceLimitation; 5],
}

pub enum EvidenceCommand {
    Run {
        report_path: Option<PathBuf>,
    },
    VerifySet {
        directory: PathBuf,
    },
    AfXdpCopy {
        receive_interface: String,
        transmit_interface: String,
        report_path: PathBuf,
    },
    AfXdpCopyFuzz {
        receive_interface: String,
        transmit_interface: String,
        seed: u64,
        rounds: u32,
        report_path: PathBuf,
    },
    DpdkVdevLifecycle {
        report_path: PathBuf,
    },
    RedpandaDurableBranch {
        brokers: String,
        report_path: PathBuf,
    },
    S3RawCapture {
        host: String,
        port: u16,
        access_key: String,
        secret_key: String,
        report_path: PathBuf,
    },
    ClickHouseProjections {
        host: String,
        port: u16,
        report_path: PathBuf,
    },
    PostgresPersistence {
        host: String,
        port: u16,
        user: String,
        password: String,
        database: String,
        report_path: PathBuf,
    },
    QuicPrototype {
        report_path: PathBuf,
    },
    AuthorizationBoundary {
        service_binary: PathBuf,
        report_path: PathBuf,
    },
    AuthService {
        service_binary: PathBuf,
        pg_host: String,
        pg_port: u16,
        pg_user: String,
        pg_password: String,
        pg_database: String,
        report_path: PathBuf,
    },
    CoinbaseLive {
        products: Vec<String>,
        window_seconds: u64,
        report_path: PathBuf,
    },
    LiveDataPlane {
        plane_address: String,
        product: String,
        window_seconds: u64,
        report_path: PathBuf,
    },
    FeedProfileMatrix {
        live_provider: String,
        report_path: PathBuf,
    },
}

#[derive(Clone, Copy)]
struct RequiredArtifact {
    file_name: &'static str,
    os: &'static str,
    family: &'static str,
    tuned_linux_native: bool,
}

const REQUIRED_ARTIFACTS: [RequiredArtifact; 3] = [
    RequiredArtifact {
        file_name: "stage_1_evidence_Windows.json",
        os: "windows",
        family: "windows",
        tuned_linux_native: false,
    },
    RequiredArtifact {
        file_name: "stage_1_evidence_macOS.json",
        os: "macos",
        family: "unix",
        tuned_linux_native: false,
    },
    RequiredArtifact {
        file_name: "stage_1_evidence_Linux.json",
        os: "linux",
        family: "unix",
        tuned_linux_native: true,
    },
];

pub fn requested_command() -> Result<EvidenceCommand, Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(EvidenceCommand::Run { report_path: None });
    };
    if argument == OsStr::new("--af-xdp-copy-conformance") {
        return af_xdp_copy_conformance_command(&mut arguments);
    }
    if argument == OsStr::new("--af-xdp-copy-fuzz") {
        return af_xdp_copy_fuzz_command(&mut arguments);
    }
    if argument == OsStr::new("--feed-profile-matrix") {
        let live_provider = required_argument(&mut arguments, "live provider")?;
        let report_path = required_argument(&mut arguments, "report path")?;
        reject_extra(&mut arguments)?;
        return Ok(EvidenceCommand::FeedProfileMatrix {
            live_provider: live_provider.to_string_lossy().into_owned(),
            report_path: PathBuf::from(report_path),
        });
    }
    if argument == OsStr::new("--live-data-plane") {
        return live_data_plane_command(&mut arguments);
    }
    if argument == OsStr::new("--coinbase-live") {
        return coinbase_live_command(&mut arguments);
    }
    if argument == OsStr::new("--auth-service") {
        return auth_service_command(&mut arguments);
    }
    if argument == OsStr::new("--authorization-boundary") {
        return authorization_boundary_command(&mut arguments);
    }
    if argument == OsStr::new("--quic-prototype") {
        let report_path = required_argument(&mut arguments, "report path")?;
        reject_extra(&mut arguments)?;
        return Ok(EvidenceCommand::QuicPrototype {
            report_path: PathBuf::from(report_path),
        });
    }
    if argument == OsStr::new("--postgres-persistence") {
        return postgres_persistence_command(&mut arguments);
    }
    if argument == OsStr::new("--clickhouse-projections") {
        let host = required_argument(&mut arguments, "host")?;
        let port = required_argument(&mut arguments, "port")?
            .to_string_lossy()
            .parse::<u16>()
            .map_err(|error| boxed_error(format!("invalid ClickHouse port: {error}")))?;
        let report_path = required_argument(&mut arguments, "report path")?;
        reject_extra(&mut arguments)?;
        return Ok(EvidenceCommand::ClickHouseProjections {
            host: host.to_string_lossy().into_owned(),
            port,
            report_path: PathBuf::from(report_path),
        });
    }
    if argument == OsStr::new("--s3-raw-capture") {
        return s3_raw_capture_command(&mut arguments);
    }
    if argument == OsStr::new("--redpanda-durable-branch") {
        let brokers = required_argument(&mut arguments, "bootstrap servers")?;
        let report_path = required_argument(&mut arguments, "report path")?;
        reject_extra(&mut arguments)?;
        return Ok(EvidenceCommand::RedpandaDurableBranch {
            brokers: brokers.to_string_lossy().into_owned(),
            report_path: PathBuf::from(report_path),
        });
    }
    if argument == OsStr::new("--dpdk-vdev-lifecycle") {
        let report_path = required_argument(&mut arguments, "report path")?;
        reject_extra(&mut arguments)?;
        return Ok(EvidenceCommand::DpdkVdevLifecycle {
            report_path: PathBuf::from(report_path),
        });
    }
    let path = required_argument(&mut arguments, "path")?;
    if let Some(extra) = arguments.next() {
        return Err(boxed_error(format!(
            "unexpected argument: {}",
            extra.to_string_lossy()
        )));
    }
    if argument == OsStr::new("--evidence-report") {
        Ok(EvidenceCommand::Run {
            report_path: Some(PathBuf::from(path)),
        })
    } else if argument == OsStr::new("--verify-evidence-set") {
        Ok(EvidenceCommand::VerifySet {
            directory: PathBuf::from(path),
        })
    } else {
        Err(boxed_error(format!(
            "unsupported argument: {}",
            argument.to_string_lossy()
        )))
    }
}

fn af_xdp_copy_conformance_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let receive_interface = required_argument(arguments, "receive interface")?;
    let transmit_interface = required_argument(arguments, "transmit interface")?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::AfXdpCopy {
        receive_interface: receive_interface.to_string_lossy().into_owned(),
        transmit_interface: transmit_interface.to_string_lossy().into_owned(),
        report_path: PathBuf::from(report_path),
    })
}

fn af_xdp_copy_fuzz_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let receive_interface = required_argument(arguments, "receive interface")?;
    let transmit_interface = required_argument(arguments, "transmit interface")?;
    let seed = required_argument(arguments, "seed")?
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|error| boxed_error(format!("invalid AF_XDP fuzz seed: {error}")))?;
    let rounds = required_argument(arguments, "rounds")?
        .to_string_lossy()
        .parse::<u32>()
        .map_err(|error| boxed_error(format!("invalid AF_XDP fuzz rounds: {error}")))?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::AfXdpCopyFuzz {
        receive_interface: receive_interface.to_string_lossy().into_owned(),
        transmit_interface: transmit_interface.to_string_lossy().into_owned(),
        seed,
        rounds,
        report_path: PathBuf::from(report_path),
    })
}

fn live_data_plane_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let plane_address = required_argument(arguments, "plane address")?;
    let product = required_argument(arguments, "product")?;
    let window_seconds = required_argument(arguments, "window seconds")?
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|error| boxed_error(format!("invalid window seconds: {error}")))?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::LiveDataPlane {
        plane_address: plane_address.to_string_lossy().into_owned(),
        product: product.to_string_lossy().into_owned(),
        window_seconds,
        report_path: PathBuf::from(report_path),
    })
}

fn coinbase_live_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let products = required_argument(arguments, "products (comma-separated)")?
        .to_string_lossy()
        .split(',')
        .map(str::to_string)
        .collect();
    let window_seconds = required_argument(arguments, "window seconds")?
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|error| boxed_error(format!("invalid window seconds: {error}")))?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::CoinbaseLive {
        products,
        window_seconds,
        report_path: PathBuf::from(report_path),
    })
}

fn auth_service_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let service_binary = required_argument(arguments, "service binary")?;
    let pg_host = required_argument(arguments, "postgres host")?;
    let pg_port = required_argument(arguments, "postgres port")?
        .to_string_lossy()
        .parse::<u16>()
        .map_err(|error| boxed_error(format!("invalid PostgreSQL port: {error}")))?;
    let pg_user = required_argument(arguments, "postgres user")?;
    let pg_password = required_argument(arguments, "postgres password")?;
    let pg_database = required_argument(arguments, "postgres database")?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::AuthService {
        service_binary: PathBuf::from(service_binary),
        pg_host: pg_host.to_string_lossy().into_owned(),
        pg_port,
        pg_user: pg_user.to_string_lossy().into_owned(),
        pg_password: pg_password.to_string_lossy().into_owned(),
        pg_database: pg_database.to_string_lossy().into_owned(),
        report_path: PathBuf::from(report_path),
    })
}

fn authorization_boundary_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let service_binary = required_argument(arguments, "service binary")?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::AuthorizationBoundary {
        service_binary: PathBuf::from(service_binary),
        report_path: PathBuf::from(report_path),
    })
}

fn postgres_persistence_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let host = required_argument(arguments, "host")?;
    let port = required_argument(arguments, "port")?
        .to_string_lossy()
        .parse::<u16>()
        .map_err(|error| boxed_error(format!("invalid PostgreSQL port: {error}")))?;
    let user = required_argument(arguments, "user")?;
    let password = required_argument(arguments, "password")?;
    let database = required_argument(arguments, "database")?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::PostgresPersistence {
        host: host.to_string_lossy().into_owned(),
        port,
        user: user.to_string_lossy().into_owned(),
        password: password.to_string_lossy().into_owned(),
        database: database.to_string_lossy().into_owned(),
        report_path: PathBuf::from(report_path),
    })
}

fn s3_raw_capture_command(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<EvidenceCommand, Box<dyn Error>> {
    let host = required_argument(arguments, "host")?;
    let port = required_argument(arguments, "port")?
        .to_string_lossy()
        .parse::<u16>()
        .map_err(|error| boxed_error(format!("invalid S3 capture port: {error}")))?;
    let access_key = required_argument(arguments, "access key")?;
    let secret_key = required_argument(arguments, "secret key")?;
    let report_path = required_argument(arguments, "report path")?;
    reject_extra(arguments)?;
    Ok(EvidenceCommand::S3RawCapture {
        host: host.to_string_lossy().into_owned(),
        port,
        access_key: access_key.to_string_lossy().into_owned(),
        secret_key: secret_key.to_string_lossy().into_owned(),
        report_path: PathBuf::from(report_path),
    })
}

fn reject_extra(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    if let Some(extra) = arguments.next() {
        return Err(boxed_error(format!(
            "unexpected argument: {}",
            extra.to_string_lossy()
        )));
    }
    Ok(())
}

fn required_argument(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    description: &str,
) -> Result<std::ffi::OsString, Box<dyn Error>> {
    let value = arguments
        .next()
        .ok_or_else(|| boxed_error(format!("missing {description}")))?;
    if value.is_empty() {
        return Err(boxed_error(format!("{description} cannot be empty")));
    }
    Ok(value)
}

pub fn write(
    path: Option<&Path>,
    tuned_linux_native: bool,
    tuned_linux_market_bar_origin: bool,
    outcomes: &[ConformanceOutcome],
    market_bar_packet_origin: &MarketBarPacketOriginConformance,
    benchmark: &ReplayToGpuiBenchmarkReport,
) -> Result<(), Box<dyn Error>> {
    let Some(path) = path else {
        return Ok(());
    };
    let report = build_report(
        tuned_linux_native,
        tuned_linux_market_bar_origin,
        outcomes,
        market_bar_packet_origin,
        benchmark,
    );
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(path, encoded)?;
    println!("stage_1_evidence_report={}", path.display());
    Ok(())
}

pub fn verify_set(directory: &Path) -> Result<(), Box<dyn Error>> {
    let expected_names = REQUIRED_ARTIFACTS
        .iter()
        .map(|required| required.file_name.to_string())
        .collect::<BTreeSet<_>>();
    let observed_names = observed_report_names(directory)?;
    if observed_names != expected_names {
        return Err(boxed_error(format!(
            "cross-platform evidence files differed: expected {expected_names:?}, observed {observed_names:?}"
        )));
    }

    let mut reports = Vec::with_capacity(REQUIRED_ARTIFACTS.len());
    for required in REQUIRED_ARTIFACTS {
        let path = directory.join(required.file_name);
        let encoded = fs::read(&path).map_err(|error| {
            boxed_error(format!(
                "failed to read required evidence {}: {error}",
                path.display()
            ))
        })?;
        let report: Stage1EvidenceReport = serde_json::from_slice(&encoded).map_err(|error| {
            boxed_error(format!(
                "failed to decode required evidence {}: {error}",
                path.display()
            ))
        })?;
        validate_report(&report, required)
            .map_err(|error| boxed_error(format!("{}: {error}", path.display())))?;
        reports.push(report);
    }
    let source_revision = validate_source_revisions(&reports)?;
    println!(
        "stage_1_cross_platform_evidence=passed schema_version={EVIDENCE_SCHEMA_VERSION} source_revision={source_revision} targets=windows,macos,linux portable_fixture_evidence=passed portable_readiness=fixture_validated accelerated_readiness_unchanged=true hardware_claims=false provider_claims=false production_claims=false"
    );
    Ok(())
}

fn observed_report_names(directory: &Path) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let entries = fs::read_dir(directory).map_err(|error| {
        boxed_error(format!(
            "failed to read evidence directory {}: {error}",
            directory.display()
        ))
    })?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| boxed_error(error.to_string()))?;
        if !entry
            .file_type()
            .map_err(|error| boxed_error(error.to_string()))?
            .is_file()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_json = Path::new(&name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if name.starts_with("stage_1_evidence_") && is_json {
            names.insert(name);
        }
    }
    Ok(names)
}

fn validate_report(
    report: &Stage1EvidenceReport,
    required: RequiredArtifact,
) -> Result<(), String> {
    require(
        report.schema_version == EVIDENCE_SCHEMA_VERSION,
        "schema_version did not match",
    )?;
    require(
        report.evidence_scope == EVIDENCE_SCOPE,
        "evidence_scope did not match",
    )?;
    require(
        report
            .source_revision
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "source_revision was absent or empty",
    )?;
    require(report.target.os == required.os, "target.os did not match")?;
    require(
        report.target.family == required.family,
        "target.family did not match",
    )?;
    require(
        !report.target.architecture.is_empty(),
        "target.architecture was empty",
    )?;
    validate_profiles(&report.profiles, required.tuned_linux_native)?;
    validate_boundaries(&report.boundaries, required.tuned_linux_native)?;
    validate_corpus(&report.corpus, required.tuned_linux_native)?;
    validate_benchmark(&report.benchmark)?;
    validate_claims_and_limitations(&report.claims, report.limitations)?;
    Ok(())
}

fn validate_profiles(profiles: &ProfileEvidence, tuned_linux_native: bool) -> Result<(), String> {
    let portable = &profiles.portable_socket;
    require(
        portable.readiness == ReadinessEvidence::FixtureValidated,
        "portable readiness did not match the cross-platform fixture evidence",
    )?;
    require(
        portable.active_mode == ActiveModeEvidence::PortableSocket,
        "portable active mode was not portable_socket",
    )?;
    require(
        portable.native_loopback == EvidenceState::Passed
            && portable.semantic_equivalence == EvidenceState::Passed
            && portable.lifecycle_and_overflow == EvidenceState::Passed
            && portable.packet_to_origin_equivalence == EvidenceState::Passed
            && portable.packet_partition_fanout_origin_equivalence == EvidenceState::Passed,
        "portable software evidence was incomplete",
    )?;

    let tuned = &profiles.tuned_linux_socket;
    require(
        tuned.readiness == ReadinessEvidence::FixtureValidated,
        "tuned Linux readiness did not match the manifest",
    )?;
    let expected_mode = if tuned_linux_native {
        ActiveModeEvidence::TunedLinuxSocket
    } else {
        ActiveModeEvidence::Unavailable
    };
    let expected_state = if tuned_linux_native {
        EvidenceState::Passed
    } else {
        EvidenceState::NotApplicable
    };
    require(
        tuned.active_mode == expected_mode
            && tuned.native_loopback == expected_state
            && tuned.semantic_equivalence == expected_state
            && tuned.lifecycle_and_overflow == expected_state
            && tuned.packet_to_origin_equivalence == expected_state
            && tuned.packet_partition_fanout_origin_equivalence == expected_state,
        "tuned Linux target-specific evidence was inconsistent",
    )?;
    validate_accelerated_profile(
        "linux_af_xdp",
        &profiles.linux_af_xdp,
        ReadinessEvidence::Implemented,
    )?;
    validate_accelerated_profile(
        "linux_dpdk",
        &profiles.linux_dpdk,
        ReadinessEvidence::ContractOnly,
    )?;
    Ok(())
}

fn validate_accelerated_profile(
    name: &str,
    profile: &AcceleratedProfileEvidence,
    expected_readiness: ReadinessEvidence,
) -> Result<(), String> {
    require(
        profile.readiness == expected_readiness
            && profile.active_mode == ActiveModeEvidence::Unavailable
            && profile.activation == EvidenceState::ExplicitlyUnavailable
            && profile.native_lifecycle == EvidenceState::NotExercised
            && profile.hardware == EvidenceState::NotClaimed,
        &format!("{name} evidence overclaimed or changed"),
    )
}

fn validate_boundaries(
    boundaries: &BoundaryEvidence,
    tuned_linux_native: bool,
) -> Result<(), String> {
    let tuned_origin_state = if tuned_linux_native {
        EvidenceState::Passed
    } else {
        EvidenceState::NotApplicable
    };
    require(
        boundaries.deterministic_packet_corpus == EvidenceState::Passed
            && boundaries.fixture_target_equivalence == EvidenceState::Passed
            && boundaries.latency_recorder == EvidenceState::Passed
            && boundaries.replay_to_gpui_host == EvidenceState::Passed
            && boundaries.same_corpus_portable_to_origin == EvidenceState::Passed
            && boundaries.same_corpus_tuned_linux_to_origin == tuned_origin_state
            && boundaries.same_corpus_portable_partition_fanout_origin == EvidenceState::Passed
            && boundaries.same_corpus_tuned_linux_partition_fanout_origin == tuned_origin_state,
        "software boundary evidence was incomplete or target-inapplicable evidence was overclaimed",
    )?;
    require(
        boundaries.renderer_submission == EvidenceState::NotMeasured
            && boundaries.physical_presentation == EvidenceState::NotMeasured,
        "renderer or physical-presentation evidence was overclaimed",
    )
}

fn validate_corpus(corpus: &CorpusEvidence, tuned_linux_native: bool) -> Result<(), String> {
    require(
        corpus.outcomes == GENERIC_CORPUS_OUTCOMES
            && corpus.accepted_canonical_events == GENERIC_CORPUS_ACCEPTED_EVENTS
            && corpus.market_bar_packet_outcomes == MARKET_BAR_CORPUS_OUTCOMES
            && corpus.market_bar_packet_accepted_canonical_events
                == MARKET_BAR_CORPUS_ACCEPTED_EVENTS
            && corpus.market_bar_packet_origin_last_source_sequence
                == MARKET_BAR_ORIGIN_LAST_SEQUENCE
            && corpus.market_bar_packet_partition_fanout_last_source_sequence
                == MARKET_BAR_PARTITION_FANOUT_LAST_SEQUENCE
            && corpus.software_fixture_targets == SOFTWARE_FIXTURE_TARGETS,
        "corpus counts or packet/partition/fanout/Origin sequence did not match",
    )?;
    let expected_native_targets = usize::from(tuned_linux_native).saturating_add(1);
    require(
        corpus.actual_native_targets == expected_native_targets,
        "actual_native_targets did not match the target",
    )
}

fn validate_benchmark(benchmark: &BenchmarkEvidence) -> Result<(), String> {
    require(
        benchmark.warmup_iterations == BENCHMARK_WARMUP_ITERATIONS
            && benchmark.measurement_iterations == BENCHMARK_MEASUREMENT_ITERATIONS,
        "benchmark iteration counts did not match",
    )?;
    for (name, stage) in [
        ("decoder_and_model", &benchmark.decoder_and_model),
        (
            "origin_frame_construction",
            &benchmark.origin_frame_construction,
        ),
        ("gpui_host_preparation", &benchmark.gpui_host_preparation),
    ] {
        require(
            stage.samples == benchmark.measurement_iterations
                && stage.p50_nanos <= stage.p95_nanos
                && stage.p95_nanos <= stage.p99_nanos
                && stage.p99_nanos <= stage.p99_9_nanos
                && stage.p99_9_nanos <= stage.maximum_nanos
                && stage.maximum_nanos > 0,
            &format!("{name} benchmark samples or percentiles were invalid"),
        )?;
    }
    Ok(())
}

fn validate_claims_and_limitations(
    claims: &ClaimEvidence,
    limitations: [EvidenceLimitation; 5],
) -> Result<(), String> {
    require(
        claims.connected_live == EvidenceState::NotClaimed
            && claims.zero_copy == EvidenceState::NotClaimed
            && claims.hardware == EvidenceState::NotClaimed
            && claims.provider == EvidenceState::NotClaimed
            && claims.production == EvidenceState::NotClaimed,
        "live, zero-copy, hardware, provider, or production evidence was overclaimed",
    )?;
    require(
        limitations
            == [
                EvidenceLimitation::AfXdpNativeLifecycleNotExercised,
                EvidenceLimitation::DpdkNativeLifecycleNotExercised,
                EvidenceLimitation::RendererSubmissionNotMeasured,
                EvidenceLimitation::PhysicalPresentationNotMeasured,
                EvidenceLimitation::HardwareAndProviderNotClaimed,
            ],
        "limitations did not match the honest evidence scope",
    )
}

fn validate_source_revisions(reports: &[Stage1EvidenceReport]) -> Result<String, Box<dyn Error>> {
    let revision = reports
        .first()
        .and_then(|report| report.source_revision.as_deref())
        .filter(|revision| !revision.is_empty())
        .ok_or_else(|| boxed_error("first source_revision was absent".to_string()))?;
    if reports
        .iter()
        .any(|report| report.source_revision.as_deref() != Some(revision))
    {
        return Err(boxed_error(
            "cross-platform source_revision values differed".to_string(),
        ));
    }
    if let Ok(expected) = env::var("GITHUB_SHA")
        && !expected.is_empty()
        && revision != expected
    {
        return Err(boxed_error(format!(
            "artifact revision {revision} did not match GITHUB_SHA {expected}"
        )));
    }
    Ok(revision.to_string())
}

fn build_report(
    tuned_linux_native: bool,
    tuned_linux_market_bar_origin: bool,
    outcomes: &[ConformanceOutcome],
    market_bar_packet_origin: &MarketBarPacketOriginConformance,
    benchmark: &ReplayToGpuiBenchmarkReport,
) -> Stage1EvidenceReport {
    let tuned_state = if tuned_linux_native {
        EvidenceState::Passed
    } else {
        EvidenceState::NotApplicable
    };
    let tuned_origin_state = if tuned_linux_market_bar_origin {
        EvidenceState::Passed
    } else {
        EvidenceState::NotApplicable
    };
    let tuned_mode = if tuned_linux_native {
        ActiveModeEvidence::TunedLinuxSocket
    } else {
        ActiveModeEvidence::Unavailable
    };
    let accepted_canonical_events = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, ConformanceOutcome::Accepted(_)))
        .count();
    Stage1EvidenceReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE.to_string(),
        source_revision: env::var("GITHUB_SHA")
            .ok()
            .filter(|revision| !revision.is_empty()),
        target: TargetEvidence {
            os: env::consts::OS.to_string(),
            architecture: env::consts::ARCH.to_string(),
            family: env::consts::FAMILY.to_string(),
        },
        profiles: ProfileEvidence {
            portable_socket: PortableProfileEvidence {
                readiness: ReadinessEvidence::FixtureValidated,
                active_mode: ActiveModeEvidence::PortableSocket,
                native_loopback: EvidenceState::Passed,
                semantic_equivalence: EvidenceState::Passed,
                lifecycle_and_overflow: EvidenceState::Passed,
                packet_to_origin_equivalence: EvidenceState::Passed,
                packet_partition_fanout_origin_equivalence: EvidenceState::Passed,
            },
            tuned_linux_socket: TunedLinuxProfileEvidence {
                readiness: ReadinessEvidence::FixtureValidated,
                active_mode: tuned_mode,
                native_loopback: tuned_state,
                semantic_equivalence: tuned_state,
                lifecycle_and_overflow: tuned_state,
                packet_to_origin_equivalence: tuned_origin_state,
                packet_partition_fanout_origin_equivalence: tuned_origin_state,
            },
            linux_af_xdp: unavailable_accelerated_profile(ReadinessEvidence::Implemented),
            linux_dpdk: unavailable_accelerated_profile(ReadinessEvidence::ContractOnly),
        },
        boundaries: BoundaryEvidence {
            deterministic_packet_corpus: EvidenceState::Passed,
            fixture_target_equivalence: EvidenceState::Passed,
            latency_recorder: EvidenceState::Passed,
            replay_to_gpui_host: EvidenceState::Passed,
            same_corpus_portable_to_origin: EvidenceState::Passed,
            same_corpus_tuned_linux_to_origin: tuned_origin_state,
            same_corpus_portable_partition_fanout_origin: EvidenceState::Passed,
            same_corpus_tuned_linux_partition_fanout_origin: tuned_origin_state,
            renderer_submission: EvidenceState::NotMeasured,
            physical_presentation: EvidenceState::NotMeasured,
        },
        corpus: CorpusEvidence {
            outcomes: outcomes.len(),
            accepted_canonical_events,
            market_bar_packet_outcomes: market_bar_packet_origin.outcome_count(),
            market_bar_packet_accepted_canonical_events: market_bar_packet_origin
                .accepted_canonical_event_count(),
            market_bar_packet_origin_last_source_sequence: market_bar_packet_origin
                .origin_last_source_sequence(),
            market_bar_packet_partition_fanout_last_source_sequence: market_bar_packet_origin
                .partition_fanout_last_source_sequence(),
            actual_native_targets: usize::from(tuned_linux_native).saturating_add(1),
            software_fixture_targets: SOFTWARE_FIXTURE_TARGETS,
        },
        benchmark: BenchmarkEvidence {
            warmup_iterations: benchmark.warmup_iterations,
            measurement_iterations: benchmark.measurement_iterations,
            decoder_and_model: benchmark_stage(&benchmark.decoder_and_model),
            origin_frame_construction: benchmark_stage(&benchmark.origin_frame),
            gpui_host_preparation: benchmark_stage(&benchmark.gpui_host),
        },
        claims: ClaimEvidence {
            connected_live: EvidenceState::NotClaimed,
            zero_copy: EvidenceState::NotClaimed,
            hardware: EvidenceState::NotClaimed,
            provider: EvidenceState::NotClaimed,
            production: EvidenceState::NotClaimed,
        },
        limitations: [
            EvidenceLimitation::AfXdpNativeLifecycleNotExercised,
            EvidenceLimitation::DpdkNativeLifecycleNotExercised,
            EvidenceLimitation::RendererSubmissionNotMeasured,
            EvidenceLimitation::PhysicalPresentationNotMeasured,
            EvidenceLimitation::HardwareAndProviderNotClaimed,
        ],
    }
}

fn unavailable_accelerated_profile(readiness: ReadinessEvidence) -> AcceleratedProfileEvidence {
    AcceleratedProfileEvidence {
        readiness,
        active_mode: ActiveModeEvidence::Unavailable,
        activation: EvidenceState::ExplicitlyUnavailable,
        native_lifecycle: EvidenceState::NotExercised,
        hardware: EvidenceState::NotClaimed,
    }
}

fn benchmark_stage(report: &ReplayBenchmarkStageReport) -> BenchmarkStageEvidence {
    BenchmarkStageEvidence {
        samples: report.sample_count,
        p50_nanos: report.p50_nanos,
        p95_nanos: report.p95_nanos,
        p99_nanos: report.p99_nanos,
        p99_9_nanos: report.p99_9_nanos,
        maximum_nanos: report.maximum_nanos,
    }
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_string())
    }
}

fn boxed_error(message: String) -> Box<dyn Error> {
    Box::new(io::Error::other(message))
}
