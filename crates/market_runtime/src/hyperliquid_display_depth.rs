use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use asceify_hyperliquid_market_adapter::{
    HYPERLIQUID_WS_URL, HyperliquidSocket, SocketEvent, WsClientEvent,
    build_aggregated_l2_subscription, build_ping, build_unsubscribe, decode_book_snapshot,
    parse_ws_frame,
};
use asceify_market_data::DepthSnapshot;

use crate::{
    hyperliquid_realtime::HyperliquidInstrumentDemand, market_service::ProviderCoordinatorWake,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_millis(2);
const PING_INTERVAL: Duration = Duration::from_secs(20);
const MESSAGE_SILENCE_TIMEOUT: Duration = Duration::from_secs(45);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAXIMUM_DECODE_FAILURES_PER_CONNECTION: u32 = 100;

/// `FlowSurface`'s 50x BTC-style ladder uses four significant figures upstream
/// for markets whose prices have at most five integer digits, then performs
/// the final side-aware 50x grouping in the ladder itself.
const DISPLAY_N_SIG_FIGS: u8 = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HyperliquidDisplayBookDemand {
    pub instrument: HyperliquidInstrumentDemand,
    pub provider_generation: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HyperliquidDisplayDepthDemand {
    pub books: Vec<HyperliquidDisplayBookDemand>,
}

pub(crate) enum HyperliquidDisplayDepthControl {
    Subscribe(HyperliquidDisplayDepthDemand),
    Stop,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HyperliquidDisplayBookSnapshot {
    pub provider_generation: u64,
    pub display_generation: u64,
    pub snapshot: DepthSnapshot,
}

pub(crate) enum HyperliquidDisplayDepthEvent {
    Reset { display_generation: u64 },
    Snapshot(HyperliquidDisplayBookSnapshot),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionExit {
    Reconnect,
    Parked,
    Closed,
}

struct SessionState {
    active: BTreeMap<String, String>,
    instruments: BTreeMap<String, HyperliquidDisplayBookDemand>,
    sequences: BTreeMap<String, u64>,
    decode_failures: u32,
    last_inbound: Instant,
    last_ping: Instant,
}

pub(crate) fn run(
    controls: &Receiver<HyperliquidDisplayDepthControl>,
    events: &SyncSender<HyperliquidDisplayDepthEvent>,
    stop: &Arc<AtomicBool>,
    wake: &ProviderCoordinatorWake,
    reconnect_delay: Duration,
) {
    let mut demand = HyperliquidDisplayDepthDemand::default();
    let mut display_generation = 1u64;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        drain_idle_controls(controls, &mut demand);
        if demand.books.is_empty() {
            match controls.recv_timeout(IDLE_POLL_INTERVAL) {
                Ok(HyperliquidDisplayDepthControl::Subscribe(next)) => demand = next,
                Ok(HyperliquidDisplayDepthControl::Stop)
                | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            continue;
        }

        if emit(
            events,
            HyperliquidDisplayDepthEvent::Reset { display_generation },
            stop,
            wake,
        ) {
            return;
        }
        if let Ok((mut socket, _shutdown)) =
            HyperliquidSocket::connect(HYPERLIQUID_WS_URL, CONNECT_TIMEOUT, stop)
        {
            match run_session(
                &mut socket,
                display_generation,
                &mut demand,
                controls,
                events,
                stop,
                wake,
            ) {
                SessionExit::Closed => return,
                SessionExit::Parked => continue,
                SessionExit::Reconnect => {}
            }
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        display_generation = display_generation.saturating_add(1).max(1);
        sleep_cancellable(reconnect_delay, stop);
    }
}

fn drain_idle_controls(
    controls: &Receiver<HyperliquidDisplayDepthControl>,
    demand: &mut HyperliquidDisplayDepthDemand,
) {
    while let Ok(control) = controls.try_recv() {
        match control {
            HyperliquidDisplayDepthControl::Subscribe(next) => *demand = next,
            HyperliquidDisplayDepthControl::Stop => demand.books.clear(),
        }
    }
}

fn run_session(
    socket: &mut HyperliquidSocket,
    display_generation: u64,
    demand: &mut HyperliquidDisplayDepthDemand,
    controls: &Receiver<HyperliquidDisplayDepthControl>,
    events: &SyncSender<HyperliquidDisplayDepthEvent>,
    stop: &Arc<AtomicBool>,
    wake: &ProviderCoordinatorWake,
) -> SessionExit {
    let started = Instant::now();
    let mut state = SessionState {
        active: BTreeMap::new(),
        instruments: BTreeMap::new(),
        sequences: BTreeMap::new(),
        decode_failures: 0,
        last_inbound: started,
        last_ping: started.checked_sub(PING_INTERVAL).unwrap_or(started),
    };
    if reconcile_subscriptions(socket, demand, &mut state).is_err() {
        return SessionExit::Reconnect;
    }
    loop {
        if stop.load(Ordering::Acquire) {
            socket.close();
            return SessionExit::Closed;
        }
        let (changed, stopped) = drain_session_controls(controls, demand);
        if stopped || demand.books.is_empty() {
            socket.close();
            return SessionExit::Parked;
        }
        if changed && reconcile_subscriptions(socket, demand, &mut state).is_err() {
            return SessionExit::Reconnect;
        }
        let now = Instant::now();
        if now.saturating_duration_since(state.last_inbound) >= MESSAGE_SILENCE_TIMEOUT {
            return SessionExit::Reconnect;
        }
        if now.saturating_duration_since(state.last_ping) >= PING_INTERVAL {
            if socket.send_text(&build_ping()).is_err() {
                return SessionExit::Reconnect;
            }
            state.last_ping = now;
        }
        match socket.read_event(now + READ_TIMEOUT) {
            Ok(SocketEvent::Text(text)) => {
                state.last_inbound = Instant::now();
                if handle_frame(&text, display_generation, &mut state, events, stop, wake).is_err()
                {
                    state.decode_failures = state.decode_failures.saturating_add(1);
                    if state.decode_failures >= MAXIMUM_DECODE_FAILURES_PER_CONNECTION {
                        return SessionExit::Reconnect;
                    }
                }
            }
            Ok(SocketEvent::Pong) => state.last_inbound = Instant::now(),
            Err(error) if is_read_timeout(&error) => {}
            Err(_) => return SessionExit::Reconnect,
        }
    }
}

fn drain_session_controls(
    controls: &Receiver<HyperliquidDisplayDepthControl>,
    demand: &mut HyperliquidDisplayDepthDemand,
) -> (bool, bool) {
    let mut changed = false;
    let mut stopped = false;
    while let Ok(control) = controls.try_recv() {
        match control {
            HyperliquidDisplayDepthControl::Subscribe(next) => {
                changed |= *demand != next;
                *demand = next;
            }
            HyperliquidDisplayDepthControl::Stop => stopped = true,
        }
    }
    (changed, stopped)
}

fn reconcile_subscriptions(
    socket: &mut HyperliquidSocket,
    demand: &HyperliquidDisplayDepthDemand,
    state: &mut SessionState,
) -> Result<(), String> {
    let mut desired = BTreeMap::new();
    let mut instruments = BTreeMap::new();
    for book in &demand.books {
        let coin = book.instrument.wire_coin.clone();
        if let Some(existing) = instruments.get(&coin)
            && existing != book
        {
            return Err("Hyperliquid display depth has conflicting coin demand".to_string());
        }
        instruments.insert(coin.clone(), book.clone());
        desired.insert(
            coin.clone(),
            build_aggregated_l2_subscription(&coin, DISPLAY_N_SIG_FIGS, None)?,
        );
    }
    let removed = state
        .active
        .keys()
        .filter(|coin| !desired.contains_key(*coin))
        .cloned()
        .collect::<Vec<_>>();
    for coin in removed {
        if let Some(frame) = state.active.remove(&coin)
            && let Ok(raw) = serde_json::from_str::<serde_json::Value>(&frame)
            && let Some(subscription) = raw.get("subscription")
        {
            socket.send_text(&build_unsubscribe(subscription))?;
        }
        state.sequences.remove(&coin);
    }
    for (coin, frame) in &desired {
        if !state.active.contains_key(coin) {
            socket.send_text(frame)?;
            state.active.insert(coin.clone(), frame.clone());
        }
    }
    state.instruments = instruments;
    Ok(())
}

fn handle_frame(
    text: &str,
    display_generation: u64,
    state: &mut SessionState,
    events: &SyncSender<HyperliquidDisplayDepthEvent>,
    stop: &AtomicBool,
    wake: &ProviderCoordinatorWake,
) -> Result<(), ()> {
    match parse_ws_frame(text).map_err(|_| ())? {
        WsClientEvent::Book { coin, book } => {
            let mapping = state.instruments.get(&coin).ok_or(())?;
            let sequence = state.sequences.get(&coin).copied().unwrap_or(1);
            let decoded = decode_book_snapshot(
                &book,
                &coin,
                &mapping.instrument.instrument_id,
                &mapping.instrument.entitlement_id,
                mapping.provider_generation,
                sequence,
                unix_nanos_now(),
            )
            .map_err(|_| ())?;
            state
                .sequences
                .insert(coin, sequence.checked_add(1).ok_or(())?);
            if emit(
                events,
                HyperliquidDisplayDepthEvent::Snapshot(HyperliquidDisplayBookSnapshot {
                    provider_generation: mapping.provider_generation,
                    display_generation,
                    snapshot: decoded.snapshot,
                }),
                stop,
                wake,
            ) {
                return Err(());
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn emit<T>(
    events: &SyncSender<T>,
    mut event: T,
    stop: &AtomicBool,
    wake: &ProviderCoordinatorWake,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        match events.try_send(event) {
            Ok(()) => {
                wake.notify();
                return false;
            }
            Err(TrySendError::Disconnected(_)) => return true,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn sleep_cancellable(delay: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        std::thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn is_read_timeout(error: &str) -> bool {
    error.contains("timed out") || error.contains("WouldBlock")
}

fn unix_nanos_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}
