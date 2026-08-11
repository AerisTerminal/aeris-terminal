//! Engine-owned Rithmic trade-session lifecycle.

use std::{
    num::NonZeroUsize,
    sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    time::{Duration, Instant},
};

use axiusflow_desktop_provider_runtime::{
    DesktopProviderConfig, DesktopProviderRuntime, InstrumentDescriptor, ProviderSessionEvent,
};
use axiusflow_local_engine_protocol::InstallProviderInstrument;
use axiusflow_market_data::{MarketEvent, MarketTrade};
use axiusflow_platform_runtime::NativeCredentialVault;
use axiusflow_rithmic_protocol_adapter::{
    AppliedRithmicEvent, MAXIMUM_RITHMIC_CREDENTIAL_BLOB_BYTES, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicCallbackLimits, RithmicProviderConfig,
    RithmicProviderDriver, RithmicProviderEvents, RithmicProviderInstrument, RithmicRetryScheduler,
    RithmicSessionLimits, try_recv_rithmic_event,
};

const CALLBACK_CAPACITY: usize = 256;
const CALLBACK_BYTES: usize = 8 * 1024 * 1024;
const EVENT_WAIT: Duration = Duration::from_millis(16);
const MESSAGE_SILENCE: Duration = Duration::from_mins(2);

pub(crate) enum RithmicRealtimeControl {
    Select(InstallProviderInstrument),
}

pub(crate) enum RithmicRealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Trade(u64, MarketTrade),
    Heartbeat(u64),
    Recovering(u64),
    Disconnected(u64),
}

type Runtime = DesktopProviderRuntime<NativeCredentialVault, RithmicProviderDriver>;

pub(crate) fn run(
    controls: &Receiver<RithmicRealtimeControl>,
    publications: &SyncSender<RithmicRealtimeEvent>,
) {
    let mut last_generation = 0_u64;
    let Ok(RithmicRealtimeControl::Select(mut selected)) = controls.recv() else {
        return;
    };
    loop {
        while let Ok(RithmicRealtimeControl::Select(newer)) = controls.try_recv() {
            selected = newer;
        }
        let generation = next_generation(last_generation, selected.session_generation);
        match run_selection(&selected, generation, controls, publications) {
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
) -> SelectionExit {
    let opened = open_runtime(selected);
    let Ok((mut runtime, events)) = opened else {
        let _ = publications.send(RithmicRealtimeEvent::Disconnected(generation));
        return wait_for_replacement(controls, generation);
    };
    let _ = publications.send(RithmicRealtimeEvent::Connecting(generation));
    if runtime.request_connection().is_err() {
        let _ = publications.send(RithmicRealtimeEvent::Recovering(generation));
    }
    let mut retries = RithmicRetryScheduler::default();
    loop {
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
                        event: MarketEvent::Trade(trade),
                        ..
                    } => {
                        let _ = publications.send(RithmicRealtimeEvent::Trade(generation, trade));
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
                    return wait_for_replacement(controls, generation);
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

fn wait_for_replacement(
    controls: &Receiver<RithmicRealtimeControl>,
    generation: u64,
) -> SelectionExit {
    loop {
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
            order_book: false,
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
    use super::next_generation;

    #[test]
    fn engine_generation_never_regresses_across_catalog_replacements_and_retries() {
        assert_eq!(next_generation(0, 7), 7);
        assert_eq!(next_generation(7, 7), 8);
        assert_eq!(next_generation(8, 12), 12);
        assert_eq!(next_generation(u64::MAX, 1), u64::MAX);
    }
}
