//! Live market data plane: Coinbase ingest to binary client streams.

mod aggregation;
mod backfill;
mod entitlement;
mod fanout;
mod instruments;
mod ownership_epoch;
mod server;

use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match bootstrap() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("axiusflow_market_data_plane fail-closed: {error}");
            ExitCode::FAILURE
        }
    }
}

struct PlaneArguments {
    listen: String,
    products: Vec<String>,
    jwks_path: Option<String>,
    policy_path: Option<String>,
    resnapshot_seconds: u64,
    environment: String,
    redpanda_brokers: Option<String>,
    ownership_state: Option<String>,
}

fn parse_arguments() -> Result<PlaneArguments, String> {
    let mut listen = None;
    let mut products = None;
    let mut jwks_path = None;
    let mut policy_path = None;
    let mut resnapshot_seconds = 30_u64;
    let mut environment = "dev".to_string();
    let mut redpanda_brokers = None;
    let mut ownership_state = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--listen" => {
                listen = Some(arguments.next().ok_or("--listen requires an address")?);
            }
            "--products" => {
                products = Some(
                    arguments
                        .next()
                        .ok_or("--products requires a comma-separated list")?,
                );
            }
            "--jwks" => {
                jwks_path = Some(arguments.next().ok_or("--jwks requires a path")?);
            }
            "--policy" => {
                policy_path = Some(arguments.next().ok_or("--policy requires a path")?);
            }
            "--resnapshot-seconds" => {
                resnapshot_seconds = arguments
                    .next()
                    .ok_or("--resnapshot-seconds requires a value")?
                    .parse()
                    .map_err(|_| "invalid --resnapshot-seconds")?;
            }
            "--environment" => {
                environment = arguments.next().ok_or("--environment requires a value")?;
            }
            "--redpanda-brokers" => {
                redpanda_brokers = Some(
                    arguments
                        .next()
                        .ok_or("--redpanda-brokers requires a value")?,
                );
            }
            "--ownership-state" => {
                ownership_state = Some(
                    arguments
                        .next()
                        .ok_or("--ownership-state requires a path")?,
                );
            }
            other => return Err(format!("unsupported argument: {other}")),
        }
    }
    Ok(PlaneArguments {
        listen: listen.ok_or("--listen is required")?,
        products: products
            .ok_or("--products is required")?
            .split(',')
            .map(str::to_string)
            .collect(),
        jwks_path,
        policy_path,
        resnapshot_seconds,
        environment,
        redpanda_brokers,
        ownership_state,
    })
}

/// The tungstenite handshake callback trait fixes one `Err` shape the
/// `result_large_err` lint cannot satisfy without reimplementing the handshake.
#[expect(clippy::result_large_err)]
fn bootstrap() -> Result<ExitCode, String> {
    let arguments = parse_arguments()?;
    let PlaneArguments {
        listen,
        products,
        jwks_path,
        policy_path,
        resnapshot_seconds,
        environment,
        redpanda_brokers,
        ownership_state,
    } = arguments;

    let entitlement = match (&jwks_path, &policy_path) {
        (Some(jwks), Some(policy)) => Some(load_entitlement(jwks, policy)?),
        (None, None) => None,
        _ => return Err("--jwks and --policy must be given together".to_string()),
    };
    let ownership_lease =
        reserve_ownership_epoch(redpanda_brokers.as_deref(), ownership_state.as_deref())?;
    let ownership_epoch = ownership_lease
        .as_ref()
        .map_or(1, ownership_epoch::OwnershipLease::epoch);
    let durable_tap = build_durable_tap(&environment, redpanda_brokers.as_deref())?;
    let plane = std::sync::Arc::new(server::MarketDataPlane::try_new_with_entitlement(
        &products,
        entitlement,
        &environment,
        ownership_epoch,
        durable_tap,
    )?);
    plane.start()?;
    start_resnapshot_supervisor(
        std::sync::Arc::clone(&plane),
        jwks_path.clone(),
        policy_path.clone(),
        resnapshot_seconds,
    );
    log_startup(&plane, &products);

    let listener = std::net::TcpListener::bind(&listen).map_err(|error| error.to_string())?;
    let shared = plane;
    for connection in listener.incoming() {
        let connection = connection.map_err(|error| error.to_string())?;
        let shared = std::sync::Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut requested_path = String::new();
            let mut requested_token: Option<String> = None;
            let denied = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
            let denied_for_callback = std::sync::Arc::clone(&denied);
            let mut callback =
                |request: &tungstenite::handshake::server::Request,
                 response: tungstenite::handshake::server::Response| {
                    let uri = request.uri().to_string();
                    let (path, query) = uri.split_once('?').unwrap_or((uri.as_str(), ""));
                    requested_path = path.to_string();
                    requested_token = query
                        .split('&')
                        .find_map(|part| part.strip_prefix("token="))
                        .map(str::to_string);
                    let product = path.trim_start_matches('/').to_string();
                    if let Err(error) =
                        shared.authorize_client(&product, requested_token.as_deref())
                    {
                        *denied_for_callback.lock().expect("denied lock poisoned") =
                            Some(format!("{error:?}"));
                        let rejection = tungstenite::http::Response::builder()
                            .status(403)
                            .body(Some(format!("entitlement denied: {error:?}")))
                            .expect("a static 403 response builds");
                        return Err(rejection);
                    }
                    Ok::<_, tungstenite::handshake::server::ErrorResponse>(response)
                };
            let callback = &mut callback;
            match tungstenite::accept_hdr(connection, callback) {
                Ok(mut websocket) => {
                    let product = requested_path.trim_start_matches('/').to_string();
                    let principal = shared
                        .authorize_client(&product, requested_token.as_deref())
                        .ok()
                        .flatten();
                    if let Err(error) = shared.serve_client(&product, principal, &mut websocket) {
                        eprintln!("client session for {product} failed: {error}");
                    }
                }
                Err(error) => {
                    let reason = denied.lock().expect("denied lock poisoned").take();
                    if let Some(reason) = reason {
                        eprintln!("entitlement denied during handshake: {reason}");
                    } else {
                        eprintln!("websocket handshake failed: {error}");
                    }
                }
            }
        });
    }
    Ok(ExitCode::SUCCESS)
}

fn log_startup(plane: &server::MarketDataPlane, products: &[String]) {
    let health = plane.health();
    println!(
        "axiusflow_market_data_plane listener_started=true products={} backfill_bars={} durable_tap_active={} durable_delivered={} durable_recovery_required={}",
        products.join(","),
        health.backfill_bars,
        plane.durable_tap_active(),
        health.durable_delivered,
        health.durable_recovery_required
    );
}

fn reserve_ownership_epoch(
    redpanda_brokers: Option<&str>,
    ownership_state: Option<&str>,
) -> Result<Option<ownership_epoch::OwnershipLease>, String> {
    if redpanda_brokers.is_some() && ownership_state.is_none() {
        return Err("--redpanda-brokers requires --ownership-state".to_string());
    }
    ownership_state
        .map(|path| ownership_epoch::reserve_next(std::path::Path::new(path)))
        .transpose()
}

#[cfg(all(target_os = "linux", feature = "redpanda"))]
fn build_durable_tap(
    environment: &str,
    redpanda_brokers: Option<&str>,
) -> Result<server::DurableTap, String> {
    use std::time::Duration;

    let Some(bootstrap_servers) = redpanda_brokers else {
        return Ok(server::DurableTap::Inactive);
    };
    let config = axiusflow_streaming::RedpandaProducerConfig {
        bootstrap_servers: bootstrap_servers.to_string(),
        client_id: "axiusflow_market_data_plane".to_string(),
        topic: fanout::durable_bar_topic(environment)?,
        maximum_in_flight: 1_024,
        delivery_capacity: 2_048,
        request_timeout: Duration::from_secs(5),
    };
    let producer = axiusflow_streaming::RedpandaProducer::connect(&config)
        .map_err(|error| error.to_string())?;
    server::DurableTap::redpanda(producer)
}

#[cfg(not(all(target_os = "linux", feature = "redpanda")))]
fn build_durable_tap(
    _environment: &str,
    redpanda_brokers: Option<&str>,
) -> Result<server::DurableTap, String> {
    if redpanda_brokers.is_some() {
        return Err("--redpanda-brokers requires the Linux redpanda feature build".to_string());
    }
    Ok(server::DurableTap::Inactive)
}

fn load_entitlement(
    jwks_path: &str,
    policy_path: &str,
) -> Result<entitlement::EntitlementGuard, String> {
    let jwks_json = std::fs::read_to_string(jwks_path)
        .map_err(|error| format!("cannot read JWKS {jwks_path}: {error}"))?;
    let key_set = axiusflow_security::Ed25519KeySetSnapshot::try_from_jwks_json(1, &jwks_json)
        .map_err(|error| format!("invalid JWKS snapshot: {error}"))?;
    let verifier = axiusflow_security::Ed25519JwtVerifier::try_new(
        key_set,
        axiusflow_security::SystemVerificationClock,
        60,
    )
    .map_err(|error| format!("verifier configuration failed: {error}"))?;
    let evaluator = load_policy_evaluator(policy_path)?;
    Ok(entitlement::EntitlementGuard::new(verifier, evaluator))
}

fn load_policy_evaluator(
    policy_path: &str,
) -> Result<axiusflow_authorization::AuthorizationEvaluator, String> {
    let content = std::fs::read_to_string(policy_path)
        .map_err(|error| format!("cannot read policy {policy_path}: {error}"))?;
    let document: serde_json::Value =
        serde_json::from_str(&content).map_err(|error| format!("malformed policy: {error}"))?;
    let version = document["version"]
        .as_u64()
        .ok_or("policy document lacks a version")?;
    let grants_json = document["grants"]
        .as_array()
        .ok_or("policy document lacks grants")?;
    let mut grants = Vec::with_capacity(grants_json.len());
    for grant in grants_json {
        let action = match grant["action"].as_str().ok_or("grant lacks action")? {
            "read" => axiusflow_authorization::AuthorizationAction::Read,
            "stream" => axiusflow_authorization::AuthorizationAction::Stream,
            "trade" => axiusflow_authorization::AuthorizationAction::Trade,
            "administer" => axiusflow_authorization::AuthorizationAction::Administer,
            other => return Err(format!("unknown grant action: {other}")),
        };
        grants.push(axiusflow_authorization::AuthorizationGrant::new(
            axiusflow_authorization::AuthorizationGrantId::try_new(
                grant["grant_id"].as_str().ok_or("grant lacks id")?,
            )
            .map_err(|error| format!("invalid grant id: {error}"))?,
            axiusflow_authorization::PrincipalId::try_new(
                grant["principal_id"]
                    .as_str()
                    .ok_or("grant lacks principal")?,
            )
            .map_err(|error| format!("invalid principal id: {error}"))?,
            axiusflow_authorization::ResourceId::try_new(
                grant["resource_id"]
                    .as_str()
                    .ok_or("grant lacks resource")?,
            )
            .map_err(|error| format!("invalid resource id: {error}"))?,
            action,
        ));
    }
    let version = axiusflow_authorization::PolicyVersion::try_new(version)
        .map_err(|error| format!("invalid policy version: {error}"))?;
    let snapshot = axiusflow_authorization::AuthorizationPolicySnapshot::try_new(version, grants)
        .map_err(|error| format!("invalid policy snapshot: {error}"))?;
    Ok(axiusflow_authorization::AuthorizationEvaluator::with_snapshot(snapshot))
}

fn start_resnapshot_supervisor(
    plane: std::sync::Arc<server::MarketDataPlane>,
    jwks_path: Option<String>,
    policy_path: Option<String>,
    interval_seconds: u64,
) {
    let (Some(jwks_path), Some(policy_path)) = (jwks_path, policy_path) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("axiusflow-entitlement-resnapshot".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval_seconds.max(1)));
                match load_entitlement(&jwks_path, &policy_path) {
                    Ok(guard) => match plane.resnapshot_entitlement(guard) {
                        Ok(disconnected) if disconnected > 0 => {
                            eprintln!("entitlement resnapshot disconnected {disconnected} clients");
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!("entitlement resnapshot failed: {error}"),
                    },
                    Err(error) => eprintln!("entitlement reload failed: {error}"),
                }
            }
        });
}
