use crate::{
    ProtoMessage,
    accounts::{CtraderAccount, DemoAccount},
    codec,
    generated::{
        ProtoOaAccountAuthRes, ProtoOaAccountDisconnectEvent, ProtoOaAccountsTokenInvalidatedEvent,
        ProtoOaApplicationAuthRes, ProtoOaCtidTraderAccount, ProtoOaErrorRes,
        ProtoOaGetAccountListByAccessTokenRes, ProtoOaLightSymbol, ProtoOaSymbolsListRes,
    },
    host::CtraderHost,
    session::{AccessToken, AppCredentials, CtraderSession, SessionFault},
    transport::{Bucket, Transport, TransportError},
};
use prost::Message;
use rustls::{
    RootCertStore, ServerConfig, ServerConnection, StreamOwned,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};
use std::{
    io::Write,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn listener() -> (TcpListener, SocketAddr, RootCertStore, Arc<ServerConfig>) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(certified.cert.der().clone()).unwrap();
    let config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certified.cert.der().clone()],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        )),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address, roots, Arc::new(config))
}
fn accept(
    listener: &TcpListener,
    config: Arc<ServerConfig>,
) -> StreamOwned<ServerConnection, TcpStream> {
    let (socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    StreamOwned::new(ServerConnection::new(config).unwrap(), socket)
}
fn reply(
    socket: &mut StreamOwned<ServerConnection, TcpStream>,
    request: &ProtoMessage,
    kind: u32,
    body: Vec<u8>,
) {
    codec::write_frame(
        socket,
        &ProtoMessage {
            payload_type: kind,
            payload: Some(body),
            client_msg_id: request.client_msg_id.clone(),
        },
    )
    .unwrap();
    socket.flush().unwrap();
}
fn account_list() -> Vec<u8> {
    ProtoOaGetAccountListByAccessTokenRes {
        payload_type: None,
        access_token: "sample".into(),
        permission_scope: None,
        ctid_trader_account: vec![
            ProtoOaCtidTraderAccount {
                ctid_trader_account_id: 7,
                is_live: Some(false),
                trader_login: Some(23),
                last_closing_deal_timestamp: None,
                last_balance_update_timestamp: None,
                broker_title_short: Some("Example".into()),
            },
            ProtoOaCtidTraderAccount {
                ctid_trader_account_id: 8,
                is_live: Some(true),
                trader_login: Some(24),
                last_closing_deal_timestamp: None,
                last_balance_update_timestamp: None,
                broker_title_short: Some("Example".into()),
            },
        ],
    }
    .encode_to_vec()
}
fn authorize(socket: &mut StreamOwned<ServerConnection, TcpStream>) {
    let app = codec::read_frame(socket).unwrap();
    assert_eq!(app.payload_type, 2100);
    reply(
        socket,
        &app,
        2101,
        ProtoOaApplicationAuthRes { payload_type: None }.encode_to_vec(),
    );
    let accounts = codec::read_frame(socket).unwrap();
    assert_eq!(accounts.payload_type, 2149);
    reply(socket, &accounts, 2150, account_list());
}
fn client(address: SocketAddr, roots: RootCertStore, stop: Arc<AtomicBool>) -> Transport {
    Transport::connect_to(address, "localhost", roots, stop).unwrap()
}
fn session(transport: Transport) -> CtraderSession {
    CtraderSession::open_with_transport(
        CtraderHost::Demo,
        &AppCredentials {
            client_id: "sample".into(),
            client_secret: "sample".into(),
        },
        AccessToken("sample".into()),
        transport,
    )
    .unwrap()
}

#[test]
fn auth_sequence_authorizes_account_only_when_used() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let request = codec::read_frame(&mut socket).unwrap();
        assert_eq!(request.payload_type, 2102);
        reply(
            &mut socket,
            &request,
            2103,
            ProtoOaAccountAuthRes {
                payload_type: None,
                ctid_trader_account_id: 7,
            }
            .encode_to_vec(),
        );
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    assert_eq!(session.accounts().len(), 2);
    assert!(!session.accounts()[0].is_live);
    assert!(session.accounts()[1].is_live);
    let account = session.accounts()[0].clone();
    session
        .authorize_account(&account, || panic!("unexpected refresh"))
        .unwrap();
    session
        .authorize_account(&account, || panic!("unexpected refresh"))
        .unwrap();
    let live = session.accounts()[1].clone();
    assert_eq!(
        session.authorize_account(&live, || panic!("unexpected refresh")),
        Err(SessionFault::Protocol)
    );
    session.close();
    server.join().unwrap();
}

#[test]
fn symbol_inventory_requires_account_auth_and_preserves_names() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let account = codec::read_frame(&mut socket).unwrap();
        assert_eq!(account.payload_type, 2102);
        reply(
            &mut socket,
            &account,
            2103,
            ProtoOaAccountAuthRes {
                payload_type: None,
                ctid_trader_account_id: 7,
            }
            .encode_to_vec(),
        );
        let request = codec::read_frame(&mut socket).unwrap();
        assert_eq!(request.payload_type, 2114);
        reply(
            &mut socket,
            &request,
            2115,
            ProtoOaSymbolsListRes {
                payload_type: None,
                ctid_trader_account_id: 7,
                symbol: vec![ProtoOaLightSymbol {
                    symbol_id: 1,
                    symbol_name: Some("EURUSD".into()),
                    enabled: Some(true),
                    base_asset_id: None,
                    quote_asset_id: None,
                    symbol_category_id: None,
                    description: None,
                    sorting_number: None,
                }],
                archived_symbol: Vec::new(),
            }
            .encode_to_vec(),
        );
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    let account = session.accounts()[0].clone();
    assert_eq!(session.symbol_names(&account).unwrap(), ["EURUSD"]);
    session.close();
    server.join().unwrap();
}

#[test]
fn demo_newtype_requires_account_observed_on_demo_host() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let _ = codec::read_frame(&mut socket);
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    let account = session.accounts()[0].clone();
    assert_eq!(
        DemoAccount::from_session(&session, &account)
            .unwrap()
            .account()
            .ctid,
        7
    );
    let fabricated = CtraderAccount {
        ctid: 99,
        ..account
    };
    assert!(DemoAccount::from_session(&session, &fabricated).is_err());
    assert!(DemoAccount::from_session(&session, &session.accounts()[1]).is_err());
    session.close();
    server.join().unwrap();
}

#[test]
fn token_invalidated_refreshes_once_then_needs_reconnect() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        for _ in 0..2 {
            let request = codec::read_frame(&mut socket).unwrap();
            assert_eq!(request.payload_type, 2102);
            reply(
                &mut socket,
                &request,
                2147,
                ProtoOaAccountsTokenInvalidatedEvent {
                    payload_type: None,
                    ctid_trader_account_ids: vec![7],
                    reason: None,
                }
                .encode_to_vec(),
            );
        }
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    let account = session.accounts()[0].clone();
    let mut refreshes = 0;
    assert_eq!(
        session.authorize_account(&account, || {
            refreshes += 1;
            Ok(AccessToken("replacement".into()))
        }),
        Err(SessionFault::NeedsReconnect)
    );
    assert_eq!(refreshes, 1);
    session.close();
    server.join().unwrap();
}

#[test]
fn unsolicited_token_invalidation_refreshes_once_and_reauthorizes() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let first = codec::read_frame(&mut socket).unwrap();
        reply(
            &mut socket,
            &first,
            2103,
            ProtoOaAccountAuthRes {
                payload_type: None,
                ctid_trader_account_id: 7,
            }
            .encode_to_vec(),
        );
        let invalidated = ProtoMessage {
            payload_type: 2147,
            payload: Some(
                ProtoOaAccountsTokenInvalidatedEvent {
                    payload_type: None,
                    ctid_trader_account_ids: vec![7],
                    reason: None,
                }
                .encode_to_vec(),
            ),
            client_msg_id: None,
        };
        codec::write_frame(&mut socket, &invalidated).unwrap();
        socket.flush().unwrap();
        let second = codec::read_frame(&mut socket).unwrap();
        assert_eq!(second.payload_type, 2102);
        reply(
            &mut socket,
            &second,
            2103,
            ProtoOaAccountAuthRes {
                payload_type: None,
                ctid_trader_account_id: 7,
            }
            .encode_to_vec(),
        );
        codec::write_frame(&mut socket, &invalidated).unwrap();
        socket.flush().unwrap();
        thread::sleep(Duration::from_millis(100));
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    let account = session.accounts()[0].clone();
    session
        .authorize_account(&account, || panic!("unexpected refresh"))
        .unwrap();
    let mut refreshes = 0;
    assert_eq!(
        session.next_event_with_refresh(Duration::from_secs(2), || {
            refreshes += 1;
            Ok(AccessToken("replacement".into()))
        }),
        Ok(None)
    );
    assert_eq!(
        session.next_event_with_refresh(Duration::from_secs(2), || panic!("second refresh")),
        Err(SessionFault::NeedsReconnect)
    );
    assert_eq!(refreshes, 1);
    session.close();
    server.join().unwrap();
}

#[test]
fn responses_correlate_out_of_order_unknown_ids_ignored_and_time_out() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let first = codec::read_frame(&mut socket).unwrap();
        let second = codec::read_frame(&mut socket).unwrap();
        reply(
            &mut socket,
            &ProtoMessage {
                client_msg_id: Some("unknown".into()),
                ..second.clone()
            },
            2105,
            vec![16, 1],
        );
        reply(&mut socket, &second, 2105, vec![16, 2]);
        reply(&mut socket, &first, 2105, vec![16, 3]);
        thread::sleep(Duration::from_millis(200));
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    let first = session
        .send_request(2104, vec![], 2105, Bucket::General, Duration::from_secs(1))
        .unwrap();
    let second = session
        .send_request(2104, vec![], 2105, Bucket::General, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        session.await_response(&first).unwrap().payload.as_deref(),
        Some([16, 3].as_slice())
    );
    assert_eq!(
        session.await_response(&second).unwrap().payload.as_deref(),
        Some([16, 2].as_slice())
    );
    let timeout = session
        .send_request(
            2104,
            vec![],
            2105,
            Bucket::General,
            Duration::from_millis(100),
        )
        .unwrap();
    assert_eq!(session.await_response(&timeout), Err(SessionFault::Timeout));
    session.close();
    server.join().unwrap();
}

#[test]
fn cancellation_unblocks_both_named_workers_before_read_deadline() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let _ = codec::read_frame(&mut socket);
    });
    let stop = Arc::new(AtomicBool::new(false));
    let mut session = session(client(address, roots, Arc::clone(&stop)));
    let started = Instant::now();
    stop.store(true, Ordering::Release);
    session.close();
    assert!(started.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}

#[test]
fn cancellation_during_a_request_reports_cancelled_not_a_broken_session() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        // Hold the request unanswered until the client cancels.
        let _ = codec::read_frame(&mut socket);
        let _ = codec::read_frame(&mut socket);
    });
    let stop = Arc::new(AtomicBool::new(false));
    let mut session = session(client(address, roots, Arc::clone(&stop)));
    let cancellation = Arc::clone(&stop);
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        cancellation.store(true, Ordering::Release);
    });
    let result = session.request(2104, vec![], 2105, Bucket::General, Duration::from_secs(5));
    assert_eq!(result, Err(SessionFault::Cancelled));
    canceller.join().unwrap();
    session.close();
    server.join().unwrap();
}

#[test]
fn cancellation_during_connect_has_a_two_second_bound() {
    let (listener, address, roots, _) = listener();
    let (accepted, ready) = std::sync::mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        thread::sleep(Duration::from_millis(400));
    });
    let stop = Arc::new(AtomicBool::new(false));
    let cancellation = Arc::clone(&stop);
    let connecting =
        thread::spawn(move || Transport::connect_to(address, "localhost", roots, cancellation));
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    stop.store(true, Ordering::Release);
    assert!(matches!(
        connecting.join().unwrap(),
        Err(TransportError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}

#[test]
fn idle_writer_sends_heartbeat_and_inbound_heartbeat_is_filtered() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        codec::write_frame(
            &mut socket,
            &ProtoMessage {
                payload_type: 51,
                payload: None,
                client_msg_id: None,
            },
        )
        .unwrap();
        socket.flush().unwrap();
        let started = Instant::now();
        let heartbeat = codec::read_frame(&mut socket).unwrap();
        assert_eq!(heartbeat.payload_type, 51);
        assert!(started.elapsed() >= Duration::from_secs(7));
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    assert!(
        session
            .next_event(Duration::from_millis(150))
            .unwrap()
            .is_none()
    );
    server.join().unwrap();
    session.close();
}

#[test]
fn pending_map_rejects_more_than_256_unanswered_requests() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        thread::sleep(Duration::from_millis(500));
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    for _ in 0..256 {
        session
            .send_request(2104, vec![], 2105, Bucket::General, Duration::from_secs(5))
            .unwrap();
    }
    assert_eq!(
        session.send_request(2104, vec![], 2105, Bucket::General, Duration::from_secs(5)),
        Err(SessionFault::Overflow)
    );
    session.close();
    server.join().unwrap();
}

#[test]
fn outbound_queue_reports_overflow_without_blocking() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        let _ = codec::read_frame(&mut socket);
    });
    let mut transport = client(address, roots, Arc::new(AtomicBool::new(false)));
    transport.pause(Bucket::General, Duration::from_secs(5));
    let frame = ProtoMessage {
        payload_type: 2104,
        payload: Some(vec![0]),
        client_msg_id: None,
    };
    transport.send(frame.clone(), Bucket::General).unwrap();
    thread::sleep(Duration::from_millis(150));
    for _ in 0..256 {
        transport.send(frame.clone(), Bucket::General).unwrap();
    }
    assert_eq!(
        transport.send(
            ProtoMessage {
                payload_type: 2104,
                payload: Some(vec![0; 256 * 1024]),
                client_msg_id: None,
            },
            Bucket::General
        ),
        Err(TransportError::RequestTooLarge)
    );
    assert_eq!(
        transport.send(frame, Bucket::General),
        Err(TransportError::Overflow)
    );
    transport.close();
    server.join().unwrap();
}

#[test]
fn general_and_historical_buckets_limit_200_requests_each() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        let mut general = Vec::new();
        let mut historical = Vec::new();
        while general.len() + historical.len() < 400 {
            let frame = codec::read_frame(&mut socket).unwrap();
            if frame.payload_type == 51 {
                continue;
            }
            if frame.payload.as_deref() == Some([0].as_slice()) {
                general.push(Instant::now());
            } else {
                historical.push(Instant::now());
            }
        }
        (general, historical)
    });
    let mut transport = client(address, roots, Arc::new(AtomicBool::new(false)));
    for (bucket, byte) in [(Bucket::General, 0_u8), (Bucket::Historical, 1_u8)] {
        for _ in 0..200 {
            let frame = ProtoMessage {
                payload_type: 2104,
                payload: Some(vec![byte]),
                client_msg_id: None,
            };
            loop {
                match transport.send(frame.clone(), bucket) {
                    Ok(()) => break,
                    Err(TransportError::Overflow) => thread::sleep(Duration::from_millis(5)),
                    Err(error) => panic!("{error:?}"),
                }
            }
        }
    }
    let (general, historical) = server.join().unwrap();
    for (observations, cap) in [(&general, 45), (&historical, 4)] {
        assert_eq!(observations.len(), 200);
        for (index, sent) in observations.iter().enumerate() {
            let in_window = observations[index..]
                .iter()
                .take_while(|later| later.duration_since(*sent) < Duration::from_secs(1))
                .count();
            assert!(
                in_window <= cap,
                "observed {in_window} > {cap} requests in one second"
            );
        }
    }
    transport.close();
}

#[test]
fn account_disconnect_reauthorizes_once_then_backs_off() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        for _ in 0..2 {
            let request = codec::read_frame(&mut socket).unwrap();
            assert_eq!(request.payload_type, 2102);
            reply(
                &mut socket,
                &request,
                2164,
                ProtoOaAccountDisconnectEvent {
                    payload_type: None,
                    ctid_trader_account_id: 7,
                }
                .encode_to_vec(),
            );
        }
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    let account = session.accounts()[0].clone();
    assert_eq!(
        session.authorize_account(&account, || panic!("unexpected refresh")),
        Err(SessionFault::AccountDisconnect(7))
    );
    session.close();
    server.join().unwrap();
}

#[test]
fn blocked_payload_pauses_its_bucket_without_retrying_order() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener, config);
        authorize(&mut socket);
        let request = codec::read_frame(&mut socket).unwrap();
        reply(
            &mut socket,
            &request,
            2142,
            ProtoOaErrorRes {
                payload_type: None,
                ctid_trader_account_id: None,
                error_code: "BLOCKED_PAYLOAD_TYPE".into(),
                description: None,
                maintenance_end_timestamp: None,
                retry_after: Some(1),
            }
            .encode_to_vec(),
        );
        let started = Instant::now();
        let next = codec::read_frame(&mut socket).unwrap();
        assert_eq!(next.payload_type, 2104);
        assert!(started.elapsed() >= Duration::from_millis(900));
    });
    let mut session = session(client(address, roots, Arc::new(AtomicBool::new(false))));
    assert_eq!(
        session.request(2106, vec![], 2107, Bucket::General, Duration::from_secs(2)),
        Err(SessionFault::RateLimited {
            bucket: Bucket::General,
            wait: Duration::from_secs(1)
        })
    );
    let next = session
        .send_request(2104, vec![], 2105, Bucket::General, Duration::from_secs(2))
        .unwrap();
    assert_eq!(session.await_response(&next), Err(SessionFault::Reconnect));
    session.close();
    server.join().unwrap();
}

#[test]
fn disconnected_socket_reconnects_with_new_generation() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        let mut first = accept(&listener, Arc::clone(&config));
        authorize(&mut first);
        drop(first);
        let mut second = accept(&listener, config);
        authorize(&mut second);
    });
    let stop = Arc::new(AtomicBool::new(false));
    let mut session = session(client(address, roots.clone(), Arc::clone(&stop)));
    assert_eq!(
        session.next_event(Duration::from_secs(2)),
        Err(SessionFault::Reconnect)
    );
    let transport = client(address, roots, stop);
    session
        .reconnect_with_transport(
            &AppCredentials {
                client_id: "sample".into(),
                client_secret: "sample".into(),
            },
            transport,
        )
        .unwrap();
    assert_eq!(session.generation, 2);
    session.close();
    server.join().unwrap();
}

#[test]
fn app_auth_failure_refetches_credentials_only_once() {
    let (listener, address, roots, config) = listener();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let mut socket = accept(&listener, Arc::clone(&config));
            let request = codec::read_frame(&mut socket).unwrap();
            reply(
                &mut socket,
                &request,
                2142,
                ProtoOaErrorRes {
                    payload_type: None,
                    ctid_trader_account_id: None,
                    error_code: "CH_CLIENT_AUTH_FAILURE".into(),
                    description: None,
                    maintenance_end_timestamp: None,
                    retry_after: None,
                }
                .encode_to_vec(),
            );
        }
    });
    let mut refetches = 0;
    let credentials = AppCredentials {
        client_id: "sample".into(),
        client_secret: "sample".into(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut connections = 0;
    let result = CtraderSession::open_with_credentials_refresh_using(
        CtraderHost::Demo,
        &credentials,
        AccessToken("sample".into()),
        || {
            refetches += 1;
            Ok(AppCredentials {
                client_id: "replacement".into(),
                client_secret: "replacement".into(),
            })
        },
        || {
            connections += 1;
            Ok(client(address, roots.clone(), Arc::clone(&stop)))
        },
    );
    assert!(matches!(result, Err(SessionFault::ClientAuthFailure)));
    assert_eq!(refetches, 1);
    assert_eq!(connections, 2);
    server.join().unwrap();
}
