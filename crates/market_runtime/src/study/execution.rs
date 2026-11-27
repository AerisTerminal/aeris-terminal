//! Bounded native calculation isolation. One worker per study runtime, one job
//! in flight. A deadline opens the circuit until the abandoned calculation
//! finishes: no replacement worker or queued backlog is created, and a late
//! result cannot commit state or publications.
use aeris_market_data::{OrderBook, TopOfBookQuote};
use std::collections::VecDeque;

use super::{
    AssertUnwindSafe, NativeStudyCalculate, NativeStudyCalculation, NativeStudyState,
    ResolvedStudyInput, StudyAlertEvent, StudyChartContext, StudyDepthView, StudyDirtyRange,
    StudyExecutionContext, StudyExecutionMeta, StudyLiveMarketData, StudyOutputBuffer,
    StudyOutputTimeline, StudyQuoteView, StudySceneState, StudySettings, StudyStyleOverlay,
    StudyTradeWindow, bounded_execution_detail, catch_unwind,
};
use crate::RetainedMarketTrade;
use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const CALCULATION_DEADLINE: Duration = Duration::from_millis(50);
pub(super) const BUSY_DETAIL: &str =
    "study worker is still finishing a calculation that missed its deadline";
pub(super) struct ExecutionOutput {
    pub(super) outputs: Vec<StudyOutputBuffer>,
    pub(super) style_overlay: StudyStyleOverlay,
    pub(super) scene: StudySceneState,
    pub(super) alerts: Vec<StudyAlertEvent>,
    pub(super) state: Option<NativeStudyState>,
    pub(super) intrabar_state: Option<NativeStudyState>,
}
type Output = Result<ExecutionOutput, String>;

struct LiveInput {
    quote: Option<(TopOfBookQuote, u8, u8)>,
    trades: Option<(VecDeque<RetainedMarketTrade>, u64, u64, u8, u8)>,
    depth: Option<(OrderBook, u8, u8)>,
}

impl LiveInput {
    fn snapshot(value: StudyLiveMarketData<'_>) -> Self {
        Self {
            quote: value
                .quote
                .map(|q| (q.quote.clone(), q.price_scale, q.quantity_scale)),
            trades: value.trades.map(|t| {
                (
                    t.trades.clone(),
                    t.session_generation,
                    t.source_watermark,
                    t.price_scale,
                    t.quantity_scale,
                )
            }),
            depth: value
                .depth
                .map(|d| (d.book.clone(), d.price_scale, d.quantity_scale)),
        }
    }
    fn view(&self) -> StudyLiveMarketData<'_> {
        StudyLiveMarketData {
            quote: self
                .quote
                .as_ref()
                .map(|(q, p, s)| StudyQuoteView::new(q, *p, *s)),
            trades: self
                .trades
                .as_ref()
                .map(|(t, g, w, p, s)| StudyTradeWindow::new(t, *g, *w, *p, *s)),
            depth: self
                .depth
                .as_ref()
                .map(|(d, p, s)| StudyDepthView::new(d, *p, *s)),
        }
    }
}

struct Job {
    settings: StudySettings,
    inputs: Vec<ResolvedStudyInput>,
    live: Vec<Option<LiveInput>>,
    timeline: StudyOutputTimeline,
    dirty: StudyDirtyRange,
    meta: StudyExecutionMeta,
    chart_context: StudyChartContext,
    calculate: NativeStudyCalculate,
    outputs: Vec<StudyOutputBuffer>,
    style_overlay: StudyStyleOverlay,
    scene: StudySceneState,
    state: Option<NativeStudyState>,
    intrabar_state: Option<NativeStudyState>,
    reply: mpsc::SyncSender<Output>,
}

pub(super) struct Executor {
    jobs: mpsc::SyncSender<Job>,
    submitted: Cell<u64>,
    finished: Arc<AtomicU64>,
    turn_remaining: Cell<Option<Duration>>,
    #[cfg(test)]
    wait_for_test_calculation: Cell<bool>,
}

impl Executor {
    pub(super) fn start() -> Result<Self, String> {
        let (jobs, receiver) = mpsc::sync_channel::<Job>(1);
        let finished = Arc::new(AtomicU64::new(0));
        let worker_finished = Arc::clone(&finished);
        thread::Builder::new()
            .name("aeris-study-calculation".into())
            .spawn(move || {
                while let Ok(mut job) = receiver.recv() {
                    let live = job
                        .live
                        .iter()
                        .map(|value| value.as_ref().map(LiveInput::view))
                        .collect::<Vec<_>>();
                    let mut alerts = Vec::new();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let mut context = StudyExecutionContext {
                            settings: &job.settings,
                            inputs: &job.inputs,
                            live_inputs: &live,
                            timeline: &job.timeline,
                            dirty: job.dirty,
                            meta: job.meta,
                            chart_context: job.chart_context,
                            outputs: &mut job.outputs,
                            style_overlay: &mut job.style_overlay,
                            scene: &mut job.scene,
                            alerts: &mut alerts,
                            state: job.state.as_mut(),
                            intrabar_state: job.intrabar_state.as_mut(),
                        };
                        (job.calculate)(&mut context)
                    }));
                    let result = match result {
                        Ok(Ok(())) => Ok(ExecutionOutput {
                            outputs: job.outputs,
                            style_overlay: job.style_overlay,
                            scene: job.scene,
                            alerts,
                            state: job.state,
                            intrabar_state: job.intrabar_state,
                        }),
                        Ok(Err(detail)) => Err(bounded_execution_detail(detail)),
                        Err(_) => Err("native study panicked".into()),
                    };
                    worker_finished.fetch_add(1, Ordering::Release);
                    // Completion must be visible before the reply wakes the caller: the next
                    // calculation can otherwise reject a finished worker as still busy.
                    let _ = job.reply.try_send(result);
                }
            })
            .map_err(|error| format!("study worker start failed: {error}"))?;
        Ok(Self {
            jobs,
            submitted: Cell::new(0),
            finished,
            turn_remaining: Cell::new(None),
            #[cfg(test)]
            wait_for_test_calculation: Cell::new(true),
        })
    }

    pub(super) fn begin_turn(&self) {
        self.turn_remaining.set(Some(CALCULATION_DEADLINE));
    }

    #[cfg(test)]
    pub(super) fn enforce_deadline_in_test(&self) {
        self.wait_for_test_calculation.set(false);
    }

    /// Whether the worker is still running a calculation abandoned at its deadline.
    pub(super) fn is_busy(&self) -> bool {
        self.finished.load(Ordering::Acquire) != self.submitted.get()
    }

    pub(super) fn calculate(
        &self,
        calculation: &NativeStudyCalculation<'_>,
        outputs: Vec<StudyOutputBuffer>,
        style_overlay: StudyStyleOverlay,
        scene: StudySceneState,
        state: Option<NativeStudyState>,
        intrabar_state: Option<NativeStudyState>,
    ) -> Output {
        if self.is_busy() {
            return Err(BUSY_DETAIL.into());
        }
        let remaining = self.turn_remaining.get().unwrap_or(CALCULATION_DEADLINE);
        if remaining.is_zero() {
            return Err("study calculation deferred: coordinator turn budget exhausted".into());
        }
        let started = Instant::now();
        let (reply, response) = mpsc::sync_channel(1);
        let job = Job {
            settings: calculation.settings.clone(),
            inputs: calculation.inputs.to_vec(),
            live: calculation
                .live_inputs
                .iter()
                .map(|value| value.map(LiveInput::snapshot))
                .collect(),
            timeline: calculation.timeline.clone(),
            dirty: calculation.dirty,
            meta: calculation.meta,
            chart_context: calculation.chart_context,
            calculate: calculation.program.calculate,
            outputs,
            style_overlay,
            scene,
            state,
            intrabar_state,
            reply,
        };
        self.jobs
            .try_send(job)
            .map_err(|_| "study worker unavailable".to_string())?;
        self.submitted.set(self.submitted.get().wrapping_add(1));
        #[cfg(test)]
        let result = if self.wait_for_test_calculation.get() {
            response.recv().ok()
        } else {
            response.recv_timeout(remaining).ok()
        };
        #[cfg(not(test))]
        let result = response.recv_timeout(remaining).ok();
        if self.turn_remaining.get().is_some() {
            #[cfg(test)]
            if self.wait_for_test_calculation.get() {
                self.turn_remaining.set(None);
            } else {
                self.turn_remaining
                    .set(Some(remaining.saturating_sub(started.elapsed())));
            }
            #[cfg(not(test))]
            self.turn_remaining
                .set(Some(remaining.saturating_sub(started.elapsed())));
        }
        result.unwrap_or_else(|| {
            Err("study calculation deadline exceeded; late result discarded".into())
        })
    }
}
