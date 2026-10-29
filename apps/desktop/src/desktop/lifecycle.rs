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
    trading_stores: Rc<RefCell<Vec<WeakEntity<trading_store::TradingStore>>>>,
    shutdown_started: Rc<Cell<bool>>,
    services: aeris_desktop::services::DesktopServices,
    context: Option<aeris_context_runtime::ContextService>,
    instance_lock: Option<Arc<aeris_platform_runtime::InstanceLock>>,
    #[cfg(test)]
    fixture_without_trading_bridge: bool,
}

impl DesktopLifecycle {
    #[must_use]
    pub(super) fn new(
        services: aeris_desktop::services::DesktopServices,
        context: aeris_context_runtime::ContextService,
        instance_lock: Arc<aeris_platform_runtime::InstanceLock>,
    ) -> Self {
        Self {
            retirements: Rc::new(RefCell::new(Vec::new())),
            workspace_persistence: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            trading_stores: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
            services,
            context: Some(context),
            instance_lock: Some(instance_lock),
            #[cfg(test)]
            fixture_without_trading_bridge: false,
        }
    }

    #[cfg(test)]
    pub(super) fn new_for_test() -> Self {
        Self {
            retirements: Rc::new(RefCell::new(Vec::new())),
            workspace_persistence: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            trading_stores: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
            services: aeris_desktop::services::DesktopServices::new(
                aeris_market_runtime::MarketService::start().expect("test market owner"),
                super::tests::test_trading_service(),
                aeris_account_runtime::AccountService::new(
                    aeris_account_runtime::AccountServiceConfig::production(),
                ),
                aeris_desktop::chart_chrome::DesktopPersistence::new(
                    std::env::temp_dir()
                        .join(format!("aeris-desktop-lifecycle-{}", std::process::id())),
                ),
                Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
            ),
            context: None,
            instance_lock: None,
            fixture_without_trading_bridge: true,
        }
    }

    #[cfg(test)]
    pub(super) fn new_for_test_with_trading(
        trading: aeris_trading_runtime::TradingService,
    ) -> Self {
        let mut lifecycle = Self::new_for_test();
        lifecycle.services.trading = trading;
        lifecycle
    }

    #[cfg(test)]
    pub(super) fn fixture_without_trading_bridge(&self) -> bool {
        self.fixture_without_trading_bridge
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

    pub(super) fn register_trading_store(&self, store: &Entity<trading_store::TradingStore>) {
        self.trading_stores.borrow_mut().push(store.downgrade());
    }

    /// The instance's subscription view, shared by pane-originated actions.
    pub(super) fn trading_store(&self) -> Option<Entity<trading_store::TradingStore>> {
        self.trading_stores.borrow().last()?.upgrade()
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
        let trading_bridges = self
            .trading_stores
            .borrow_mut()
            .drain(..)
            .filter_map(|store| store.update(cx, |store, _| store.stop()).ok().flatten())
            .collect::<Vec<_>>();
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
        let market = self.services.market.clone();
        let trading = self.services.trading.clone();
        let context = self.context.clone();
        let instance_lock = self.instance_lock.clone();
        Some(cx.background_executor().spawn(async move {
            let mut account_failure = None;
            let mut failure = None;
            for bridge in trading_bridges {
                if bridge.join().is_err() {
                    failure = Some("desktop trading bridge stopped unexpectedly".to_string());
                }
            }
            if let Err(error) =
                retain_account_refresh_quiesce_for_exit(account_refresh, "desktop exit")
            {
                account_failure = Some(error);
            }
            // The one workspace writer owns chart chrome too, so these waits drain both.
            for persistence in workspace_persistence {
                if let Err(error) = persistence.wait(Duration::from_secs(2)) {
                    failure = Some(error);
                }
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
            shutdown_outcome(account_failure, failure)
        }))
    }

    #[cfg(test)]
    pub(super) fn shutdown_started(&self) -> bool {
        self.shutdown_started.get()
    }

    #[cfg(test)]
    pub(super) fn pending_workspace_persistence_waits(&self) -> usize {
        self.workspace_persistence.borrow().len()
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
                    super::flush_diagnostics_before_exit();
                    std::process::exit(1);
                }
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

/// An account refresh that did not reach durability blocks the exit, because exiting then
/// could lose a rotated refresh token; any other shutdown failure is reported and the
/// process still quits.
fn shutdown_outcome(
    account_failure: Option<String>,
    failure: Option<String>,
) -> Result<(), DesktopShutdownError> {
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
}

#[cfg(test)]
mod tests {
    use super::{DesktopLifecycle, shutdown_outcome};

    #[test]
    fn an_unsettled_account_refresh_blocks_exit_over_any_other_failure() {
        let blocked = shutdown_outcome(
            Some("account refresh did not settle".to_string()),
            Some("market owner did not stop".to_string()),
        )
        .expect_err("an unsettled refresh fails shutdown");
        assert!(blocked.blocks_exit());
        assert_eq!(blocked.to_string(), "account refresh did not settle");

        let reported = shutdown_outcome(None, Some("market owner did not stop".to_string()))
            .expect_err("an owner failure is reported");
        assert!(!reported.blocks_exit());
        assert!(shutdown_outcome(None, None).is_ok());
    }

    #[gpui::test]
    async fn a_duplicate_quit_request_gets_no_shutdown_of_its_own(cx: &mut gpui::TestAppContext) {
        let lifecycle = DesktopLifecycle::new_for_test();
        let first = cx
            .update(|cx| lifecycle.begin_quit(cx))
            .expect("the first quit request owns the shutdown");
        assert!(lifecycle.shutdown_started());
        assert!(
            cx.update(|cx| lifecycle.begin_quit(cx)).is_none(),
            "a duplicate request must not start a second shutdown that could bypass the first's fences"
        );
        cx.update(|cx| lifecycle.quit_after_shutdown(cx));
        // The in-memory test trading store cannot checkpoint its WAL, so the first shutdown
        // reports that owner failure; it still drained the account fence and may exit.
        if let Err(error) = first.await {
            assert!(
                !error.blocks_exit(),
                "the first owner drains every fence: {error}"
            );
        }
    }
}
