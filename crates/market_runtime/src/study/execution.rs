//! Bounded native calculation isolation. One worker per study runtime, one job
//! in flight. A deadline opens the circuit: no replacement worker or queued
//! backlog is created, and a late result cannot commit state or publications.
use super::{
    AssertUnwindSafe, NativeStudyCalculate, NativeStudyCalculation, NativeStudyState, OrderBook,
    ResolvedStudyInput, StudyDepthView, StudyDirtyRange, StudyExecutionContext,
    StudyLiveMarketData, StudyOutputBuffer, StudyOutputTimeline, StudyQuoteView, StudySettings,
    StudyTradeSample, StudyTradeWindow, TopOfBookQuote, VecDeque, bounded_execution_detail,
    catch_unwind,
};
use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const CALCULATION_DEADLINE: Duration = Duration::from_millis(50);
type Output = Result<(Vec<StudyOutputBuffer>, Option<NativeStudyState>), String>;

struct LiveInput {
    quote: Option<(TopOfBookQuote, u8, u8)>,
    trades: Option<(VecDeque<StudyTradeSample>, u64, u64, u8, u8)>,
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
    calculate: NativeStudyCalculate,
    outputs: Vec<StudyOutputBuffer>,
    state: Option<NativeStudyState>,
    reply: mpsc::SyncSender<Output>,
}

pub(super) struct Executor {
    jobs: mpsc::SyncSender<Job>,
    failed: AtomicBool,
    turn_remaining: Cell<Option<Duration>>,
}

impl Executor {
    pub(super) fn start() -> Result<Self, String> {
        let (jobs, receiver) = mpsc::sync_channel::<Job>(1);
        thread::Builder::new()
            .name("aeris-study-calculation".into())
            .spawn(move || {
                while let Ok(mut job) = receiver.recv() {
                    let live = job
                        .live
                        .iter()
                        .map(|value| value.as_ref().map(LiveInput::view))
                        .collect::<Vec<_>>();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let mut context = StudyExecutionContext {
                            settings: &job.settings,
                            inputs: &job.inputs,
                            live_inputs: &live,
                            timeline: &job.timeline,
                            dirty: job.dirty,
                            outputs: &mut job.outputs,
                            state: job.state.as_mut(),
                        };
                        (job.calculate)(&mut context)
                    }));
                    let result = match result {
                        Ok(Ok(())) => Ok((job.outputs, job.state)),
                        Ok(Err(detail)) => Err(bounded_execution_detail(detail)),
                        Err(_) => Err("native study panicked".into()),
                    };
                    let _ = job.reply.try_send(result);
                }
            })
            .map_err(|error| format!("study worker start failed: {error}"))?;
        Ok(Self {
            jobs,
            failed: AtomicBool::new(false),
            turn_remaining: Cell::new(None),
        })
    }

    pub(super) fn begin_turn(&self) {
        self.turn_remaining.set(Some(CALCULATION_DEADLINE));
    }

    pub(super) fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub(super) fn calculate(
        &self,
        calculation: &NativeStudyCalculation<'_>,
        outputs: Vec<StudyOutputBuffer>,
        state: Option<NativeStudyState>,
    ) -> Output {
        if self.failed.load(Ordering::Acquire) {
            return Err("study execution disabled after worker deadline; restart required".into());
        }
        let remaining = self.turn_remaining.get().unwrap_or(CALCULATION_DEADLINE);
        if remaining.is_zero() {
            self.failed.store(true, Ordering::Release);
            return Err("study execution disabled after exhausting coordinator turn budget".into());
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
            calculate: calculation.program.calculate,
            outputs,
            state,
            reply,
        };
        self.jobs
            .try_send(job)
            .map_err(|_| "study worker unavailable".to_string())?;
        let result = response.recv_timeout(remaining);
        if self.turn_remaining.get().is_some() {
            self.turn_remaining
                .set(Some(remaining.saturating_sub(started.elapsed())));
        }
        if let Ok(result) = result {
            result
        } else {
            self.failed.store(true, Ordering::Release);
            Err("study calculation deadline exceeded; late result discarded".into())
        }
    }
}
