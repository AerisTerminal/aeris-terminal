use std::{
    io::{Read, Write},
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use axiusflow_engine::{bind_listener, serve_client};
use axiusflow_local_engine_protocol::{
    ClientHello, ClientKind, EngineFaultCode, Envelope, EnvelopeDecoder, PROTOCOL_VERSION,
    encode_envelope, envelope,
};
use interprocess::local_socket::{GenericNamespaced, ToNsName as _, prelude::*};

static NEXT_NAME: AtomicU64 = AtomicU64::new(1);

fn unique_name() -> String {
    format!(
        "axiusflow-engine-test-{}-{}",
        std::process::id(),
        NEXT_NAME.fetch_add(1, Ordering::Relaxed)
    )
}

fn hello(token: &[u8]) -> Vec<u8> {
    encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: token.to_vec(),
            client_kind: ClientKind::Ui as i32,
        })),
    })
    .expect("encode hello")
}

fn exchange(token: &[u8], client_token: &[u8]) -> envelope::Payload {
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let expected = token.to_vec();
    let server = thread::spawn(move || {
        let stream = listener.accept().expect("accept client");
        serve_client(stream, &expected, 41).expect("serve client");
    });
    let socket_name = name
        .to_ns_name::<GenericNamespaced>()
        .expect("create socket name");
    let mut stream = LocalSocketStream::connect(socket_name).expect("connect client");
    stream.write_all(&hello(client_token)).expect("write hello");
    let mut bytes = [0_u8; 4096];
    let count = stream.read(&mut bytes).expect("read reply");
    server.join().expect("join server");
    let mut decoder = EnvelopeDecoder::try_new().expect("create decoder");
    decoder
        .push(&bytes[..count])
        .expect("decode reply")
        .pop()
        .and_then(|envelope| envelope.payload)
        .expect("reply payload")
}

#[test]
fn valid_installation_token_receives_engine_readiness() {
    let token = [7_u8; 32];
    let envelope::Payload::EngineReady(ready) = exchange(&token, &token) else {
        panic!("expected engine readiness");
    };
    assert_eq!(ready.protocol_version, PROTOCOL_VERSION);
    assert_eq!(ready.engine_epoch, 41);
}

#[test]
fn invalid_installation_token_is_rejected_without_readiness() {
    let envelope::Payload::Fault(fault) = exchange(&[7_u8; 32], &[8_u8; 32]) else {
        panic!("expected authentication fault");
    };
    assert_eq!(fault.code, EngineFaultCode::Unauthenticated as i32);
}

#[test]
fn listener_name_has_exactly_one_owner() {
    let name = unique_name();
    let _first = bind_listener(&name).expect("bind first owner");
    assert!(matches!(
        bind_listener(&name)
            .expect_err("reject second owner")
            .kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    ));
}
