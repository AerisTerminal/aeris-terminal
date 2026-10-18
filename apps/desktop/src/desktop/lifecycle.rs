//! Single-process desktop shutdown coordination.

use super::*;
use aeris_observability::diagnostic;

pub(super) struct DesktopShutdownError {
    detail: String,
    blocks_exit: bool,
}

impl std::fmt::Display for DesktopShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl DesktopShutdownError {
    pub(super) const fn blocks_exit(&self) -> bool {
        self.blocks_exit
    }
}

#[derive(Clone)]
pub(super) struct DesktopLifecycle {
    retirements: Rc<RefCell<Vec<Task<bool>>>>,
    workspace_persistence: Rc<RefCell<Vec<WorkspaceLayoutShutdownWait>>>,
    terminals: Rc<RefCell<Vec<WeakEntity<WorkspaceSurface>>>>,
    shutdown_started: Rc<Cell<bool>>,
    services: aeris_desktop::services::DesktopServices,
    context: Option<aeris_context_runtime::ContextService>,
    instance_lock: Option<Arc<aeris_platform_runtime::InstanceLock>>,
}

impl DesktopLifecycle {
    #[must_use]
    pub(super) fn new(
        services: aeris_desktop::services::DesktopServices,
        context: aeris_context_runtime::ContextService,
        instance_lock: aeris_platform_runtime::InstanceLock,
    ) -> Self {
        Self {
            retirements: Rc::new(RefCell::new(Vec::new())),
            workspace_persistence: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
            services,
            context: Some(context),
            instance_lock: Some(Arc::new(instance_lock)),
        }
    }

    #[cfg(test)]
    pub(super) fn new_for_test() -> Self {
        Self {
            retirements: Rc::new(RefCell::new(Vec::new())),
            workspace_persistence: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
            services: aeris_desktop::services::DesktopServices::new(
                aeris_market_runtime::MarketService::start().expect("test market owner"),
                super::tests::test_trading_service(),
                aeris_account_runtime::AccountService::new(
                    aeris_account_runtime::AccountServiceConfig::from_environment(),
                ),
                aeris_desktop::chart_chrome::DesktopPersistence::new(
                    std::env::temp_dir()
                        .join(format!("aeris-desktop-lifecycle-{}", std::process::id())),
                ),
                Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
            ),
            context: None,
            instance_lock: None,
        }
    }

    pub(super) fn context_view(&self) -> Option<aeris_context_runtime::ContextView> {
        self.context
            .as_ref()
            .map(aeris_context_runtime::ContextService::view)
    }

    pub(super) fn trading(&self) -> &aeris_trading_runtime::TradingService {
        &self.services.trading
    }

    pub(super) fn clock(&self) -> &dyn aeris_trading_runtime::clock::Clock {
        self.services.clock.as_ref()
    }

    pub(super) fn trading_feedback(&self) -> &aeris_desktop::trading::TradingFeedback {
        &self.services.trading_feedback
    }

    pub(super) fn market(&self) -> &aeris_market_runtime::MarketService {
        &self.services.market
    }

    pub(super) fn account(&self) -> Option<&aeris_desktop::account::DesktopAccount> {
        self.services.account.as_ref()
    }

    pub(super) fn account_service(&self) -> &aeris_account_runtime::AccountService {
        &self.services.account_service
    }

    pub(super) fn persistence(&self) -> &aeris_desktop::chart_chrome::DesktopPersistence {
        &self.services.persistence
    }

    pub(super) fn context_service(&self) -> Option<aeris_context_runtime::ContextService> {
        self.context.clone()
    }

    pub(super) fn set_context_publication_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        if let Some(context) = &self.context {
            context.set_publication_wake(wake);
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

    pub(super) fn await_workspace_persistence(&self, wait: WorkspaceLayoutShutdownWait) {
        self.workspace_persistence.borrow_mut().push(wait);
    }

    pub(super) fn begin_quit(
        &self,
        cx: &mut App,
    ) -> Option<Task<Result<(), DesktopShutdownError>>> {
        if self.shutdown_started.replace(true) {
            return None;
        }
        let account_refresh = self.account_service().begin_refresh_quiesce();
        let terminals = self.terminals.borrow_mut().drain(..).collect::<Vec<_>>();
        for terminal in terminals {
            terminal
                .update(cx, |terminal, terminal_cx| {
                    terminal.retire_market_worker(terminal_cx);
                })
                .ok();
        }
        let workspace_persistence = self
            .workspace_persistence
            .borrow_mut()
            .drain(..)
            .collect::<Vec<_>>();
        let retirements = self.retirements.borrow_mut().drain(..).collect::<Vec<_>>();
        let persistence = self.services.persistence.clone();
        let chart_chrome_persistence = persistence.chart_chrome_shutdown_wait();
        let market = self.services.market.clone();
        let trading = self.services.trading.clone();
        let context = self.context.clone();
        let instance_lock = self.instance_lock.clone();
        Some(cx.background_executor().spawn(async move {
            let mut account_failure = None;
            let mut failure = None;
            if let Err(error) =
                retain_account_refresh_quiesce_for_exit(account_refresh, "desktop exit")
            {
                account_failure = Some(error);
            }
            for persistence in workspace_persistence {
                if let Err(error) = persistence.wait(Duration::from_secs(2)) {
                    failure = Some(error);
                }
            }
            match chart_chrome_persistence.wait(Duration::from_secs(2)) {
                Ok(generation)
                    if persistence.chart_chrome_shutdown_generation_is_current(generation) => {}
                Ok(_) => {
                    failure = Some(
                        "chart preferences changed while desktop shutdown was preparing"
                            .to_string(),
                    );
                }
                Err(error) => failure = Some(error),
            }
            for retirement in retirements {
                if !retirement.await {
                    failure = Some(
                        "desktop market worker did not retire before its deadline".to_string(),
                    );
                }
            }
            // Provider sessions log out only through the runtime shutdown;
            // process exit alone drops the plant sockets without a logout.
            if let Err(error) = market.shutdown(Duration::from_secs(2)) {
                failure = Some(error);
            }
            if let Err(error) = trading.shutdown(Duration::from_secs(2)) {
                failure = Some(error);
            }
            if let Some(context) = context
                && let Err(error) = context.shutdown(Duration::from_secs(2))
            {
                failure = Some(error);
            }
            drop(instance_lock);
            if let Some(detail) = account_failure {
                Err(DesktopShutdownError {
                    detail,
                    blocks_exit: true,
                })
            } else if let Some(detail) = failure {
                Err(DesktopShutdownError {
                    detail,
                    blocks_exit: false,
                })
            } else {
                Ok(())
            }
        }))
    }

    pub(super) fn quit_after_shutdown(&self, cx: &mut App) {
        self.quit_after_shutdown_attempt(cx);
    }

    fn quit_after_shutdown_attempt(&self, cx: &mut App) {
        let Some(shutdown) = self.begin_quit(cx) else {
            // The first shutdown owner is still responsible for the eventual
            // quit. A duplicate request must not bypass its durability fences.
            return;
        };
        cx.spawn(async move |cx| {
            if let Err(error) = shutdown.await {
                let blocks_exit = error.blocks_exit;
                diagnostic!("Aeris desktop shutdown failed: {error}");
                if blocks_exit {
                    let _ = aeris_observability::flush_diagnostics();
                    std::process::exit(1);
                }
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}
