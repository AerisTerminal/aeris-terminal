//! Single-process desktop shutdown coordination.

use super::*;

#[derive(Clone)]
pub(super) struct DesktopLifecycle {
    retirements: Rc<RefCell<Vec<Task<bool>>>>,
    workspace_persistence: Rc<RefCell<Vec<WorkspaceLayoutShutdownWait>>>,
    terminals: Rc<RefCell<Vec<WeakEntity<WorkspaceSurface>>>>,
    shutdown_started: Rc<Cell<bool>>,
}

impl DesktopLifecycle {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            retirements: Rc::new(RefCell::new(Vec::new())),
            workspace_persistence: Rc::new(RefCell::new(Vec::new())),
            terminals: Rc::new(RefCell::new(Vec::new())),
            shutdown_started: Rc::new(Cell::new(false)),
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
        let workspace_persistence = self
            .workspace_persistence
            .borrow_mut()
            .drain(..)
            .collect::<Vec<_>>();
        let retirements = self.retirements.borrow_mut().drain(..).collect::<Vec<_>>();
        let chart_chrome_persistence = chart_chrome::chart_chrome_shutdown_wait();
        Some(cx.background_executor().spawn(async move {
            let mut failure = None;
            for persistence in workspace_persistence {
                if let Err(error) = persistence.wait(Duration::from_secs(2)) {
                    failure = Some(error);
                }
            }
            match chart_chrome_persistence.wait(Duration::from_secs(2)) {
                Ok(generation)
                    if chart_chrome::chart_chrome_shutdown_generation_is_current(generation) => {}
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
            failure.map_or(Ok(()), Err)
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
