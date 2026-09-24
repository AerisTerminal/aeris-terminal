//! One bounded process-wide resolver. OS DNS may block, but callers never do
//! past their deadline and timed-out calls never create replacement threads.
use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(20);
type Resolution = io::Result<Vec<SocketAddr>>;
struct Request {
    host: String,
    port: u16,
    deadline: Instant,
    reply: mpsc::SyncSender<Resolution>,
}
static RESOLVER: OnceLock<io::Result<mpsc::SyncSender<Request>>> = OnceLock::new();

/// Resolves at most sixteen addresses using a single worker and one queued request.
///
/// # Errors
/// Returns timeout, cancellation, queue-capacity, or OS resolution errors.
pub fn resolve_addresses(
    host: &str,
    port: u16,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Resolution {
    let resolver = RESOLVER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<Request>(1);
            thread::Builder::new()
                .name("asceify-dns".into())
                .spawn(move || {
                    while let Ok(request) = receiver.recv() {
                        if Instant::now() >= request.deadline {
                            continue;
                        }
                        let result = (request.host.as_str(), request.port)
                            .to_socket_addrs()
                            .map(|addresses| addresses.take(16).collect());
                        let _ = request.reply.try_send(result);
                    }
                })?;
            Ok(sender)
        })
        .as_ref()
        .map_err(|error| io::Error::new(error.kind(), "DNS worker unavailable"))?;
    resolve_with(resolver, host, port, deadline, stop)
}

fn resolve_with(
    resolver: &mpsc::SyncSender<Request>,
    host: &str,
    port: u16,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Resolution {
    let check = || {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            Err(io::Error::new(io::ErrorKind::Interrupted, "DNS cancelled"))
        } else if Instant::now() >= deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "DNS deadline expired",
            ))
        } else {
            Ok(())
        }
    };
    check()?;
    if host.is_empty() || host.len() > 253 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid DNS hostname",
        ));
    }
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return Ok(vec![SocketAddr::new(address, port)]);
    }
    let (reply, receiver) = mpsc::sync_channel(1);
    let mut request = Request {
        host: host.into(),
        port,
        deadline,
        reply,
    };
    loop {
        check()?;
        match resolver.try_send(request) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(returned)) => {
                request = returned;
                thread::sleep(
                    Duration::from_millis(1)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "DNS worker stopped",
                ));
            }
        }
    }
    loop {
        check()?;
        match receiver.recv_timeout(POLL.min(deadline.saturating_duration_since(Instant::now()))) {
            Ok(result) => {
                check()?;
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "DNS worker stopped",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stalled_resolution_bounds_wait_and_queue_without_spawning_workers() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let result = resolve_with(
            &sender,
            "example.invalid",
            443,
            Instant::now() + Duration::from_millis(10),
            None,
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let result = resolve_with(
            &sender,
            "example.invalid",
            443,
            Instant::now() + Duration::from_millis(10),
            None,
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let stop = AtomicBool::new(true);
        let result = resolve_with(
            &sender,
            "example.invalid",
            443,
            Instant::now() + Duration::from_secs(1),
            Some(&stop),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }
}
