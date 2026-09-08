//! Paired IPC streams, exact handshake validation, and bounded frame delivery.

use super::*;

pub(super) fn fresh_session_nonce() -> Result<u64, EngineConnectionFailure> {
    let mut nonce_bytes = [0_u8; 8];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|_| unreached_failure("system CSPRNG unavailable".to_string()))?;
    Ok(u64::from_le_bytes(nonce_bytes).max(1))
}

/// Opens one directed session stream and sends its hello.
///
/// The command stream is nonblocking from birth: handshake reads poll it with
/// a deadline because a wedged endpoint must fail bounded instead of hanging
/// a blocking read. The event stream stays blocking for prompt close
/// detection once its reader thread owns it; its hello is written before any
/// thread shares the handle.
///
/// When `optional` is set, a refused connection resolves to `None` instead of
/// an error so a legacy single-stream resident can still be replaced.
pub(super) fn open_session_stream(
    name: interprocess::local_socket::Name<'_>,
    installation_token: &[u8],
    release: &axiusflow_platform_runtime::ReleaseIdentity,
    session_nonce: u64,
    stream_role: StreamRole,
    optional: bool,
    deadline: Instant,
) -> Result<Option<LocalSocketStream>, EngineConnectionFailure> {
    let mut stream = match LocalSocketStream::connect(name) {
        Ok(stream) => stream,
        Err(_) if optional => return Ok(None),
        Err(error) => return Err(unreached_failure(error.to_string())),
    };
    if stream_role == StreamRole::Command {
        stream
            .set_nonblocking(true)
            .map_err(|error| reached_failure(error.to_string()))?;
    }
    send_hello(
        &mut stream,
        installation_token,
        release,
        session_nonce,
        stream_role,
        deadline,
    )
    .map_err(reached_failure)?;
    Ok(Some(stream))
}

/// Reads the readiness reply on the command stream and checks release identity.
pub(super) fn read_session_ready(
    command: &mut LocalSocketStream,
    decoder: &mut EnvelopeDecoder,
    release: &axiusflow_platform_runtime::ReleaseIdentity,
    deadline: Instant,
) -> Result<EngineReady, EngineConnectionFailure> {
    let ready = match read_one_payload(command, decoder, deadline).map_err(reached_failure)? {
        envelope::Payload::EngineReady(ready) => ready,
        envelope::Payload::Fault(fault) => {
            return Err(EngineConnectionFailure {
                detail: fault.redacted_detail,
                endpoint_reached: true,
                legacy_stopped: false,
            });
        }
        _ => {
            return Err(EngineConnectionFailure {
                detail: "engine did not complete readiness negotiation".to_string(),
                endpoint_reached: true,
                legacy_stopped: false,
            });
        }
    };
    if ready.release_identity != release.release_identity
        || ready.install_generation != release.install_generation
    {
        return Err(EngineConnectionFailure {
            detail: "resident engine release identity does not match the active desktop"
                .to_string(),
            endpoint_reached: true,
            legacy_stopped: false,
        });
    }
    Ok(ready)
}

fn send_hello(
    stream: &mut LocalSocketStream,
    installation_token: &[u8],
    release: &axiusflow_platform_runtime::ReleaseIdentity,
    session_nonce: u64,
    stream_role: StreamRole,
    deadline: Instant,
) -> Result<(), String> {
    let frame = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: installation_token.to_vec(),
            client_kind: ClientKind::Ui as i32,
            release_identity: release.release_identity.clone(),
            install_generation: release.install_generation,
            session_nonce,
            stream_role: stream_role as i32,
        })),
    })
    .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
    write_frame(stream, &frame, deadline)
}

/// Writes one complete frame without ever blocking the transport handle:
/// temporary backpressure is retried until the deadline, anything else fails.
pub(super) fn write_frame(
    stream: &mut LocalSocketStream,
    frame: &[u8],
    deadline: Instant,
) -> Result<(), String> {
    let mut written = 0;
    while written < frame.len() {
        match stream.write(&frame[written..]) {
            Ok(0) => {}
            Ok(count) => written += count,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err("ipc_send failed: local transport is unavailable".to_string());
            }
        }
        if written < frame.len() {
            if Instant::now() >= deadline {
                return Err("ipc_send failed: local transport is busy".to_string());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    stream
        .flush()
        .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())
}

/// Reads one complete payload, polling a nonblocking handshake stream until
/// the deadline. A zero-byte read here is temporary no-data, not peer
/// closure: the deadline bounds a wedged or departed peer.
pub(super) fn read_one_payload(
    stream: &mut LocalSocketStream,
    decoder: &mut EnvelopeDecoder,
    deadline: Instant,
) -> Result<envelope::Payload, String> {
    loop {
        let mut chunk = [0_u8; 16 * 1024];
        match stream.read(&mut chunk) {
            Ok(0) => {}
            Ok(count) => {
                let mut envelopes = decoder
                    .push(&chunk[..count])
                    .map_err(|_| "ipc_receive failed: local message is invalid".to_string())?;
                if let Some(envelope) = envelopes.pop() {
                    return envelope.payload.ok_or_else(|| {
                        "ipc_receive failed: engine message has no payload".to_string()
                    });
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err("ipc_receive failed: local engine connection closed".to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err("ipc_receive failed: local engine connection closed".to_string());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Shuts down a legacy single-stream resident over an already-authenticated
/// command stream: the readiness reply was already consumed by the caller.
pub(super) struct FramedConnection {
    command: LocalSocketStream,
    incoming: Receiver<Result<Envelope, String>>,
    stop: Arc<AtomicBool>,
}

impl FramedConnection {
    /// Forms a paired session from a write-only command stream and a
    /// read-only event stream.
    ///
    /// Neither stream is ever split: each transport handle has exactly one
    /// owner and one direction, so a blocking read can never stall a
    /// concurrent write on any platform.
    pub(super) fn new(
        command: LocalSocketStream,
        event: LocalSocketStream,
    ) -> Result<Self, String> {
        // Where the platform supports receive timeouts the event reader wakes
        // periodically to observe `stop`; elsewhere the blocked read unblocks
        // on data or peer closure. `set_recv_timeout` is unsupported on
        // Windows named pipes, so it is only attempted where it exists.
        #[cfg(unix)]
        let _ = event.set_recv_timeout(Some(Duration::from_millis(100)));
        let (incoming_tx, incoming) = mpsc::sync_channel(IPC_INBOX_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("axiusflow-desktop-ipc-reader".to_string())
            .spawn(move || read_ipc_messages(event, &incoming_tx, &reader_stop))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            command,
            incoming,
            stop,
        })
    }

    pub(super) fn send(&mut self, payload: envelope::Payload) -> Result<(), String> {
        let deadline = Instant::now()
            .checked_add(SESSION_SEND_TIMEOUT)
            .ok_or_else(|| "ipc_send failed: local transport is busy".to_string())?;
        self.send_until(payload, deadline)
    }

    pub(super) fn send_until(
        &mut self,
        payload: envelope::Payload,
        deadline: Instant,
    ) -> Result<(), String> {
        let frame = encode_envelope(&Envelope {
            protocol_version: PROTOCOL_VERSION,
            target_consumer_id: 0,
            payload: Some(payload),
        })
        .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
        if Instant::now() >= deadline {
            return Err("ipc_send failed: local transport timed out".to_string());
        }
        write_frame(&mut self.command, &frame, deadline)
    }

    pub(super) fn receive_routed(&mut self) -> Result<(u64, envelope::Payload), String> {
        self.incoming
            .recv()
            .map_err(|_| "ipc_receive failed: local engine connection closed".to_string())?
            .and_then(routed_payload)
    }

    pub(super) fn receive_routed_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(u64, envelope::Payload)>, String> {
        match self.incoming.recv_timeout(timeout) {
            Ok(envelope) => envelope.and_then(routed_payload).map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err("ipc_receive failed: local engine connection closed".to_string())
            }
        }
    }
}

impl Drop for FramedConnection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn routed_payload(message: Envelope) -> Result<(u64, envelope::Payload), String> {
    let payload = message
        .payload
        .ok_or_else(|| "ipc_receive failed: engine message has no payload".to_string())?;
    Ok((message.target_consumer_id, payload))
}

fn read_ipc_messages(
    mut reader: LocalSocketStream,
    incoming: &mpsc::SyncSender<Result<Envelope, String>>,
    stop: &AtomicBool,
) {
    let mut decoder = match EnvelopeDecoder::try_new() {
        Ok(decoder) => decoder,
        Err(error) => {
            let _ = incoming.send(Err(error.to_string()));
            return;
        }
    };
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let mut chunk = [0_u8; 16 * 1024];
        let count = match reader.read(&mut chunk) {
            // The reader is always blocking (see `FramedConnection::new`), so
            // a zero-byte read genuinely means the peer closed, on every
            // platform. Never treat it as temporary no-data.
            Ok(0) => {
                let _ = incoming.send(Err(
                    "ipc_receive failed: local engine connection closed".to_string()
                ));
                return;
            }
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(_) => {
                let _ = incoming.send(Err(
                    "ipc_receive failed: local transport is unavailable".to_string()
                ));
                return;
            }
        };
        let Ok(envelopes) = decoder.push(&chunk[..count]) else {
            let _ = incoming.send(Err(
                "ipc_receive failed: local message is invalid".to_string()
            ));
            return;
        };
        for envelope in envelopes {
            if incoming.send(Ok(envelope)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    fn burst_reader() -> (
        Receiver<Result<Envelope, String>>,
        Receiver<()>,
        thread::JoinHandle<()>,
    ) {
        let name = format!(
            "axiusflow-framing-burst-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let listener = ListenerOptions::new()
            .name(
                name.as_str()
                    .to_ns_name::<GenericNamespaced>()
                    .expect("name"),
            )
            .create_sync()
            .expect("listener");
        let (incoming, receiver) = mpsc::sync_channel(2);
        let (finished, completion) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let stream = listener.accept().expect("accept real IPC stream");
            read_ipc_messages(stream, &incoming, &AtomicBool::new(false));
            finished.send(()).expect("completion receiver");
        });
        let mut writer = LocalSocketStream::connect(
            name.as_str()
                .to_ns_name::<GenericNamespaced>()
                .expect("name"),
        )
        .expect("connect");
        for sequence in 1..=8 {
            let frame = encode_envelope(&Envelope {
                protocol_version: PROTOCOL_VERSION,
                target_consumer_id: sequence,
                payload: Some(envelope::Payload::GetEngineStatus(GetEngineStatus {})),
            })
            .expect("frame");
            writer.write_all(&frame).expect("write burst");
        }
        drop(writer);
        (receiver, completion, reader)
    }

    #[test]
    fn slow_consumer_drains_a_real_ipc_burst_in_order_through_a_bounded_inbox() {
        let (incoming, completed, reader) = burst_reader();
        assert!(matches!(
            completed.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        for sequence in 1..=8 {
            let message = incoming
                .recv_timeout(Duration::from_secs(5))
                .expect("bounded delivery")
                .expect("valid frame");
            assert_eq!(message.target_consumer_id, sequence);
        }
        completed
            .recv_timeout(Duration::from_secs(5))
            .expect("reader exits after peer closes");
        reader.join().expect("reader");
    }

    #[test]
    fn retiring_a_consumer_unblocks_bounded_ipc_delivery() {
        let (incoming, completed, reader) = burst_reader();
        incoming
            .recv_timeout(Duration::from_secs(5))
            .expect("reader accepted data")
            .expect("valid frame");
        drop(incoming);
        completed
            .recv_timeout(Duration::from_secs(5))
            .expect("retirement releases backpressure");
        reader.join().expect("reader");
    }
}
