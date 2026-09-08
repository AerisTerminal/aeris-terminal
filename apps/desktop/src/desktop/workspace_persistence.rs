//! Coalesced asynchronous workspace layout persistence.

use super::*;
use std::time::Instant;

#[derive(Clone)]
struct WorkspaceLayoutRequest {
    layout_generation: u64,
    active_workspace_id: u64,
    workspace_tabs: Vec<WorkspaceTabState>,
}

struct WorkspaceLayoutCompletion {
    request_generation: u64,
    result: Result<WorkspaceState, String>,
}

#[derive(Clone)]
pub(super) struct WorkspaceLayoutPersistence {
    latest: Arc<Mutex<Option<WorkspaceLayoutRequest>>>,
    wake: SyncSender<()>,
    result: Arc<Mutex<Option<WorkspaceLayoutCompletion>>>,
    workspace_revision: Rc<Cell<u64>>,
    layout_generation: Rc<Cell<u64>>,
    pending: Rc<Cell<bool>>,
    error: Rc<RefCell<Option<String>>>,
}

impl WorkspaceLayoutPersistence {
    pub(super) fn new(workspace_revision: u64, layout_generation: u64) -> Result<Self, String> {
        let latest = Arc::new(Mutex::new(None));
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let result = Arc::new(Mutex::new(None));
        let worker_latest = Arc::clone(&latest);
        let worker_result = Arc::clone(&result);
        std::thread::Builder::new()
            .name("axiusflow-workspace-layout-client".to_string())
            .spawn(move || {
                run_workspace_layout_persistence(&worker_latest, &wake_rx, &worker_result);
            })
            .map_err(|_| "workspace layout client could not start".to_string())?;
        Ok(Self {
            latest,
            wake: wake_tx,
            result,
            workspace_revision: Rc::new(Cell::new(workspace_revision)),
            layout_generation: Rc::new(Cell::new(layout_generation)),
            pending: Rc::new(Cell::new(false)),
            error: Rc::new(RefCell::new(None)),
        })
    }

    pub(super) fn request(
        &self,
        active_workspace_id: u64,
        workspace_tabs: Vec<WorkspaceTabState>,
    ) -> Result<(), String> {
        let generation = self
            .layout_generation
            .get()
            .checked_add(1)
            .ok_or_else(|| "workspace layout generation is exhausted".to_string())?;
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WorkspaceLayoutRequest {
            layout_generation: generation,
            active_workspace_id,
            workspace_tabs,
        });
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => {
                self.layout_generation.set(generation);
                self.pending.set(true);
                self.error.borrow_mut().take();
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(())) => {
                Err("workspace layout client is unavailable".to_string())
            }
        }
    }

    pub(super) fn poll(&self) -> bool {
        let Some(result) = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return false;
        };
        // A save may complete while a newer layout is waiting in the coalesced slot.
        // Its success or error must not retire the current request's presentation.
        if result.request_generation != self.layout_generation.get() {
            return false;
        }
        match result.result {
            Ok(workspace) => {
                self.workspace_revision.set(workspace.workspace_revision);
                self.layout_generation.set(workspace.layout_generation);
                self.error.borrow_mut().take();
            }
            Err(error) => {
                *self.error.borrow_mut() = Some(error);
            }
        }
        self.pending.set(false);
        true
    }

    pub(super) fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }

    /// Waits for the latest coalesced layout request to reach durable local storage.
    /// Used only during window shutdown so the active workspace cannot be lost
    /// merely because no market frame happened after the user's last selection.
    pub(super) fn flush(&self, timeout: Duration) -> Result<(), String> {
        if !self.pending.get() {
            return self.error().map_or(Ok(()), Err);
        }
        let expected_generation = self.layout_generation.get();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(completion) = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                && completion.request_generation == expected_generation
            {
                self.pending.set(false);
                return match completion.result {
                    Ok(workspace) => {
                        self.workspace_revision.set(workspace.workspace_revision);
                        self.layout_generation.set(workspace.layout_generation);
                        self.error.borrow_mut().take();
                        Ok(())
                    }
                    Err(error) => {
                        *self.error.borrow_mut() = Some(error.clone());
                        Err(error)
                    }
                };
            }
            if Instant::now() >= deadline {
                return Err("workspace layout save timed out during shutdown".to_string());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn run_workspace_layout_persistence(
    latest: &Mutex<Option<WorkspaceLayoutRequest>>,
    wake: &Receiver<()>,
    result: &Mutex<Option<WorkspaceLayoutCompletion>>,
) {
    while wake.recv().is_ok() {
        while wake.recv_timeout(Duration::from_millis(100)).is_ok() {}
        let Some(request) = latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            continue;
        };
        let request_generation = request.layout_generation;
        let mut current = local_state::load_workspace();
        current.workspace_revision = current.workspace_revision.saturating_add(1);
        current.layout_generation = request
            .layout_generation
            .max(current.layout_generation.saturating_add(1));
        current.active_workspace_id = request.active_workspace_id;
        current.workspace_tabs = request.workspace_tabs;
        let completed = local_state::save_workspace(&current).map(|()| current);
        *result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WorkspaceLayoutCompletion {
            request_generation,
            result: completed,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persistence() -> (WorkspaceLayoutPersistence, Receiver<()>) {
        let (wake, receiver) = mpsc::sync_channel(1);
        (
            WorkspaceLayoutPersistence {
                latest: Arc::new(Mutex::new(None)),
                wake,
                result: Arc::new(Mutex::new(None)),
                workspace_revision: Rc::new(Cell::new(1)),
                layout_generation: Rc::new(Cell::new(1)),
                pending: Rc::new(Cell::new(false)),
                error: Rc::new(RefCell::new(None)),
            },
            receiver,
        )
    }

    #[test]
    fn superseded_save_results_cannot_clear_pending_or_publish_an_error() {
        for result in [
            Ok(WorkspaceState {
                workspace_revision: 2,
                layout_generation: 2,
                ..WorkspaceState::default()
            }),
            Err("old save failed".to_string()),
        ] {
            let (state, _receiver) = persistence();
            state.request(1, vec![]).expect("first request");
            state.request(2, vec![]).expect("coalesced replacement");
            *state.result.lock().expect("result") = Some(WorkspaceLayoutCompletion {
                request_generation: 2,
                result,
            });
            assert!(!state.poll());
            assert!(state.pending.get());
            assert_eq!(state.layout_generation.get(), 3);
            assert_eq!(state.workspace_revision.get(), 1);
            assert_eq!(state.error(), None);
            assert_eq!(
                state
                    .latest
                    .lock()
                    .expect("latest")
                    .as_ref()
                    .expect("request")
                    .active_workspace_id,
                2
            );
        }
    }

    #[test]
    fn current_save_result_finishes_pending_and_preserves_concrete_error() {
        let (state, _receiver) = persistence();
        state.request(1, vec![]).expect("request");
        *state.result.lock().expect("result") = Some(WorkspaceLayoutCompletion {
            request_generation: 2,
            result: Err("layout storage is unavailable".to_string()),
        });
        assert!(state.poll());
        assert!(!state.pending.get());
        assert_eq!(
            state.error().as_deref(),
            Some("layout storage is unavailable")
        );
        state.request(1, vec![]).expect("retry");
        assert_eq!(state.error(), None);
    }

    #[test]
    fn flush_commits_the_latest_active_workspace_before_shutdown() {
        let (state, _receiver) = persistence();
        state.request(42, vec![]).expect("request");
        *state.result.lock().expect("result") = Some(WorkspaceLayoutCompletion {
            request_generation: 2,
            result: Ok(WorkspaceState {
                workspace_revision: 2,
                layout_generation: 2,
                active_workspace_id: 42,
                ..WorkspaceState::default()
            }),
        });

        state
            .flush(Duration::from_millis(20))
            .expect("latest layout flushes");
        assert!(!state.pending.get());
        assert_eq!(state.workspace_revision.get(), 2);
        assert_eq!(state.layout_generation.get(), 2);
        assert_eq!(state.error(), None);
    }

    #[test]
    fn flush_surfaces_current_persistence_failure() {
        let (state, _receiver) = persistence();
        state.request(7, vec![]).expect("request");
        *state.result.lock().expect("result") = Some(WorkspaceLayoutCompletion {
            request_generation: 2,
            result: Err("disk write failed".to_string()),
        });

        assert_eq!(
            state.flush(Duration::from_millis(20)),
            Err("disk write failed".to_string())
        );
        assert!(!state.pending.get());
        assert_eq!(state.error().as_deref(), Some("disk write failed"));
    }

    #[test]
    fn exhausted_generation_does_not_enqueue_or_overwrite_pending_layout() {
        let (state, receiver) = persistence();
        state.layout_generation.set(u64::MAX);
        assert!(state.request(1, vec![]).is_err());
        assert!(state.latest.lock().expect("latest").is_none());
        assert!(receiver.try_recv().is_err());
    }
}
