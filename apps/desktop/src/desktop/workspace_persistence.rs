//! Coalesced asynchronous workspace layout persistence.

use super::*;
use std::{sync::atomic::AtomicU64, time::Instant};

#[derive(Clone)]
struct WorkspaceLayoutRequest {
    layout_generation: u64,
    active_workspace_id: u64,
    workspace_tabs: Vec<WorkspaceTabState>,
    chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    watchlist_entries: Vec<WorkspaceWatchlistEntryState>,
    drawing_templates: Vec<WorkspaceDrawingTemplateState>,
    saved_layouts: Vec<Arc<WorkspaceSavedLayoutState>>,
    chart_chrome: aeris_contracts::WorkspaceChromeState,
}

/// One save's document content; the persistence owner assigns the layout generation.
pub(super) struct WorkspaceSaveContent {
    pub(super) active_workspace_id: u64,
    pub(super) workspace_tabs: Vec<WorkspaceTabState>,
    pub(super) chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    pub(super) watchlist_entries: Vec<WorkspaceWatchlistEntryState>,
    pub(super) drawing_templates: Vec<WorkspaceDrawingTemplateState>,
    pub(super) saved_layouts: Vec<Arc<WorkspaceSavedLayoutState>>,
    pub(super) chart_chrome: aeris_contracts::WorkspaceChromeState,
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

/// The receipt slot the writer publishes and shutdown waiters block on: the condvar wakes a
/// waiter the moment a receipt or a newer request lands, instead of polling on a sleep.
type DurabilitySlot = (Mutex<Option<WorkspaceLayoutDurability>>, std::sync::Condvar);

fn publish_durability(slot: &DurabilitySlot, receipt: Option<WorkspaceLayoutDurability>) {
    let (latest, published) = slot;
    *latest
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = receipt;
    published.notify_all();
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
        durability: Arc<DurabilitySlot>,
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
        let (latest, published) = &*durability;
        let mut receipt = latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = Instant::now() + timeout;
        loop {
            if latest_requested_generation.load(std::sync::atomic::Ordering::Acquire)
                != expected_generation
            {
                return Err(
                    "workspace layout changed after shutdown persistence was claimed".to_string(),
                );
            }
            if let Some(completion) = receipt.as_ref().cloned() {
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
            let now = Instant::now();
            if now >= deadline {
                return Err("workspace layout save timed out during shutdown".to_string());
            }
            let (guard, _timed_out) = published
                .wait_timeout(receipt, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            receipt = guard;
        }
    }
}

#[derive(Clone)]
pub(super) struct WorkspaceLayoutPersistence {
    latest: Arc<Mutex<Option<WorkspaceLayoutRequest>>>,
    wake: SyncSender<()>,
    result: Arc<Mutex<Option<WorkspaceLayoutCompletion>>>,
    durability: Arc<DurabilitySlot>,
    latest_requested_generation: Arc<AtomicU64>,
    workspace_revision: Rc<Cell<u64>>,
    layout_generation: Rc<Cell<u64>>,
    pending: Rc<Cell<bool>>,
    error: Rc<RefCell<Option<String>>>,
}

impl WorkspaceLayoutPersistence {
    pub(super) fn new(
        workspace_revision: u64,
        layout_generation: u64,
        persistence: &aeris_desktop::chart_chrome::DesktopPersistence,
        completion_wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let latest = Arc::new(Mutex::new(None));
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let result = Arc::new(Mutex::new(None));
        let durability: Arc<DurabilitySlot> =
            Arc::new((Mutex::new(None), std::sync::Condvar::new()));
        let latest_requested_generation = Arc::new(AtomicU64::new(layout_generation));
        let worker_latest = Arc::clone(&latest);
        let worker_result = Arc::clone(&result);
        let worker_durability = Arc::clone(&durability);
        let worker_persistence = persistence.clone();
        std::thread::Builder::new()
            .name("aeris-workspace-layout-client".to_string())
            .spawn(move || {
                run_workspace_layout_persistence(
                    &worker_latest,
                    &wake_rx,
                    &worker_result,
                    &worker_durability,
                    &worker_persistence,
                    completion_wake.as_ref(),
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

    pub(super) fn request_with_chart_settings(
        &self,
        content: WorkspaceSaveContent,
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
            active_workspace_id: content.active_workspace_id,
            workspace_tabs: content.workspace_tabs,
            chart_settings_templates: content.chart_settings_templates,
            watchlist_entries: content.watchlist_entries,
            drawing_templates: content.drawing_templates,
            saved_layouts: content.saved_layouts,
            chart_chrome: content.chart_chrome,
        });
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => {
                self.layout_generation.set(generation);
                self.latest_requested_generation
                    .store(generation, std::sync::atomic::Ordering::Release);
                // A pending shutdown wait observes the newer generation at once instead of
                // waiting out its deadline.
                self.durability.1.notify_all();
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

    #[cfg(test)]
    fn request(
        &self,
        active_workspace_id: u64,
        workspace_tabs: Vec<WorkspaceTabState>,
    ) -> Result<(), String> {
        self.request_with_chart_settings(WorkspaceSaveContent {
            active_workspace_id,
            workspace_tabs,
            chart_settings_templates: Vec::new(),
            watchlist_entries: Vec::new(),
            drawing_templates: Vec::new(),
            saved_layouts: Vec::new(),
            chart_chrome: aeris_contracts::WorkspaceChromeState::default(),
        })
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
    durability: &DurabilitySlot,
    persistence: &aeris_desktop::chart_chrome::DesktopPersistence,
    completion_wake: &(dyn Fn() + Send + Sync),
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
        let path = persistence.workspace_path();
        let mut current = local_state::load_workspace(&path);
        current.workspace_revision = current.workspace_revision.saturating_add(1);
        current.layout_generation = request
            .layout_generation
            .max(current.layout_generation.saturating_add(1));
        current.active_workspace_id = request.active_workspace_id;
        current.workspace_tabs = request.workspace_tabs;
        current.chart_settings_templates = request.chart_settings_templates;
        current.default_chart_settings = None;
        current.watchlist = request
            .watchlist_entries
            .iter()
            .filter_map(|entry| {
                entry
                    .instrument
                    .as_ref()
                    .map(|instrument| instrument.display_symbol.clone())
            })
            .collect();
        current.watchlist_entries = request.watchlist_entries;
        current.drawing_templates = request.drawing_templates;
        current.saved_layouts = request
            .saved_layouts
            .iter()
            .map(|layout| WorkspaceSavedLayoutState::clone(layout))
            .collect();
        current.chart_chrome = Some(request.chart_chrome);
        let completed = local_state::save_workspace(&current, &path).map(|()| current);
        publish_durability(
            durability,
            Some(WorkspaceLayoutDurability {
                request_generation,
                result: completed.as_ref().map(|_| ()).map_err(Clone::clone),
            }),
        );
        *result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WorkspaceLayoutCompletion {
            request_generation,
            result: completed,
        });
        completion_wake();
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
                durability: Arc::new((Mutex::new(None), std::sync::Condvar::new())),
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
    fn coalesced_request_keeps_chart_settings_templates_with_the_layout() {
        let (state, _receiver) = persistence();
        let template = WorkspaceChartSettingsTemplateState {
            name: "Scalping".to_string(),
            appearance: None,
            crosshair_mode: 1,
            time_zone: "Etc/UTC".to_string(),
            show_order_management_lines: Some(true),
            show_execution_marks: Some(false),
            extend_order_lines_left: Some(false),
            legend: None,
        };

        let watchlist_entry = WorkspaceWatchlistEntryState {
            instrument: local_state::fixture_workspace().watchlist_entries[0]
                .instrument
                .clone(),
            flag_color: "#335CFF".to_string(),
        };
        let drawing_template = WorkspaceDrawingTemplateState {
            kind: "trend_line".to_string(),
            name: "Orange".to_string(),
            template_json: r#"{"name":"Orange","kind":"trend_line","options":{}}"#.to_string(),
        };
        state
            .request_with_chart_settings(WorkspaceSaveContent {
                active_workspace_id: 7,
                workspace_tabs: vec![],
                chart_settings_templates: vec![template.clone()],
                watchlist_entries: vec![watchlist_entry.clone()],
                drawing_templates: vec![drawing_template.clone()],
                saved_layouts: local_state::default_workspace()
                    .saved_layouts
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
                chart_chrome: aeris_contracts::WorkspaceChromeState::default(),
            })
            .expect("request");

        let latest = state.latest.lock().expect("latest");
        let request = latest.as_ref().expect("request remains coalesced");
        assert_eq!(request.chart_settings_templates, vec![template]);
        assert_eq!(request.watchlist_entries, vec![watchlist_entry]);
        assert_eq!(request.drawing_templates, vec![drawing_template]);
        assert_eq!(request.saved_layouts.len(), 1);
    }

    #[test]
    fn shutdown_wait_observes_latest_active_workspace_durability_without_stealing_ui_result() {
        let (state, _receiver) = persistence();
        state.request(42, vec![]).expect("request");
        *state.durability.0.lock().expect("durability") = Some(WorkspaceLayoutDurability {
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
        *state.durability.0.lock().expect("durability") = Some(WorkspaceLayoutDurability {
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
        *state.durability.0.lock().expect("durability") = Some(WorkspaceLayoutDurability {
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
    fn a_request_while_the_writer_wake_is_full_is_coalesced_not_refused() {
        let (state, receiver) = persistence();
        state.request(1, vec![]).expect("first request");
        state
            .request(2, vec![])
            .expect("a full wake slot coalesces the newer layout");
        assert!(receiver.try_recv().is_ok());
        assert!(
            receiver.try_recv().is_err(),
            "one wake hands the writer the newest layout"
        );
        assert_eq!(
            state
                .latest
                .lock()
                .expect("latest")
                .as_ref()
                .map(|request| request.active_workspace_id),
            Some(2)
        );
        assert!(state.pending.get());
        assert_eq!(state.error(), None);
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

    fn temporary_workspace_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aeris-workspace-persistence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ))
    }

    #[test]
    fn chrome_changes_save_through_the_one_workspace_writer() {
        let root = temporary_workspace_root("chrome-writer");
        let owner = aeris_desktop::chart_chrome::DesktopPersistence::new(root.clone());
        let writer = WorkspaceLayoutPersistence::new(1, 1, &owner, Arc::new(|| {}))
            .expect("workspace writer starts");
        let chrome = aeris_desktop::chart_chrome::ChartChromePreferences {
            chart_type: aeris_chart_integration::ChartType::Line,
            ..aeris_desktop::chart_chrome::ChartChromePreferences::default()
        }
        .to_workspace_chrome();

        writer
            .request_with_chart_settings(WorkspaceSaveContent {
                active_workspace_id: 1,
                workspace_tabs: local_state::default_workspace().workspace_tabs,
                chart_settings_templates: Vec::new(),
                watchlist_entries: Vec::new(),
                drawing_templates: Vec::new(),
                saved_layouts: Vec::new(),
                chart_chrome: chrome.clone(),
            })
            .expect("a chrome change requests a workspace save");
        let generation = writer
            .shutdown_wait()
            .wait(Duration::from_secs(10))
            .expect("the one writer drains the save before shutdown");
        assert!(writer.shutdown_generation_is_current(generation));

        let saved = local_state::load_workspace(&owner.workspace_path());
        assert_eq!(
            saved.chart_chrome,
            Some(chrome),
            "chrome preferences persist inside the workspace document"
        );
        std::fs::remove_dir_all(&root).expect("fixture root removed");
    }

    #[test]
    fn a_newer_request_wakes_a_pending_shutdown_wait_through_the_condvar() {
        let (state, _receiver) = persistence();
        state.request(7, vec![]).expect("first request");
        let wait = state.shutdown_wait();
        let latest_requested = Arc::clone(&state.latest_requested_generation);
        let durability = Arc::clone(&state.durability);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            latest_requested.store(3, std::sync::atomic::Ordering::Release);
            let (_, published) = &*durability;
            published.notify_all();
        });

        let started = Instant::now();
        assert_eq!(
            wait.wait(Duration::from_secs(30)),
            Err("workspace layout changed after shutdown persistence was claimed".to_string())
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the condvar wakes the wait instead of letting it time out"
        );
        waker.join().expect("waker completes");
    }
}
