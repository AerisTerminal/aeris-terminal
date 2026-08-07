use super::{
    COMMAND_CAPACITY, CoinbaseDesktopWorker, INBOX_BATCH, LiveLoopState, apply_environment_event,
};
use crate::market_worker::{MarketWorkerCommand, MarketWorkerSender};
use axiusflow_coinbase_market_adapter::CoinbaseProviderEvents;
use axiusflow_platform_runtime::{
    NativeNetworkMonitor, NativePowerMonitor, NetworkEvent, PowerEvent,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const MINIMUM_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const MAXIMUM_RECONNECT_DELAY: Duration = Duration::from_secs(8);

#[derive(Clone, Copy)]
pub(super) enum EnvironmentalEvent {
    Network(NetworkEvent),
    Power(PowerEvent),
}

pub(super) enum WorkerInboxEvent {
    ProviderReady,
    UiDiagnosticsReady,
    Environment(EnvironmentalEvent),
    Command(MarketWorkerCommand),
}

pub(super) struct InboxDrainContext<'a, V: axiusflow_platform_runtime::CredentialVault> {
    pub(super) worker: &'a mut CoinbaseDesktopWorker<V>,
    pub(super) events: &'a CoinbaseProviderEvents,
    pub(super) state: &'a mut LiveLoopState,
    pub(super) message_tx: &'a MarketWorkerSender,
    pub(super) provider_wake_pending: &'a AtomicBool,
}

pub(super) struct ReconnectBackoff {
    delay: Duration,
    retry_at: Option<Instant>,
}

impl ReconnectBackoff {
    pub(super) const fn new() -> Self {
        Self {
            delay: MINIMUM_RECONNECT_DELAY,
            retry_at: None,
        }
    }

    pub(super) fn retry_ready(&mut self, now: Instant) -> bool {
        let Some(retry_at) = self.retry_at else {
            self.retry_at = Some(now + self.delay);
            return false;
        };
        if now < retry_at {
            return false;
        }
        self.retry_at = None;
        self.delay = self
            .delay
            .checked_mul(2)
            .unwrap_or(MAXIMUM_RECONNECT_DELAY)
            .min(MAXIMUM_RECONNECT_DELAY);
        true
    }

    pub(super) fn reset(&mut self) {
        self.delay = MINIMUM_RECONNECT_DELAY;
        self.retry_at = None;
    }

    pub(super) fn wait_duration(&self, recovery_required: bool, now: Instant) -> Option<Duration> {
        if recovery_required {
            self.retry_at
                .map(|retry_at| retry_at.saturating_duration_since(now))
        } else {
            None
        }
    }
}

pub(super) fn forward_commands(
    command_rx: &Receiver<MarketWorkerCommand>,
    inbox_tx: &SyncSender<WorkerInboxEvent>,
) {
    loop {
        match command_rx.recv() {
            Ok(MarketWorkerCommand::Recovery(command)) => {
                if inbox_tx
                    .send(WorkerInboxEvent::Command(MarketWorkerCommand::Recovery(
                        command,
                    )))
                    .is_err()
                {
                    return;
                }
            }
            Ok(MarketWorkerCommand::Shutdown) | Err(_) => {
                let _ = inbox_tx.send(WorkerInboxEvent::Command(MarketWorkerCommand::Shutdown));
                return;
            }
        }
    }
}

pub(super) fn drain_worker_inbox<V: axiusflow_platform_runtime::CredentialVault>(
    inbox_rx: &Receiver<WorkerInboxEvent>,
    ready_event: &mut Option<WorkerInboxEvent>,
    context: &mut InboxDrainContext<'_, V>,
) -> Result<bool, String> {
    for _ in 0..INBOX_BATCH {
        let event = ready_event.take().or_else(|| inbox_rx.try_recv().ok());
        let Some(event) = event else {
            return Ok(false);
        };
        match event {
            WorkerInboxEvent::ProviderReady => {
                context
                    .provider_wake_pending
                    .store(false, Ordering::Release);
            }
            WorkerInboxEvent::UiDiagnosticsReady => {}
            WorkerInboxEvent::Environment(event) => {
                context.state.recovery_announced |= apply_environment_event(
                    context.worker,
                    context.events,
                    event,
                    &mut context.state.prepared,
                    &mut context.state.streaming_generation,
                    &mut context.state.retained,
                    context.message_tx,
                )?;
            }
            WorkerInboxEvent::Command(MarketWorkerCommand::Recovery(command)) => {
                if context.state.pending_recovery.len() >= COMMAND_CAPACITY {
                    return Err("desktop recovery command inbox overflowed".to_string());
                }
                context.state.pending_recovery.push_back(command);
            }
            WorkerInboxEvent::Command(MarketWorkerCommand::Shutdown) => return Ok(true),
        }
    }
    Ok(false)
}

pub(super) fn environment_events(
    sender: SyncSender<WorkerInboxEvent>,
) -> (Option<NetworkEvent>, bool) {
    let network = NativeNetworkMonitor::connect().ok();
    let power = NativePowerMonitor::connect().ok();
    let network_active = if let Some(mut monitor) = network {
        let initial = monitor.current();
        let network_sender = sender.clone();
        let active = thread::Builder::new()
            .name("axiusflow-network-monitor".to_string())
            .spawn(move || {
                while let Ok(event) = monitor.next_event() {
                    if network_sender
                        .send(WorkerInboxEvent::Environment(EnvironmentalEvent::Network(
                            event,
                        )))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .is_ok();
        (Some(initial), active)
    } else {
        (None, false)
    };
    let power_active = if let Some(mut monitor) = power {
        thread::Builder::new()
            .name("axiusflow-power-monitor".to_string())
            .spawn(move || {
                while let Ok(event) = monitor.next_event() {
                    if sender
                        .send(WorkerInboxEvent::Environment(EnvironmentalEvent::Power(
                            event,
                        )))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .is_ok()
    } else {
        false
    };
    (network_active.0, network_active.1 && power_active)
}

pub(super) fn apply_initial_network<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    event: Option<NetworkEvent>,
) -> Result<(), String> {
    if event == Some(NetworkEvent::Unavailable) {
        worker
            .handle_network_event(NetworkEvent::Unavailable)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn wait_for_inbox(
    receiver: &Receiver<WorkerInboxEvent>,
    wait: Option<Duration>,
) -> Result<Option<WorkerInboxEvent>, String> {
    match wait {
        Some(wait) => match receiver.recv_timeout(wait) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err("desktop market inbox disconnected".to_string())
            }
        },
        None => receiver
            .recv()
            .map(Some)
            .map_err(|_| "desktop market inbox disconnected".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MINIMUM_RECONNECT_DELAY, ReconnectBackoff, WorkerInboxEvent, forward_commands,
        wait_for_inbox,
    };
    use crate::market_worker::MarketWorkerCommand;
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn reconnect_backoff_delays_and_resets_attempts() {
        let started = Instant::now();
        let mut backoff = ReconnectBackoff::new();
        assert!(!backoff.retry_ready(started));
        assert_eq!(
            backoff.wait_duration(false, started + MINIMUM_RECONNECT_DELAY),
            None
        );
        assert_eq!(
            backoff.wait_duration(true, started),
            Some(MINIMUM_RECONNECT_DELAY)
        );
        assert!(!backoff.retry_ready(started + MINIMUM_RECONNECT_DELAY / 2));
        assert!(backoff.retry_ready(started + MINIMUM_RECONNECT_DELAY));
        assert_eq!(backoff.delay, MINIMUM_RECONNECT_DELAY * 2);

        backoff.reset();
        assert_eq!(backoff.delay, MINIMUM_RECONNECT_DELAY);
        assert_eq!(backoff.retry_at, None);
    }

    #[test]
    fn disconnected_command_channel_wakes_the_worker_for_shutdown() {
        let (command_tx, command_rx) = mpsc::sync_channel::<MarketWorkerCommand>(1);
        let (inbox_tx, inbox_rx) = mpsc::sync_channel(1);
        let forwarder = thread::spawn(move || forward_commands(&command_rx, &inbox_tx));
        drop(command_tx);
        assert!(matches!(
            inbox_rx.recv().expect("shutdown reaches worker inbox"),
            WorkerInboxEvent::Command(MarketWorkerCommand::Shutdown)
        ));
        forwarder.join().expect("command forwarder stops cleanly");
    }

    #[test]
    fn worker_inbox_wait_uses_reconnect_deadlines_without_polling() {
        let (_sender, receiver) = mpsc::sync_channel(1);
        let started = Instant::now();
        assert!(
            wait_for_inbox(&receiver, Some(Duration::from_millis(5)))
                .expect("deadline wait succeeds")
                .is_none()
        );
        assert!(started.elapsed() >= Duration::from_millis(5));
    }
}
