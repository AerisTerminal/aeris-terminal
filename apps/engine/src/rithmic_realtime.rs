//! Engine-owned Rithmic trade-session lifecycle.

use std::{
    num::NonZeroUsize,
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use axiusflow_desktop_provider_runtime::{
    DesktopProviderConfig, DesktopProviderRuntime, InstrumentDescriptor, ProviderSessionEvent,
};
use axiusflow_local_engine_protocol::InstallProviderInstrument;
use axiusflow_market_data::{DepthSnapshot, MarketEvent, MarketTrade};
use axiusflow_platform_runtime::{
    NativeCredentialVault, NativeNetworkMonitor, NativePowerMonitor, NetworkEvent, PowerEvent,
};
use axiusflow_rithmic_protocol_adapter::{
    AppliedRithmicEvent, MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicCallbackLimits, RithmicEnvironmentEvent,
    RithmicProviderConfig, RithmicProviderDriver, RithmicProviderEvents, RithmicProviderInstrument,
    RithmicRetryScheduler, RithmicSessionLimits, apply_rithmic_environment_event,
    try_recv_rithmic_event,
};

const CALLBACK_CAPACITY: usize = 256;
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const EVENT_WAIT: Duration = Duration::from_millis(16);
const MESSAGE_SILENCE: Duration = Duration::from_mins(2);
const ENVIRONMENT_CAPACITY: usize = 8;

pub(crate) enum RithmicRealtimeControl {
    Select(InstallProviderInstrument),
}

pub(crate) enum RithmicRealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Trade(u64, MarketTrade),
    Depth(u64, DepthSnapshot),
    Heartbeat(u64),
    Recovering(u64),
    Disconnected(u64),
}

type Runtime = DesktopProviderRuntime<NativeCredentialVault, RithmicProviderDriver>;

enum EnvironmentMessage {
    Event(RithmicEnvironmentEvent),
    Failed,
}

#[derive(Clone, Copy)]
struct EnvironmentState {
    network: NetworkEvent,
    suspended: bool,
}

impl EnvironmentState {
    fn observe(&mut self, event: RithmicEnvironmentEvent) {
        match event {
            RithmicEnvironmentEvent::Network(network) => self.network = network,
            RithmicEnvironmentEvent::Power(PowerEvent::Suspending) => self.suspended = true,
            RithmicEnvironmentEvent::Power(PowerEvent::Resumed) => self.suspended = false,
        }
    }
}

pub(crate) fn run(
    controls: &Receiver<RithmicRealtimeControl>,
    publications: &SyncSender<RithmicRealtimeEvent>,
) {
    let Ok((environment, mut environment_state)) = start_environment_monitors() else {
        while let Ok(RithmicRealtimeControl::Select(selected)) = controls.recv() {
            let _ = publications.send(RithmicRealtimeEvent::Disconnected(
                selected.session_generation,
            ));
        }
        return;
    };
    let mut last_generation = 0_u64;
    let Ok(RithmicRealtimeControl::Select(mut selected)) = controls.recv() else {
        return;
    };
    loop {
        while let Ok(RithmicRealtimeControl::Select(newer)) = controls.try_recv() {
            selected = newer;
        }
        let generation = next_generation(last_generation, selected.session_generation);
        match run_selection(
            &selected,
            generation,
            controls,
            publications,
            &environment,
            &mut environment_state,
        ) {
            SelectionExit::Replace {
                selected: replacement,
                generation,
            } => {
                selected = replacement;
                last_generation = generation;
            }
            SelectionExit::Closed { generation } => {
                let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
                return;
            }
        }
    }
}

enum SelectionExit {
    Replace {
        selected: InstallProviderInstrument,
        generation: u64,
    },
    Closed {
        generation: u64,
    },
}

fn run_selection(
    selected: &InstallProviderInstrument,
    mut generation: u64,
    controls: &Receiver<RithmicRealtimeControl>,
    publications: &SyncSender<RithmicRealtimeEvent>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
) -> SelectionExit {
    let opened = open_runtime(selected);
    let Ok((mut runtime, events)) = opened else {
        let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
        return wait_for_replacement(controls, environment, environment_state, generation);
    };
    let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
    let mut retries = RithmicRetryScheduler::default();
    if apply_current_environment(&mut runtime, &events, &mut retries, *environment_state).is_err() {
        let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
    }
    loop {
        match poll_environment(
            environment,
            environment_state,
            &mut runtime,
            &events,
            &mut retries,
            publications,
            generation,
        ) {
            Ok(Some(updated)) => generation = updated,
            Ok(None) => {}
            Err(()) => {
                let _ = runtime.stop();
                return SelectionExit::Closed { generation };
            }
        }
        match controls.try_recv() {
            Ok(RithmicRealtimeControl::Select(replacement)) => {
                let _ = runtime.stop();
                return SelectionExit::Replace {
                    selected: replacement,
                    generation,
                };
            }
            Err(TryRecvError::Disconnected) => {
                let _ = runtime.stop();
                return SelectionExit::Closed { generation };
            }
            Err(TryRecvError::Empty) => {}
        }
        while events.has_ready() {
            match try_recv_rithmic_event(&mut runtime, &events, &mut retries, Instant::now()) {
                Ok(Some(AppliedRithmicEvent::Semantic(event))) => match event {
                    ProviderSessionEvent::InstrumentsDiscovered { .. } => {
                        let _ = publications.send(RithmicRealtimeEvent::Connected(generation));
                    }
                    ProviderSessionEvent::Market {
                        event: MarketEvent::Trade(mut trade),
                        ..
                    } => {
                        trade.metadata.session_generation = generation;
                        let _ = publications.send(RithmicRealtimeEvent::Trade(generation, trade));
                    }
                    ProviderSessionEvent::Market {
                        event: MarketEvent::DepthSnapshot(mut snapshot),
                        ..
                    } => {
                        snapshot.metadata.session_generation = generation;
                        let _ =
                            publications.send(RithmicRealtimeEvent::Depth(generation, snapshot));
                    }
                    ProviderSessionEvent::Heartbeat { .. } => {
                        let _ = publications.send(RithmicRealtimeEvent::Heartbeat(generation));
                    }
                    ProviderSessionEvent::DiscoveryStarted
                    | ProviderSessionEvent::SystemsDiscovered { .. }
                    | ProviderSessionEvent::AuthenticationChanged { .. }
                    | ProviderSessionEvent::Market { .. }
                    | ProviderSessionEvent::Invalidated { .. }
                    | ProviderSessionEvent::Stopped => {}
                },
                Ok(Some(AppliedRithmicEvent::RetryScheduled(_))) => {
                    let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
                }
                Ok(Some(AppliedRithmicEvent::TerminalFailure { .. })) | Err(_) => {
                    let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
                    let _ = runtime.stop();
                    return wait_for_replacement(
                        controls,
                        environment,
                        environment_state,
                        generation,
                    );
                }
                Ok(None) => break,
            }
        }
        if retries
            .retry_due(&mut runtime, Instant::now())
            .is_ok_and(|started| started.is_some())
        {
            generation = next_generation(generation, generation);
            let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
        }
        std::thread::sleep(EVENT_WAIT);
    }
}

fn poll_environment(
    environment: &Receiver<EnvironmentMessage>,
    state: &mut EnvironmentState,
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    publications: &SyncSender<RithmicRealtimeEvent>,
    generation: u64,
) -> Result<Option<u64>, ()> {
    let event = match environment.try_recv() {
        Ok(EnvironmentMessage::Event(event)) => event,
        Ok(EnvironmentMessage::Failed) | Err(TryRecvError::Disconnected) => return Err(()),
        Err(TryRecvError::Empty) => return Ok(None),
    };
    state.observe(event);
    match apply_rithmic_environment_event(runtime, events, retries, event) {
        Ok(Some(_)) => {
            let generation = next_generation(generation, generation);
            let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
            Ok(Some(generation))
        }
        Ok(None) => {
            let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
            Ok(None)
        }
        Err(_) => {
            let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
            Ok(None)
        }
    }
}

fn wait_for_replacement(
    controls: &Receiver<RithmicRealtimeControl>,
    environment: &Receiver<EnvironmentMessage>,
    environment_state: &mut EnvironmentState,
    generation: u64,
) -> SelectionExit {
    loop {
        match environment.try_recv() {
            Ok(EnvironmentMessage::Event(event)) => environment_state.observe(event),
            Ok(EnvironmentMessage::Failed) | Err(TryRecvError::Disconnected) => {
                return SelectionExit::Closed { generation };
            }
            Err(TryRecvError::Empty) => {}
        }
        match controls.recv_timeout(EVENT_WAIT) {
            Ok(RithmicRealtimeControl::Select(selected)) => {
                return SelectionExit::Replace {
                    selected,
                    generation,
                };
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return SelectionExit::Closed { generation };
            }
        }
    }
}

fn apply_current_environment(
    runtime: &mut Runtime,
    events: &RithmicProviderEvents,
    retries: &mut RithmicRetryScheduler,
    state: EnvironmentState,
) -> Result<(), String> {
    if state.network == NetworkEvent::Unavailable {
        apply_rithmic_environment_event(
            runtime,
            events,
            retries,
            RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable),
        )
        .map_err(|_| "Rithmic native network state could not be applied".to_string())?;
    }
    if state.suspended {
        apply_rithmic_environment_event(
            runtime,
            events,
            retries,
            RithmicEnvironmentEvent::Power(PowerEvent::Suspending),
        )
        .map_err(|_| "Rithmic native power state could not be applied".to_string())?;
    }
    if state.network != NetworkEvent::Unavailable && !state.suspended {
        runtime
            .request_connection()
            .map_err(|_| "Rithmic connection could not start".to_string())?;
    }
    Ok(())
}

fn start_environment_monitors() -> Result<(Receiver<EnvironmentMessage>, EnvironmentState), String>
{
    let network = NativeNetworkMonitor::connect()
        .map_err(|_| "Rithmic native network monitor is unavailable".to_string())?;
    let initial_network = network.current();
    let power = NativePowerMonitor::connect()
        .map_err(|_| "Rithmic native power monitor is unavailable".to_string())?;
    let (sender, receiver) = mpsc::sync_channel(ENVIRONMENT_CAPACITY);
    let mut network = network;
    spawn_environment_monitor(
        "axiusflow-engine-rithmic-network-monitor",
        sender.clone(),
        move || {
            network
                .next_event()
                .map(RithmicEnvironmentEvent::Network)
                .map_err(|_| ())
        },
    )?;
    let mut power = power;
    spawn_environment_monitor(
        "axiusflow-engine-rithmic-power-monitor",
        sender,
        move || {
            power
                .next_event()
                .map(RithmicEnvironmentEvent::Power)
                .map_err(|_| ())
        },
    )?;
    Ok((
        receiver,
        EnvironmentState {
            network: initial_network,
            suspended: false,
        },
    ))
}

fn spawn_environment_monitor(
    name: &'static str,
    sender: SyncSender<EnvironmentMessage>,
    mut next: impl FnMut() -> Result<RithmicEnvironmentEvent, ()> + Send + 'static,
) -> Result<(), String> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            loop {
                match next() {
                    Ok(event) if sender.send(EnvironmentMessage::Event(event)).is_ok() => {}
                    Ok(_) => return,
                    Err(()) => {
                        let _ = sender.send(EnvironmentMessage::Failed);
                        return;
                    }
                }
            }
        })
        .map(|_| ())
        .map_err(|_| "Rithmic native environment monitor could not start".to_string())
}

fn open_runtime(
    selected: &InstallProviderInstrument,
) -> Result<(Runtime, RithmicProviderEvents), String> {
    let descriptor = InstrumentDescriptor {
        instrument_id: selected.instrument_id.clone(),
        provider_symbol: selected.provider_symbol.clone(),
        display_symbol: selected.display_symbol.clone(),
        venue_id: selected.venue_id.clone(),
        price_scale: u8::try_from(selected.price_scale)
            .map_err(|_| "Rithmic live price scale is invalid".to_string())?,
        quantity_scale: u8::try_from(selected.quantity_scale)
            .map_err(|_| "Rithmic live quantity scale is invalid".to_string())?,
    };
    let provider = RithmicProviderConfig::try_new(
        "Axiusflow",
        env!("CARGO_PKG_VERSION"),
        RithmicSessionLimits::default(),
        MESSAGE_SILENCE,
        vec![RithmicProviderInstrument {
            descriptor,
            entitlement_id: selected.entitlement_id.clone(),
            trades: true,
            quotes: false,
            order_book: true,
        }],
    )
    .map_err(|_| "Rithmic live provider configuration is invalid".to_string())?;
    let limits = RithmicCallbackLimits::try_new(
        nonzero(CALLBACK_CAPACITY),
        nonzero(CALLBACK_BYTES),
        NonZeroUsize::MIN,
    )
    .map_err(|_| "Rithmic live callback limits are invalid".to_string())?;
    let (driver, events) = RithmicProviderDriver::new(provider, limits);
    let vault = NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE)
        .map_err(|_| "Rithmic live credential vault is unavailable".to_string())?;
    let runtime = DesktopProviderRuntime::try_new(
        vault,
        driver,
        RITHMIC_TEST_VAULT_KEY,
        DesktopProviderConfig::new(nonzero(32), nonzero(MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES)),
    )
    .map_err(|_| "Rithmic live runtime is unavailable".to_string())?;
    Ok((runtime, events))
}

const fn next_generation(previous: u64, requested: u64) -> u64 {
    let incremented = previous.saturating_add(1);
    if requested > incremented {
        requested
    } else {
        incremented
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentState, next_generation};
    use axiusflow_platform_runtime::{NetworkEvent, PowerEvent};
    use axiusflow_rithmic_protocol_adapter::RithmicEnvironmentEvent;

    #[test]
    fn engine_generation_never_regresses_across_catalog_replacements_and_retries() {
        assert_eq!(next_generation(0, 7), 7);
        assert_eq!(next_generation(7, 7), 8);
        assert_eq!(next_generation(8, 12), 12);
        assert_eq!(next_generation(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn native_environment_state_survives_provider_selection_replacement() {
        let mut state = EnvironmentState {
            network: NetworkEvent::Available,
            suspended: false,
        };
        state.observe(RithmicEnvironmentEvent::Network(NetworkEvent::Unavailable));
        state.observe(RithmicEnvironmentEvent::Power(PowerEvent::Suspending));
        assert_eq!(state.network, NetworkEvent::Unavailable);
        assert!(state.suspended);
        state.observe(RithmicEnvironmentEvent::Network(NetworkEvent::Available));
        state.observe(RithmicEnvironmentEvent::Power(PowerEvent::Resumed));
        assert_eq!(state.network, NetworkEvent::Available);
        assert!(!state.suspended);
    }
}
