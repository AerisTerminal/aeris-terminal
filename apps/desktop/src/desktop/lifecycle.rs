//! Single-process desktop shutdown coordination.

use super::*;

#[derive(Clone)]
pub(super) struct DesktopLifecycle {
    retirements: Rc<RefCell<Vec<Task<bool>>>>,
    terminals: Rc<RefCell<Vec<WeakEntity<WorkspaceSurface>>>>,
    shutdown_started: Rc<Cell<bool>>,
}

impl DesktopLifecycle {
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            retirements: Rc::new(RefCell::new(Vec::new())),
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
        Some(cx.background_executor().spawn(async move {
            let mut detach_failed = false;
            for retirement in retirements {
                if !retirement.await {
                    detach_failed = true;
                }
            }
            if detach_failed {
                Err("desktop market worker did not retire before its deadline".to_string())
            } else {
                Ok(())
            }
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
