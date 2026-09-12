//! Coalesced asynchronous workspace layout persistence.

use super::*;
use std::{sync::atomic::AtomicU64, time::Instant};

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
struct WorkspaceLayoutDurability {
    request_generation: u64,
    result: Result<(), String>,
}

pub(super) struct WorkspaceLayoutShutdownWait {
    state: WorkspaceLayoutShutdownWaitState,
}

enum WorkspaceLayoutShutdownWaitState {
    Settled {
        expected_generation: u64,
        result: Result<(), String>,
    },
    Pending {
        expected_generation: u64,
        durability: Arc<Mutex<Option<WorkspaceLayoutDurability>>>,
        latest_requested_generation: Arc<AtomicU64>,
    },
}

impl WorkspaceLayoutShutdownWait {
    pub(super) fn wait(self, timeout: Duration) -> Result<u64, String> {
        let (expected_generation, durability, latest_requested_generation) = match self.state {
            WorkspaceLayoutShutdownWaitState::Settled {
                expected_generation,
                result,
            } => return result.map(|()| expected_generation),
            WorkspaceLayoutShutdownWaitState::Pending {
                expected_generation,
                durability,
                latest_requested_generation,
            } => (expected_generation, durability, latest_requested_generation),
        };
        let deadline = Instant::now() + timeout;
        loop {
            if latest_requested_generation.load(std::sync::atomic::Ordering::Acquire)
                != expected_generation
            {
                return Err(
                    "workspace layout changed after shutdown persistence was claimed".to_string(),
                );
            }
            if let Some(completion) = durability
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .cloned()
            {
                if completion.request_generation > expected_generation {
                    return Err(
                        "workspace layout changed after shutdown persistence was claimed"
                            .to_string(),
                    );
                }
                if completion.request_generation == expected_generation {
                    return completion.result.map(|()| expected_generation);
                }
            }
            if Instant::now() >= deadline {
                return Err("workspace layout save timed out during shutdown".to_string());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[derive(Clone)]
pub(super) struct WorkspaceLayoutPersistence {
    latest: Arc<Mutex<Option<WorkspaceLayoutRequest>>>,
    wake: SyncSender<()>,
    result: Arc<Mutex<Option<WorkspaceLayoutCompletion>>>,
    durability: Arc<Mutex<Option<WorkspaceLayoutDurability>>>,
    latest_requested_generation: Arc<AtomicU64>,
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
        let durability = Arc::new(Mutex::new(None));
        let latest_requested_generation = Arc::new(AtomicU64::new(layout_generation));
        let worker_latest = Arc::clone(&latest);
        let worker_result = Arc::clone(&result);
        let worker_durability = Arc::clone(&durability);
        std::thread::Builder::new()
            .name("axiusflow-workspace-layout-client".to_string())
            .spawn(move || {
                run_workspace_layout_persistence(
                    &worker_latest,
                    &wake_rx,
                    &worker_result,
                    &worker_durability,
                );
            })
            .map_err(|_| "workspace layout client could not start".to_string())?;
        Ok(Self {
            latest,
            wake: wake_tx,
            result,
            durability,
            latest_requested_generation,
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
        let Some(generation) = self.layout_generation.get().checked_add(1) else {
            let error = "workspace layout generation is exhausted".to_string();
            *self.error.borrow_mut() = Some(error.clone());
            return Err(error);
        };
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
                self.latest_requested_generation
                    .store(generation, std::sync::atomic::Ordering::Release);
                self.pending.set(true);
                self.error.borrow_mut().take();
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(())) => {
                let error = "workspace layout client is unavailable".to_string();
                self.latest
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                self.pending.set(false);
                *self.error.borrow_mut() = Some(error.clone());
                Err(error)
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

    /// Captures a sendable receipt for the latest coalesced layout request.
    /// The receipt is waited off the GPUI thread during shutdown.
    pub(super) fn shutdown_wait(&self) -> WorkspaceLayoutShutdownWait {
        if !self.pending.get() {
            return WorkspaceLayoutShutdownWait {
                state: WorkspaceLayoutShutdownWaitState::Settled {
                    expected_generation: self
                        .latest_requested_generation
                        .load(std::sync::atomic::Ordering::Acquire),
                    result: self.error().map_or(Ok(()), Err),
                },
            };
        }
        WorkspaceLayoutShutdownWait {
            state: WorkspaceLayoutShutdownWaitState::Pending {
                expected_generation: self
                    .latest_requested_generation
                    .load(std::sync::atomic::Ordering::Acquire),
                durability: Arc::clone(&self.durability),
                latest_requested_generation: Arc::clone(&self.latest_requested_generation),
            },
        }
    }

    pub(super) fn shutdown_generation_is_current(&self, generation: u64) -> bool {
        self.error.borrow().is_none()
            && self
                .latest_requested_generation
                .load(std::sync::atomic::Ordering::Acquire)
                == generation
    }
}

fn run_workspace_layout_persistence(
    latest: &Mutex<Option<WorkspaceLayoutRequest>>,
    wake: &Receiver<()>,
    result: &Mutex<Option<WorkspaceLayoutCompletion>>,
    durability: &Mutex<Option<WorkspaceLayoutDurability>>,
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
        *durability
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WorkspaceLayoutDurability {
            request_generation,
            result: completed.as_ref().map(|_| ()).map_err(Clone::clone),
        });
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
                durability: Arc::new(Mutex::new(None)),
                latest_requested_generation: Arc::new(AtomicU64::new(1)),
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
    fn shutdown_wait_observes_latest_active_workspace_durability_without_stealing_ui_result() {
        let (state, _receiver) = persistence();
        state.request(42, vec![]).expect("request");
        *state.durability.lock().expect("durability") = Some(WorkspaceLayoutDurability {
            request_generation: 2,
            result: Ok(()),
        });
        *state.result.lock().expect("result") = Some(WorkspaceLayoutCompletion {
            request_generation: 2,
            result: Ok(WorkspaceState {
                workspace_revision: 2,
                layout_generation: 2,
                active_workspace_id: 42,
                ..WorkspaceState::default()
            }),
        });

        assert_eq!(
            state
                .shutdown_wait()
                .wait(Duration::from_millis(20))
                .expect("latest layout reaches durability"),
            2
        );
        assert!(
            state.result.lock().expect("result").is_some(),
            "shutdown durability observation must not consume the UI completion"
        );
    }

    #[test]
    fn shutdown_wait_surfaces_current_persistence_failure() {
        let (state, _receiver) = persistence();
        state.request(7, vec![]).expect("request");
        *state.durability.lock().expect("durability") = Some(WorkspaceLayoutDurability {
            request_generation: 2,
            result: Err("disk write failed".to_string()),
        });

        assert_eq!(
            state.shutdown_wait().wait(Duration::from_millis(20)),
            Err("disk write failed".to_string())
        );
    }

    #[test]
    fn shutdown_wait_is_captured_without_waiting_on_the_ui_owner() {
        let (state, _receiver) = persistence();
        state.request(7, vec![]).expect("request");

        assert!(matches!(
            state.shutdown_wait().state,
            WorkspaceLayoutShutdownWaitState::Pending {
                expected_generation: 2,
                ..
            }
        ));
        assert!(state.pending.get());
    }

    #[test]
    fn shutdown_wait_rejects_a_newer_layout_request_before_commit() {
        let (state, _receiver) = persistence();
        state.request(7, vec![]).expect("first request");
        let wait = state.shutdown_wait();
        state.request(8, vec![]).expect("newer request");
        *state.durability.lock().expect("durability") = Some(WorkspaceLayoutDurability {
            request_generation: 2,
            result: Ok(()),
        });

        assert_eq!(
            wait.wait(Duration::from_millis(20)),
            Err("workspace layout changed after shutdown persistence was claimed".to_string())
        );
    }

    #[test]
    fn settled_shutdown_wait_returns_generation_for_post_wait_revalidation() {
        let (state, _receiver) = persistence();
        let generation = state
            .shutdown_wait()
            .wait(Duration::ZERO)
            .expect("settled persistence is immediately durable");
        assert_eq!(generation, 1);
        assert!(state.shutdown_generation_is_current(generation));

        state.request(8, vec![]).expect("newer request");
        assert!(!state.shutdown_generation_is_current(generation));
    }

    #[test]
    fn disconnected_persistence_worker_cannot_leave_a_false_pending_success() {
        let (state, receiver) = persistence();
        drop(receiver);

        assert!(state.request(7, vec![]).is_err());
        assert!(!state.pending.get());
        assert!(state.latest.lock().expect("latest").is_none());
        assert!(state.shutdown_wait().wait(Duration::ZERO).is_err());
    }

    #[test]
    fn exhausted_generation_does_not_enqueue_or_overwrite_pending_layout() {
        let (state, receiver) = persistence();
        state.layout_generation.set(u64::MAX);
        assert!(state.request(1, vec![]).is_err());
        assert!(state.latest.lock().expect("latest").is_none());
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            state.error().as_deref(),
            Some("workspace layout generation is exhausted")
        );
        assert!(!state.shutdown_generation_is_current(1));
    }
}
