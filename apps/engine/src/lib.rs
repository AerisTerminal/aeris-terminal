//! Resident engine process boundary and authenticated local handshake.

use std::io::{self, Read, Write};

use axiusflow_local_engine_protocol::{
    ClientHello, EngineFaultCode, EngineReady, Envelope, EnvelopeDecoder, Fault, PROTOCOL_VERSION,
    encode_envelope, envelope,
};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};

/// Stable per-user local socket name for protocol version one.
pub const ENGINE_SOCKET_NAME: &str = "axiusflow-engine-v1";

/// Minimum entropy required for the installation credential.
pub const MINIMUM_TOKEN_BYTES: usize = 32;

/// Creates the engine's single-instance local listener.
///
/// # Errors
/// Returns an I/O error if another engine owns the name or the OS rejects it.
pub fn bind_listener(name: &str) -> io::Result<LocalSocketListener> {
    let name = name.to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(name).create_sync()
}

/// Handles one client through the authenticated readiness handshake.
///
/// # Errors
/// Returns an error for I/O, framing, or malformed first-message failures.
pub fn serve_client(
    mut stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
) -> Result<(), String> {
    if installation_token.len() < MINIMUM_TOKEN_BYTES {
        return Err("installation credential is too short".to_string());
    }
    let hello = read_hello(&mut stream)?;
    if hello.installation_token != installation_token {
        write_payload(
            &mut stream,
            envelope::Payload::Fault(Fault {
                code: EngineFaultCode::Unauthenticated as i32,
                redacted_detail: "local engine authentication failed".to_string(),
            }),
        )?;
        return Ok(());
    }
    write_payload(
        &mut stream,
        envelope::Payload::EngineReady(EngineReady {
            protocol_version: PROTOCOL_VERSION,
            engine_epoch,
            workspace_revision: 0,
        }),
    )
}

fn read_hello(stream: &mut LocalSocketStream) -> Result<ClientHello, String> {
    let mut decoder = EnvelopeDecoder::try_new().map_err(|error| error.to_string())?;
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let count = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("client disconnected before authentication".to_string());
        }
        let envelopes = decoder
            .push(&chunk[..count])
            .map_err(|error| error.to_string())?;
        if let Some(envelope) = envelopes.into_iter().next() {
            return match envelope.payload {
                Some(envelope::Payload::ClientHello(hello)) => Ok(hello),
                _ => Err("client hello must be the first engine message".to_string()),
            };
        }
    }
}

fn write_payload(stream: &mut LocalSocketStream, payload: envelope::Payload) -> Result<(), String> {
    let frame = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(payload),
    })
    .map_err(|error| error.to_string())?;
    stream
        .write_all(&frame)
        .map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())
}
