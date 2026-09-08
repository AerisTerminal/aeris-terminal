//! Engine lifecycle preferences and asynchronous desktop shutdown.

use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum DesktopLifetimeMode {
    #[default]
    KeepEngineWarm,
    KeepMarketsLive,
    ExitWithDesktop,
}

impl DesktopLifetimeMode {
    pub(super) const fn engine_resource_mode(self) -> ResourceMode {
        match self {
            Self::KeepMarketsLive => ResourceMode::MarketsLive,
            Self::KeepEngineWarm | Self::ExitWithDesktop => ResourceMode::Warm,
        }
    }

    pub(super) const fn protocol_mode(self) -> EngineLifetimeMode {
        match self {
            Self::ExitWithDesktop => EngineLifetimeMode::ExitCompletely,
            Self::KeepEngineWarm => EngineLifetimeMode::KeepEngineWarm,
            Self::KeepMarketsLive => EngineLifetimeMode::KeepMarketsLive,
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::ExitWithDesktop => "Exit fully",
            Self::KeepEngineWarm => "Engine warm",
            Self::KeepMarketsLive => "Markets live",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::ExitWithDesktop => {
                "When you close Axiusflow, everything stops. Prices will not keep updating until you open Axiusflow again."
            }
            Self::KeepEngineWarm => {
                "When you close Axiusflow, a small part of the app stays open so Axiusflow can start faster next time. Live prices do not keep updating."
            }
            Self::KeepMarketsLive => {
                "When you close Axiusflow, your selected markets keep receiving live prices in the background. This uses internet data and some computer resources. Turn on Live retention to use this option."
            }
        }
    }

    #[cfg(test)]
    pub(super) const fn next(self, markets_live_permitted: bool) -> Self {
        match (self, markets_live_permitted) {
            (Self::ExitWithDesktop, _) => Self::KeepEngineWarm,
            (Self::KeepEngineWarm, true) => Self::KeepMarketsLive,
            (Self::KeepEngineWarm, false) | (Self::KeepMarketsLive, _) => Self::ExitWithDesktop,
        }
    }

    pub(super) fn from_workspace(workspace: &WorkspaceState) -> Result<Self, String> {
        match EngineLifetimeMode::try_from(workspace.lifetime_mode)
            .map_err(|_| "resident engine returned an invalid lifetime mode".to_string())?
        {
            EngineLifetimeMode::ExitCompletely => Ok(Self::ExitWithDesktop),
            EngineLifetimeMode::KeepEngineWarm => Ok(Self::KeepEngineWarm),
            EngineLifetimeMode::KeepMarketsLive if workspace.markets_live_permitted => {
                Ok(Self::KeepMarketsLive)
            }
            EngineLifetimeMode::KeepMarketsLive => {
                Err("resident engine markets-live mode lacks explicit permission".to_string())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LifecycleToggle {
    AutoStart,
    LiveRetention,
}

impl LifecycleToggle {
    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::AutoStart => "settings_start_automatically",
            Self::LiveRetention => "settings_live_retention",
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::AutoStart => "Start automatically",
            Self::LiveRetention => "Live retention",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::AutoStart => {
                "Start Axiusflow's background service when you sign in to your computer, so Axiusflow is ready faster when you open it."
            }
            Self::LiveRetention => {
                "Allow selected markets to keep receiving live prices after you close Axiusflow. This uses internet data and some computer resources in the background. Turning it off also turns off Markets live."
            }
        }
    }

    pub(super) const fn toggle(self) -> fn(&mut TerminalApp, &mut Context<TerminalApp>) {
        match self {
            Self::AutoStart => TerminalApp::toggle_engine_autostart,
            Self::LiveRetention => TerminalApp::toggle_markets_live_permission,
        }
    }
}

pub(super) struct LifecyclePreferenceRequest {
    pub(super) mode: DesktopLifetimeMode,
    pub(super) autostart_enabled: bool,
    pub(super) markets_live_permitted: bool,
}

type LifecyclePreferenceResult = Result<WorkspaceState, String>;

#[derive(Clone, Copy)]
pub(super) struct LifecyclePresentation {
    pub(super) mode: DesktopLifetimeMode,
    pub(super) autostart_enabled: bool,
    pub(super) markets_live_permitted: bool,
    pub(super) pending: bool,
}

pub(super) fn finish_desktop_shutdown(
    mode: DesktopLifetimeMode,
    detach_failed: bool,
    shutdown_engine: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let shutdown_result = if mode == DesktopLifetimeMode::ExitWithDesktop {
        shutdown_engine()
    } else {
        Ok(())
    };
    match (detach_failed, shutdown_result) {
        (false, Ok(())) => Ok(()),
        (true, Ok(())) => {
            Err("desktop market worker did not detach before its deadline".to_string())
        }
        (false, Err(error)) => Err(error),
        (true, Err(error)) => Err(format!(
            "desktop market worker detach expired; engine shutdown failed: {error}"
        )),
    }
}

#[derive(Clone)]
pub(super) struct DesktopLifecycle {
    mode: Rc<Cell<DesktopLifetimeMode>>,
    autostart_enabled: Rc<Cell<bool>>,
    markets_live_permitted: Rc<Cell<bool>>,
    preference_pending: Rc<Cell<bool>>,
    preference_error: Rc<RefCell<Option<String>>>,
    preference_requests: SyncSender<LifecyclePreferenceRequest>,
    preference_results: Rc<RefCell<Receiver<LifecyclePreferenceResult>>>,
    retirements: Rc<RefCell<Vec<Task<bool>>>>,
    terminals: Rc<RefCell<Vec<WeakEntity<WorkspaceSurface>>>>,
    shutdown_started: Rc<Cell<bool>>,
}

impl DesktopLifecycle {
    pub(super) fn set_preference_error(&self, error: String) {
        *self.preference_error.borrow_mut() = Some(error);
    }

    pub(super) fn new(
        mode: DesktopLifetimeMode,
        autostart_enabled: bool,
        markets_live_permitted: bool,
    ) -> Result<Self, String> {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("axiusflow-engine-lifecycle-client".to_string())
            .spawn(move || run_lifecycle_preferences(&request_rx, &result_tx))
            .map_err(|_| "desktop lifecycle client could not start".to_string())?;
        Ok(Self {
            mode: Rc::new(Cell::new(mode)),
            autostart_enabled: Rc::new(Cell::new(autostart_enabled)),
            markets_live_permitted: Rc::new(Cell::new(markets_live_permitted)),
            preference_pending: Rc::new(Cell::new(false)),
            preference_error: Rc::new(RefCell::new(None)),
            preference_requests: request_tx,
            preference_results: Rc::new(RefCell::new(result_rx)),
            retirements: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
        })
    }

    pub(super) fn presentation(&self) -> LifecyclePresentation {
        LifecyclePresentation {
            mode: self.mode.get(),
            autostart_enabled: self.autostart_enabled.get(),
            markets_live_permitted: self.markets_live_permitted.get(),
            pending: self.preference_pending.get(),
        }
    }

    pub(super) fn preference_error(&self) -> Option<String> {
        self.preference_error.borrow().clone()
    }

    pub(super) fn request_preferences(
        &self,
        request: LifecyclePreferenceRequest,
    ) -> Result<(), String> {
        if self.preference_pending.get() {
            return Ok(());
        }
        self.preference_requests
            .try_send(request)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "engine lifecycle update is already pending".to_string()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "engine lifecycle client is unavailable".to_string()
                }
            })?;
        self.preference_pending.set(true);
        self.preference_error.borrow_mut().take();
        Ok(())
    }

    pub(super) fn poll_preferences(&self) -> bool {
        match self.preference_results.borrow().try_recv() {
            Ok(Ok(workspace)) => {
                match DesktopLifetimeMode::from_workspace(&workspace) {
                    Ok(mode) => {
                        self.mode.set(mode);
                        self.autostart_enabled.set(workspace.autostart_enabled);
                        self.markets_live_permitted
                            .set(workspace.markets_live_permitted);
                        self.preference_error.borrow_mut().take();
                    }
                    Err(error) => *self.preference_error.borrow_mut() = Some(error),
                }
                self.preference_pending.set(false);
                true
            }
            Ok(Err(error)) => {
                *self.preference_error.borrow_mut() = Some(error);
                self.preference_pending.set(false);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                if self.preference_pending.replace(false) {
                    *self.preference_error.borrow_mut() =
                        Some("engine lifecycle client stopped unexpectedly".to_string());
                    return true;
                }
                false
            }
        }
    }

    pub(super) fn register_terminal(&self, terminal: &Entity<WorkspaceSurface>) {
        self.terminals.borrow_mut().push(terminal.downgrade());
    }

    pub(super) fn retire_market_worker(&self, retirement: MarketWorkerRetirement, cx: &App) {
        self.retirements.borrow_mut().push(
            cx.background_executor()
                .spawn(async move { retirement.wait() }),
        );
    }

    pub(super) fn begin_quit(&self, cx: &mut App) -> Option<Task<Result<(), String>>> {
        if self.shutdown_started.replace(true) {
            return None;
        }
        let terminals = self.terminals.borrow_mut().drain(..).collect::<Vec<_>>();
        for terminal in terminals {
            terminal
                .update(cx, |terminal, terminal_cx| {
                    terminal.retire_market_worker(terminal_cx);
                })
                .ok();
        }
        let retirements = self.retirements.borrow_mut().drain(..).collect::<Vec<_>>();
        let mode = self.mode.get();
        Some(cx.background_executor().spawn(async move {
            let mut detach_failed = false;
            for retirement in retirements {
                if !retirement.await {
                    detach_failed = true;
                }
            }
            finish_desktop_shutdown(mode, detach_failed, || {
                axiusflow_local_engine_client::shutdown_running_engine()
            })
        }))
    }

    pub(super) fn quit_after_shutdown(&self, cx: &mut App) {
        let Some(shutdown) = self.begin_quit(cx) else {
            cx.quit();
            return;
        };
        cx.spawn(async move |cx| {
            if let Err(error) = shutdown.await {
                eprintln!("Axiusflow desktop shutdown failed: {error}");
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

fn run_lifecycle_preferences(
    requests: &Receiver<LifecyclePreferenceRequest>,
    results: &SyncSender<LifecyclePreferenceResult>,
) {
    while let Ok(request) = requests.recv() {
        let result = axiusflow_local_engine_client::sibling_engine_executable()
            .and_then(|executable| {
                axiusflow_local_engine_client::connect_or_start_engine(&executable)
            })
            .and_then(|mut client| {
                let workspace = client.restore_workspace()?;
                client.set_engine_lifecycle(
                    workspace.workspace_revision,
                    request.mode.protocol_mode(),
                    request.autostart_enabled,
                    request.markets_live_permitted,
                )
            });
        if results.send(result).is_err() {
            return;
        }
    }
}
