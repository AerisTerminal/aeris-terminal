//! QUIC prototype behind explicit profile negotiation (Section 8.1/8.2).
//!
//! One loopback `stream_quic` connection carries the negotiated profile: a
//! reliable control stream for profile and subscription lifecycle, a reliable
//! market stream for an ordered snapshot and deltas, and self-identifying
//! datagrams. Negotiation is explicit: an unsupported profile offer is rejected
//! and closed, never silently downgraded. The client pins the server's exact
//! self-signed certificate, and a client that pins a different certificate must
//! fail. This is a prototype behind negotiation, not a certified transport:
//! load, loss, migration, and the operating-system matrix are not exercised.

use quinn::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig, TransportConfig};
use serde::Serialize;
use std::{env, error::Error, fs, net::SocketAddr, path::Path, sync::Arc, time::Instant};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_2_quic_prototype";
const QUINN_VERSION: &str = "0.11.11";
const PROFILE_OFFER: &[u8] = b"offer stream_quic\n";
const PROFILE_ACCEPT: &[u8] = b"accept stream_quic\n";
const FOREIGN_OFFER: &[u8] = b"offer stream_websocket\n";
const PROFILE_REJECT: &[u8] = b"reject\n";
const SUBSCRIPTION_REQUEST: &[u8] = b"subscribe bars\n";
const DELTA_COUNT: usize = 8;
const DATAGRAM_COUNT: usize = 4;
const HANDSHAKE_SAMPLES: usize = 32;

#[derive(Serialize)]
struct NegotiationEvidence {
    supported_offer_accepted: &'static str,
    unsupported_offer_rejected: &'static str,
    no_silent_fallback: &'static str,
    certificate_pinning_enforced: &'static str,
}

#[derive(Serialize)]
struct StreamEvidence {
    control_stream: &'static str,
    reliable_market_stream: &'static str,
    ordered_byte_exact_delivery: &'static str,
}

#[derive(Serialize)]
struct DatagramEvidence {
    self_identifying_flows: &'static str,
    received_of_sent: usize,
}

#[derive(Serialize)]
struct HandshakeEvidence {
    samples: usize,
    p50_nanos: u64,
    p95_nanos: u64,
    maximum_nanos: u64,
}

#[derive(Serialize)]
struct QuicPrototypeReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    quinn_version: &'static str,
    negotiation: NegotiationEvidence,
    streams: StreamEvidence,
    datagrams: DatagramEvidence,
    handshake: HandshakeEvidence,
    zero_rtt: &'static str,
    certification: &'static str,
    limitations: [&'static str; 4],
}

/// Runs the prototype and writes one evidence artifact.
pub fn run(report_path: &Path) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for QUIC prototype evidence")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(prototype())?;
    let handshake_p50 = outcome.handshake.p50_nanos;
    let report = QuicPrototypeReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        quinn_version: QUINN_VERSION,
        negotiation: NegotiationEvidence {
            supported_offer_accepted: "passed",
            unsupported_offer_rejected: "passed",
            no_silent_fallback: "passed",
            certificate_pinning_enforced: "passed",
        },
        streams: StreamEvidence {
            control_stream: "passed",
            reliable_market_stream: "passed",
            ordered_byte_exact_delivery: "passed",
        },
        datagrams: DatagramEvidence {
            self_identifying_flows: "passed",
            received_of_sent: outcome.datagrams_received,
        },
        handshake: outcome.handshake,
        zero_rtt: "disabled",
        certification: "prototype_not_certified",
        limitations: [
            "loopback_only",
            "self_signed_pinned_certificate",
            "no_loss_or_migration_matrix",
            "no_load_benchmark",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "quic_prototype=passed negotiation=explicit streams=byte_exact datagrams={}/{} handshake_p50_ns={} zero_rtt=disabled certification=prototype_not_certified report={}",
        outcome.datagrams_received,
        DATAGRAM_COUNT,
        handshake_p50,
        report_path.display()
    );
    Ok(())
}

struct PrototypeOutcome {
    datagrams_received: usize,
    handshake: HandshakeEvidence,
}

async fn prototype() -> Result<PrototypeOutcome, Box<dyn Error>> {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let certificate = certified.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());

    let mut server_config = ServerConfig::with_single_cert(vec![certificate.clone()], key.into())?;
    server_config.transport = Arc::new(transport_config());
    let server = Endpoint::server(server_config, "127.0.0.1:0".parse::<SocketAddr>()?)?;
    let server_address = server.local_addr()?;

    let client_config = pinned_client_config(&certificate)?;
    let server_task = tokio::spawn(server_side(server));

    let mut handshake_nanos = Vec::with_capacity(HANDSHAKE_SAMPLES);
    let mut datagrams_received = 0_usize;
    for sample in 0..HANDSHAKE_SAMPLES {
        let mut client_endpoint = Endpoint::client("127.0.0.1:0".parse::<SocketAddr>()?)?;
        client_endpoint.set_default_client_config(client_config.clone());
        let started = Instant::now();
        let connection = client_endpoint
            .connect(server_address, "localhost")?
            .await
            .map_err(|error| format!("handshake sample {sample} failed: {error}"))?;
        handshake_nanos.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        if sample == 0 {
            datagrams_received = exercise_profile(&connection)
                .await
                .map_err(|error| format!("profile exercise failed: {error}"))?;
            verify_rejection(&client_endpoint, server_address)
                .await
                .map_err(|error| format!("rejection check failed: {error}"))?;
            verify_wrong_certificate_rejected(server_address)
                .await
                .map_err(|error| format!("certificate check failed: {error}"))?;
        }
        connection.close(0_u32.into(), b"done");
        client_endpoint.wait_idle().await;
    }
    server_task.abort();

    handshake_nanos.sort_unstable();
    let handshake = HandshakeEvidence {
        samples: handshake_nanos.len(),
        p50_nanos: percentile(&handshake_nanos, 50, 100),
        p95_nanos: percentile(&handshake_nanos, 95, 100),
        maximum_nanos: handshake_nanos.last().copied().unwrap_or(0),
    };
    Ok(PrototypeOutcome {
        datagrams_received,
        handshake,
    })
}

async fn exercise_profile(connection: &Connection) -> Result<usize, Box<dyn Error>> {
    let (mut send, mut receive) = connection.open_bi().await?;
    send.write_all(PROFILE_OFFER).await?;
    send.finish()?;
    let reply = receive.read_to_end(64).await?;
    if reply != PROFILE_ACCEPT {
        return Err("stream_quic negotiation was not accepted".into());
    }

    let (mut send, _) = connection.open_bi().await?;
    send.write_all(SUBSCRIPTION_REQUEST).await?;
    send.finish()?;

    let (_, mut market) = connection.accept_bi().await?;
    let expected = market_payload();
    let received = market.read_to_end(expected.len()).await?;
    if received != expected {
        return Err("reliable market stream delivery diverged".into());
    }

    let mut received_datagrams = 0_usize;
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while received_datagrams < DATAGRAM_COUNT && Instant::now() < deadline {
        let datagram = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            connection.read_datagram(),
        )
        .await;
        match datagram {
            Ok(Ok(bytes)) => {
                verify_datagram(&bytes, received_datagrams)?;
                received_datagrams += 1;
            }
            Ok(Err(error)) => return Err(error.to_string().into()),
            Err(_) => break,
        }
    }
    if received_datagrams == 0 {
        return Err("no datagrams arrived on the loopback prototype".into());
    }
    Ok(received_datagrams)
}

async fn verify_rejection(
    client_endpoint: &Endpoint,
    server_address: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    let connection = client_endpoint
        .connect(server_address, "localhost")?
        .await?;
    let (mut send, mut receive) = connection.open_bi().await?;
    send.write_all(FOREIGN_OFFER).await?;
    send.finish()?;
    let reply = receive.read_to_end(64).await?;
    if reply != PROFILE_REJECT {
        return Err("an unsupported profile offer was not explicitly rejected".into());
    }
    connection.close(0_u32.into(), b"rejected");
    Ok(())
}

async fn verify_wrong_certificate_rejected(
    server_address: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    let foreign = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let foreign_config = pinned_client_config(foreign.cert.der())?;
    let mut foreign_endpoint = Endpoint::client("127.0.0.1:0".parse::<SocketAddr>()?)?;
    foreign_endpoint.set_default_client_config(foreign_config);
    match foreign_endpoint.connect(server_address, "localhost")?.await {
        Ok(_) => Err("a client pinning a foreign certificate connected".into()),
        Err(_) => Ok(()),
    }
}

async fn server_side(server: Endpoint) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(incoming) = server.accept().await {
        tokio::spawn(async move {
            if let Ok(connection) = incoming.await {
                let _ = serve_connection(connection).await;
            }
        });
    }
    Ok(())
}

async fn serve_connection(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    loop {
        let Ok((mut send, mut receive)) = connection.accept_bi().await else {
            return Ok(());
        };
        let offer = receive.read_to_end(64).await?;
        if offer == PROFILE_OFFER {
            send.write_all(PROFILE_ACCEPT).await?;
            send.finish()?;
        } else {
            send.write_all(PROFILE_REJECT).await?;
            send.finish()?;
            connection.closed().await;
            return Ok(());
        }

        let Ok((_subscription_send, mut subscription_receive)) = connection.accept_bi().await
        else {
            return Ok(());
        };
        let subscription = subscription_receive.read_to_end(64).await?;
        if subscription != SUBSCRIPTION_REQUEST {
            connection.close(2_u32.into(), b"unknown subscription");
            return Ok(());
        }
        let (mut market_send, _) = connection.open_bi().await?;
        let payload = market_payload();
        market_send.write_all(&payload).await?;
        market_send.finish()?;
        for index in 0..DATAGRAM_COUNT {
            connection.send_datagram(datagram_for(index).into())?;
        }
    }
}

fn market_payload() -> Vec<u8> {
    let mut payload = b"snapshot:64-bars\n".to_vec();
    for index in 0..DELTA_COUNT {
        payload.extend_from_slice(format!("delta:{index}\n").as_bytes());
    }
    payload
}

fn datagram_for(index: usize) -> Vec<u8> {
    format!("flow:1 subscription:bars sequence:{index} schema:1 class:state_replace").into_bytes()
}

fn verify_datagram(bytes: &[u8], expected_index: usize) -> Result<(), Box<dyn Error>> {
    if bytes != datagram_for(expected_index) {
        return Err("datagram content diverged".into());
    }
    Ok(())
}

fn transport_config() -> TransportConfig {
    let mut config = TransportConfig::default();
    config.datagram_receive_buffer_size(Some(65_536));
    config.datagram_send_buffer_size(65_536);
    config
}

fn pinned_client_config(certificate: &CertificateDer<'_>) -> Result<ClientConfig, Box<dyn Error>> {
    let mut roots = quinn::rustls::RootCertStore::empty();
    roots.add(certificate.clone())?;
    let rustls = quinn::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(rustls)?,
    ));
    config.transport_config(Arc::new(transport_config()));
    Ok(config)
}

fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * numerator).div_ceil(denominator);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}
