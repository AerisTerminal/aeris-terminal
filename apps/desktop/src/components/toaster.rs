//! Toasts stacked in the top-right corner of a chart pane, clear of its price axis, built the way
//! Sonner builds them (<https://emilkowal.ski/ui/building-a-toast-component>):
//!
//! - Every moving value is a retargetable transition rather than a fixed animation, so a toast
//!   that is still entering when the stack changes glides from wherever it is to its new place.
//! - Collapsed, older toasts tuck behind the newest: each sits a little lower and narrower, takes
//!   the front toast's height and hides its content. Hovering expands the stack into a list laid
//!   out from each toast's measured height.
//! - A toast's timer pauses while its stack is hovered or swiped and while the window is
//!   inactive, so nothing disappears while the trader is reading it or away.
//! - Swiping right dismisses a toast past a distance or with enough momentum; a swipe the wrong
//!   way meets friction, a short one springs back, and a click dismisses.
//!
//! Chart status toasts stay while their condition holds. Fill toasts announce new executions;
//! the trading owner stays authoritative for fills, and this module only remembers which fills
//! it has already announced.

use super::*;
use gpui::{DispatchPhase, EntityId, MouseMoveEvent, MouseUpEvent};
use std::collections::{BTreeMap, BTreeSet};

const TOAST_WIDTH: f32 = 320.0;
/// Space between the stack and the pane's top edge and price axis.
const STACK_INSET: f32 = 12.0;
/// Space between toasts in the expanded list.
const EXPANDED_GAP: f32 = 8.0;
/// How far each older toast peeks out below the one in front of it while collapsed.
const COLLAPSED_PEEK: f32 = 10.0;
/// How much narrower each older toast is than the one in front of it while collapsed.
const COLLAPSED_SCALE_STEP: f32 = 0.05;
/// Collapsed, the front toast and the two tucked directly behind it stay visible.
const COLLAPSED_VISIBLE: usize = 3;
/// Live toasts a stack holds; a newer one retires the oldest timed toast past this.
const MAXIMUM_TOASTS: usize = 5;
/// Height assumed for a toast until its first layout measures it.
const ESTIMATED_HEIGHT: f32 = 58.0;
const MOVE_DURATION: Duration = Duration::from_millis(400);
const EXIT_DURATION: Duration = Duration::from_millis(250);
const SPRING_BACK_DURATION: Duration = Duration::from_millis(200);
/// How far an expired toast that was tucked behind the front one drifts down, as a share of its
/// height, while it fades.
const TUCKED_EXIT_DRIFT: f32 = 0.4;
const FILL_LIFETIME: Duration = Duration::from_secs(6);
/// A swipe this far right dismisses however slowly it moved.
const SWIPE_DISTANCE: f32 = 45.0;
/// A swipe faster than this, in logical pixels per millisecond, dismisses however short it was.
const SWIPE_VELOCITY: f32 = 0.11;
/// A press that travels less than this is a click.
const CLICK_SLOP: f32 = 3.0;
const ICON_SIZE: f32 = 16.0;
/// A fill first reported this long after it executed is history the owner caught up on after a
/// reconnect or restart, not news.
const FILL_FRESHNESS_NANOS: i64 = 60_000_000_000;

/// Targets closer than this, in logical pixels or opacity, are the same place.
const TWEEN_PRECISION: f32 = 0.001;

fn ease_out_cubic(progress: f32) -> f32 {
    1.0 - (1.0 - progress).powi(3)
}

fn depth_f32(depth: usize) -> f32 {
    f32::from(u16::try_from(depth).unwrap_or(u16::MAX))
}

/// One animated value. Retargeting starts from wherever the value is now, so an interrupted
/// transition never jumps.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Tween {
    fn settled(value: f32, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            start: now,
            duration: Duration::ZERO,
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32())
            .min(1.0)
    }

    fn value(&self, now: Instant) -> f32 {
        self.from + (self.to - self.from) * ease_out_cubic(self.progress(now))
    }

    fn is_moving(&self, now: Instant) -> bool {
        (self.from - self.to).abs() > TWEEN_PRECISION && self.progress(now) < 1.0
    }

    fn retarget(&mut self, to: f32, now: Instant, duration: Duration) {
        if (self.to - to).abs() <= TWEEN_PRECISION {
            return;
        }
        *self = Self {
            from: self.value(now),
            to,
            start: now,
            duration,
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ToastMotion {
    x: Tween,
    y: Tween,
    height: Tween,
    scale: Tween,
    opacity: Tween,
    content_opacity: Tween,
}

impl ToastMotion {
    fn tweens(&self) -> [&Tween; 6] {
        [
            &self.x,
            &self.y,
            &self.height,
            &self.scale,
            &self.opacity,
            &self.content_opacity,
        ]
    }
}

/// Where a toast is drawn at one instant, relative to its stack's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ToastFrame {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    opacity: f32,
    content_opacity: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToastTone {
    Buy,
    Sell,
    Warning,
    Loss,
    Muted,
}

impl From<ChartNoticeTone> for ToastTone {
    fn from(tone: ChartNoticeTone) -> Self {
        match tone {
            ChartNoticeTone::Muted => Self::Muted,
            ChartNoticeTone::Warning => Self::Warning,
            ChartNoticeTone::Loss => Self::Loss,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToastKind {
    Fill,
    /// A chart condition, by its label. It has no timer: it leaves when the condition clears.
    ChartStatus(&'static str),
}

/// The pane a stack of toasts sits on, by its workspace surface.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ToastStack(EntityId);

impl ToastStack {
    pub(super) fn of<T: 'static>(surface: &Entity<T>) -> Self {
        Self(surface.entity_id())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToastTimer {
    remaining: Duration,
    running_since: Option<Instant>,
}

impl ToastTimer {
    fn pause(&mut self, now: Instant) {
        if let Some(since) = self.running_since.take() {
            self.remaining = self
                .remaining
                .saturating_sub(now.saturating_duration_since(since));
        }
    }

    fn resume(&mut self, now: Instant) {
        self.running_since.get_or_insert(now);
    }

    fn deadline(&self) -> Option<Instant> {
        self.running_since.map(|since| since + self.remaining)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExitPath {
    /// Expired or clicked: lifts away and fades.
    Lift,
    /// Swiped: carries on to the right and fades.
    Swipe,
}

/// What a new toast says and how long it stays; `None` keeps it until it is dismissed.
struct ToastContent {
    kind: ToastKind,
    tone: ToastTone,
    title: SharedString,
    description: Option<SharedString>,
    lifetime: Option<Duration>,
}

#[derive(Clone, Debug)]
struct Toast {
    id: u64,
    stack: ToastStack,
    kind: ToastKind,
    tone: ToastTone,
    title: SharedString,
    description: Option<SharedString>,
    timer: Option<ToastTimer>,
    measured_height: Option<f32>,
    /// When the toast started leaving; it is removed once its exit has run.
    exit: Option<Instant>,
    motion: ToastMotion,
}

impl Toast {
    fn is_live(&self) -> bool {
        self.exit.is_none()
    }

    fn height(&self) -> f32 {
        self.measured_height.unwrap_or(ESTIMATED_HEIGHT)
    }

    fn frame(&self, now: Instant) -> ToastFrame {
        let motion = &self.motion;
        ToastFrame {
            x: motion.x.value(now),
            y: motion.y.value(now),
            width: TOAST_WIDTH * motion.scale.value(now),
            height: motion.height.value(now),
            opacity: motion.opacity.value(now).clamp(0.0, 1.0),
            content_opacity: motion.content_opacity.value(now).clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Swipe {
    toast: u64,
    stack: ToastStack,
    origin_x: f32,
    started: Instant,
}

/// Which fills the owner has reported, so each new execution is announced once.
#[derive(Debug, Default)]
struct FillWatch {
    revision: Option<u64>,
    /// `None` until the first snapshot, whose fills are history.
    seen: Option<BTreeSet<aeris_trading::FillId>>,
}

impl FillWatch {
    /// The fills that are news in this snapshot, oldest first.
    fn fresh<'a>(
        &mut self,
        revision: u64,
        fills: &'a [aeris_trading::Fill],
        now_unix_nanos: i64,
    ) -> Vec<&'a aeris_trading::Fill> {
        if self.revision == Some(revision) {
            return Vec::new();
        }
        self.revision = Some(revision);
        let current = fills.iter().map(|fill| fill.id.clone()).collect();
        let Some(seen) = self.seen.replace(current) else {
            return Vec::new();
        };
        let mut fresh: Vec<_> = fills
            .iter()
            .filter(|fill| {
                !seen.contains(&fill.id)
                    && now_unix_nanos.saturating_sub(fill.execution_unix_nanos)
                        <= FILL_FRESHNESS_NANOS
            })
            .collect();
        fresh.sort_by_key(|fill| fill.execution_unix_nanos);
        fresh
    }
}

/// Every toast in the window, oldest first, and the pointer state of their stacks.
#[derive(Debug, Default)]
pub(super) struct Toaster {
    toasts: Vec<Toast>,
    next_id: u64,
    hovered: Option<ToastStack>,
    swipe: Option<Swipe>,
    /// Timers run only while the window is active; it counts as inactive until the first tick.
    window_active: bool,
    reduce_motion: bool,
    /// A chart condition the trader dismissed stays dismissed until it changes or clears.
    dismissed_status: BTreeMap<ToastStack, &'static str>,
    fills: FillWatch,
    /// The timer expiry a wake-up is already scheduled for.
    wake: Option<Instant>,
}

impl Toaster {
    fn motion_duration(&self, duration: Duration) -> Duration {
        if self.reduce_motion {
            Duration::ZERO
        } else {
            duration
        }
    }

    fn is_expanded(&self, stack: ToastStack) -> bool {
        self.hovered == Some(stack) || self.swipe.is_some_and(|swipe| swipe.stack == stack)
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.toasts.iter().position(|toast| toast.id == id)
    }

    /// Live toasts of a stack, newest (front) first.
    fn live_indices(&self, stack: ToastStack) -> Vec<usize> {
        (0..self.toasts.len())
            .rev()
            .filter(|&index| {
                let toast = &self.toasts[index];
                toast.stack == stack && toast.is_live()
            })
            .collect()
    }

    fn push(&mut self, stack: ToastStack, content: ToastContent, now: Instant) -> u64 {
        let ToastContent {
            kind,
            tone,
            title,
            description,
            lifetime,
        } = content;
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        // A new toast enters from one toast-height above its place, transparent.
        self.toasts.push(Toast {
            id,
            stack,
            kind,
            tone,
            title,
            description,
            timer: lifetime.map(|remaining| ToastTimer {
                remaining,
                running_since: None,
            }),
            measured_height: None,
            exit: None,
            motion: ToastMotion {
                x: Tween::settled(0.0, now),
                y: Tween::settled(-ESTIMATED_HEIGHT, now),
                height: Tween::settled(ESTIMATED_HEIGHT, now),
                scale: Tween::settled(1.0, now),
                opacity: Tween::settled(0.0, now),
                content_opacity: Tween::settled(1.0, now),
            },
        });
        self.retire_overflow(stack, now);
        self.relayout(now);
        id
    }

    /// Keeps a stack bounded: past the limit the oldest timed toasts leave, and toasts already
    /// leaving beyond twice the limit are dropped at once.
    fn retire_overflow(&mut self, stack: ToastStack, now: Instant) {
        let live = self.live_indices(stack);
        let excess = live.len().saturating_sub(MAXIMUM_TOASTS);
        let oldest_timed: Vec<usize> = live
            .into_iter()
            .rev()
            .filter(|&index| self.toasts[index].timer.is_some())
            .take(excess)
            .collect();
        for index in oldest_timed {
            self.begin_exit(index, ExitPath::Lift, now);
        }
        let held = self
            .toasts
            .iter()
            .filter(|toast| toast.stack == stack)
            .count();
        let mut surplus = held.saturating_sub(2 * MAXIMUM_TOASTS);
        self.toasts.retain(|toast| {
            if surplus > 0 && toast.stack == stack && !toast.is_live() {
                surplus -= 1;
                return false;
            }
            true
        });
    }

    /// Starts a toast's exit. Returns whether it was still live.
    fn begin_exit(&mut self, index: usize, path: ExitPath, now: Instant) -> bool {
        let Some(toast) = self.toasts.get(index) else {
            return false;
        };
        if !toast.is_live() {
            return false;
        }
        let stack = toast.stack;
        let tucked = !self.is_expanded(stack) && self.live_indices(stack).first() != Some(&index);
        let duration = self.motion_duration(EXIT_DURATION);
        let toast = &mut self.toasts[index];
        toast.exit = Some(now);
        let y = toast.motion.y.value(now);
        let height = toast.motion.height.to;
        match path {
            ExitPath::Swipe => {
                toast
                    .motion
                    .x
                    .retarget(TOAST_WIDTH + STACK_INSET, now, duration);
            }
            ExitPath::Lift if tucked => {
                toast
                    .motion
                    .y
                    .retarget(y + height * TUCKED_EXIT_DRIFT, now, duration);
            }
            ExitPath::Lift => toast.motion.y.retarget(y - height, now, duration),
        }
        toast.motion.opacity.retarget(0.0, now, duration);
        let id = toast.id;
        if self.swipe.is_some_and(|swipe| swipe.toast == id) {
            self.swipe = None;
        }
        true
    }

    /// Dismisses a toast at the trader's request. A dismissed chart condition stays away until
    /// it changes.
    fn dismiss(&mut self, index: usize, path: ExitPath, now: Instant) {
        let toast = &self.toasts[index];
        if let ToastKind::ChartStatus(label) = toast.kind {
            self.dismissed_status.insert(toast.stack, label);
        }
        self.begin_exit(index, path, now);
    }

    /// Retargets every live toast to its place in its stack.
    fn relayout(&mut self, now: Instant) {
        let duration = self.motion_duration(MOVE_DURATION);
        let stacks: BTreeSet<ToastStack> = self.toasts.iter().map(|toast| toast.stack).collect();
        for stack in stacks {
            let expanded = self.is_expanded(stack);
            let live = self.live_indices(stack);
            let front_height = live
                .first()
                .map_or(ESTIMATED_HEIGHT, |&index| self.toasts[index].height());
            let mut list_offset = 0.0;
            for (depth, index) in live.into_iter().enumerate() {
                let toast = &mut self.toasts[index];
                let own_height = toast.height();
                let depth = depth_f32(depth);
                let (y, height, scale, opacity, content_opacity) = if expanded {
                    (list_offset, own_height, 1.0, 1.0, 1.0)
                } else {
                    (
                        depth * COLLAPSED_PEEK,
                        front_height,
                        1.0 - depth * COLLAPSED_SCALE_STEP,
                        if depth < depth_f32(COLLAPSED_VISIBLE) {
                            1.0
                        } else {
                            0.0
                        },
                        if depth == 0.0 { 1.0 } else { 0.0 },
                    )
                };
                list_offset += own_height + EXPANDED_GAP;
                let motion = &mut toast.motion;
                motion.y.retarget(y, now, duration);
                motion.height.retarget(height, now, duration);
                motion.scale.retarget(scale, now, duration);
                motion.opacity.retarget(opacity, now, duration);
                motion
                    .content_opacity
                    .retarget(content_opacity, now, duration);
            }
        }
    }

    /// The height of a stack's hover area: the whole list when expanded, the front toast and
    /// the edges peeking below it when collapsed.
    fn extent(&self, stack: ToastStack) -> f32 {
        let heights: Vec<f32> = self
            .live_indices(stack)
            .into_iter()
            .map(|index| self.toasts[index].height())
            .collect();
        let Some(front) = heights.first() else {
            return 0.0;
        };
        if self.is_expanded(stack) {
            heights.iter().sum::<f32>() + EXPANDED_GAP * depth_f32(heights.len() - 1)
        } else {
            front + COLLAPSED_PEEK * depth_f32((heights.len() - 1).min(COLLAPSED_VISIBLE - 1))
        }
    }

    /// Records a toast's rendered height. Returns whether the layout changed.
    fn measure(&mut self, id: u64, height: f32, now: Instant) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let toast = &mut self.toasts[index];
        if toast
            .measured_height
            .is_some_and(|measured| (measured - height).abs() < 0.5)
        {
            return false;
        }
        if toast.measured_height.replace(height).is_none() {
            // The first measurement replaces the estimate it entered with.
            toast.motion.height = Tween::settled(height, now);
        }
        self.relayout(now);
        true
    }

    /// Follows the pointer onto and off a stack. Returns whether anything changed.
    fn set_hovered(&mut self, stack: ToastStack, hovered: bool, now: Instant) -> bool {
        let next = if hovered {
            Some(stack)
        } else if self.hovered == Some(stack) {
            None
        } else {
            return false;
        };
        if self.hovered == next {
            return false;
        }
        self.hovered = next;
        self.sync_timers(now);
        self.relayout(now);
        true
    }

    fn begin_swipe(&mut self, id: u64, x: f32, now: Instant) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let toast = &self.toasts[index];
        if !toast.is_live() {
            return false;
        }
        self.swipe = Some(Swipe {
            toast: id,
            stack: toast.stack,
            origin_x: x,
            started: now,
        });
        self.sync_timers(now);
        self.relayout(now);
        true
    }

    /// Follows the pointer during a swipe. Dragging left, the wrong way, meets friction.
    fn move_swipe(&mut self, x: f32, now: Instant) -> bool {
        let Some(swipe) = self.swipe else {
            return false;
        };
        let delta = x - swipe.origin_x;
        let offset = if delta >= 0.0 {
            delta
        } else {
            delta / (1.5 + delta.abs() / 20.0)
        };
        let Some(index) = self.index_of(swipe.toast) else {
            return false;
        };
        self.toasts[index].motion.x = Tween::settled(offset, now);
        true
    }

    /// Ends a swipe: a click or a swipe far or fast enough dismisses, anything else springs
    /// back. Returns whether a swipe was in progress.
    fn end_swipe(&mut self, x: f32, now: Instant) -> bool {
        let Some(swipe) = self.swipe.take() else {
            return false;
        };
        if let Some(index) = self.index_of(swipe.toast) {
            let delta = x - swipe.origin_x;
            let elapsed_ms = now.saturating_duration_since(swipe.started).as_secs_f32() * 1000.0;
            let velocity = if elapsed_ms > 0.0 {
                delta / elapsed_ms
            } else {
                0.0
            };
            if delta.abs() < CLICK_SLOP {
                self.dismiss(index, ExitPath::Lift, now);
            } else if delta >= SWIPE_DISTANCE || velocity > SWIPE_VELOCITY {
                self.dismiss(index, ExitPath::Swipe, now);
            } else {
                self.spring_back(index, now);
            }
        }
        self.sync_timers(now);
        self.relayout(now);
        true
    }

    fn spring_back(&mut self, index: usize, now: Instant) {
        let duration = self.motion_duration(SPRING_BACK_DURATION);
        self.toasts[index].motion.x.retarget(0.0, now, duration);
    }

    /// Shows a pane's current chart condition, kept in place while it holds and lifted away
    /// once it clears.
    pub(super) fn show_chart_status(
        &mut self,
        stack: ToastStack,
        notice: Option<ChartSurfaceNotice>,
        now: Instant,
    ) {
        // A repair in progress is announced by the legend's own spinner beside the symbol, and
        // a centred notice covers a chart with nothing to read.
        let notice = notice.filter(|notice| {
            notice.placement == ChartNoticePlacement::Corner
                && notice.label != ChartState::Loading.label()
        });
        let current = self.toasts.iter().position(|toast| {
            toast.stack == stack
                && toast.is_live()
                && matches!(toast.kind, ToastKind::ChartStatus(_))
        });
        let Some(notice) = notice else {
            self.dismissed_status.remove(&stack);
            if let Some(index) = current
                && self.begin_exit(index, ExitPath::Lift, now)
            {
                self.relayout(now);
            }
            return;
        };
        if self.dismissed_status.get(&stack) == Some(&notice.label) {
            return;
        }
        self.dismissed_status.remove(&stack);
        if let Some(index) = current {
            let toast = &mut self.toasts[index];
            if toast.kind == ToastKind::ChartStatus(notice.label) {
                // A changed detail, such as a retry message, updates the toast in place.
                toast.tone = notice.tone.into();
                toast.description = notice.detail.map(SharedString::from);
                return;
            }
            self.begin_exit(index, ExitPath::Lift, now);
        }
        self.push(
            stack,
            ToastContent {
                kind: ToastKind::ChartStatus(notice.label),
                tone: notice.tone.into(),
                title: SharedString::new_static(notice.label),
                description: notice.detail.map(SharedString::from),
                lifetime: None,
            },
            now,
        );
    }

    /// Announces the fills in a trading snapshot that are news, on the given stack. Returns
    /// whether any toast was added.
    fn announce_fills(
        &mut self,
        snapshot: &aeris_trading_runtime::TradingSnapshot,
        host: Option<ToastStack>,
        now_unix_nanos: i64,
        now: Instant,
    ) -> bool {
        let fresh = self
            .fills
            .fresh(snapshot.revision, &snapshot.fills, now_unix_nanos);
        let Some(host) = host else {
            return false;
        };
        for fill in &fresh {
            let (title, description) =
                fill_notice_text(fill, &snapshot.instruments, &snapshot.accounts);
            self.push_fill(host, fill.side, title, description, now);
        }
        !fresh.is_empty()
    }

    fn push_fill(
        &mut self,
        stack: ToastStack,
        side: aeris_trading::OrderSide,
        title: String,
        description: String,
        now: Instant,
    ) -> u64 {
        let tone = match side {
            aeris_trading::OrderSide::Buy => ToastTone::Buy,
            aeris_trading::OrderSide::Sell => ToastTone::Sell,
        };
        self.push(
            stack,
            ToastContent {
                kind: ToastKind::Fill,
                tone,
                title: title.into(),
                description: Some(description.into()),
                lifetime: Some(FILL_LIFETIME),
            },
            now,
        )
    }

    /// Drops the toasts of panes that no longer exist.
    fn retain_stacks(&mut self, live: impl Fn(ToastStack) -> bool) {
        self.toasts.retain(|toast| live(toast.stack));
        self.dismissed_status.retain(|stack, _| live(*stack));
        if self.hovered.is_some_and(|stack| !live(stack)) {
            self.hovered = None;
        }
        if self.swipe.is_some_and(|swipe| !live(swipe.stack)) {
            self.swipe = None;
        }
    }

    /// Advances timers and exits. Timers run only while their stack is collapsed and the
    /// window is active. Returns whether anything changed.
    fn tick(&mut self, now: Instant, window_active: bool, reduce_motion: bool) -> bool {
        self.reduce_motion = reduce_motion;
        self.window_active = window_active;
        let mut changed = false;
        if !window_active && let Some(swipe) = self.swipe.take() {
            // The release may land in another window; spring back rather than stay held.
            if let Some(index) = self.index_of(swipe.toast) {
                self.spring_back(index, now);
            }
            changed = true;
        }
        if self
            .hovered
            .is_some_and(|stack| self.toasts.iter().all(|toast| toast.stack != stack))
        {
            self.hovered = None;
            changed = true;
        }
        for index in self.sync_timers(now) {
            changed |= self.begin_exit(index, ExitPath::Lift, now);
        }
        let exit = self.motion_duration(EXIT_DURATION);
        let held = self.toasts.len();
        self.toasts.retain(|toast| {
            toast
                .exit
                .is_none_or(|started| now.saturating_duration_since(started) < exit)
        });
        changed |= self.toasts.len() != held;
        if changed {
            self.relayout(now);
        }
        changed
    }

    /// Pauses the timers of held stacks and runs the rest, from `now`. Returns the toasts whose
    /// time is up.
    fn sync_timers(&mut self, now: Instant) -> Vec<usize> {
        let mut expired = Vec::new();
        for index in 0..self.toasts.len() {
            let held = !self.window_active || self.is_expanded(self.toasts[index].stack);
            let toast = &mut self.toasts[index];
            if !toast.is_live() {
                continue;
            }
            let Some(timer) = toast.timer.as_mut() else {
                continue;
            };
            if held {
                timer.pause(now);
            } else {
                timer.resume(now);
            }
            if timer.deadline().is_some_and(|deadline| deadline <= now) {
                expired.push(index);
            }
        }
        expired
    }

    fn is_animating(&self, now: Instant) -> bool {
        self.toasts.iter().any(|toast| {
            !toast.is_live()
                || toast
                    .motion
                    .tweens()
                    .iter()
                    .any(|tween| tween.is_moving(now))
        })
    }

    /// The next timer expiry to wake up for, unless a wake-up for it or an earlier one is
    /// already scheduled.
    fn claim_wake(&mut self) -> Option<Instant> {
        let deadline = self
            .toasts
            .iter()
            .filter(|toast| toast.is_live())
            .filter_map(|toast| toast.timer?.deadline())
            .min()?;
        if self.wake.is_some_and(|wake| wake <= deadline) {
            return None;
        }
        self.wake = Some(deadline);
        Some(deadline)
    }

    fn wake_fired(&mut self, deadline: Instant) {
        if self.wake == Some(deadline) {
            self.wake = None;
        }
    }
}

/// "Bought 2 ESZ5" over "at 6,012.25 · Account", in the owner's exact fixed-point values.
fn fill_notice_text(
    fill: &aeris_trading::Fill,
    instruments: &[aeris_trading_runtime::TradingInstrument],
    accounts: &[aeris_trading::TradingAccount],
) -> (String, String) {
    let verb = match fill.side {
        aeris_trading::OrderSide::Buy => "Bought",
        aeris_trading::OrderSide::Sell => "Sold",
    };
    let symbol = instruments
        .iter()
        .find(|instrument| instrument.instrument_id == fill.instrument_id)
        .map_or(fill.instrument_id.as_str(), |instrument| {
            instrument.contract.provenance.display_symbol.as_str()
        });
    let account = accounts
        .iter()
        .find(|account| account.id == fill.account_id)
        .map_or(fill.account_id.as_str(), |account| {
            account.display_name.as_str()
        });
    let quantity = market_price_text(fill.quantity.units(), u32::from(fill.quantity.scale()));
    let price = market_price_text(fill.price.units(), u32::from(fill.price.scale()));
    (
        format!("{verb} {quantity} {symbol}"),
        format!("at {price} · {account}"),
    )
}

/// The view that owns the toaster its stacks render and update.
pub(super) trait ToastHost: 'static {
    fn toaster_mut(&mut self) -> &mut Toaster;
}

fn tone_icon(tone: ToastTone, theme: &AerisTheme) -> (HugeIcon, ThemeColor) {
    let colors = theme.colors;
    match tone {
        ToastTone::Buy => (HugeIcon::CheckIcon, colors.buy),
        ToastTone::Sell => (HugeIcon::CheckIcon, colors.sell),
        ToastTone::Warning => (HugeIcon::Info, colors.warning),
        ToastTone::Loss => (HugeIcon::Failure, colors.danger),
        ToastTone::Muted => (HugeIcon::Info, colors.icon),
    }
}

/// The card every toast is drawn on.
fn toast_surface(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .shadow_md()
}

/// A toast's content: its icon, title and description.
fn toast_body(
    tone: ToastTone,
    title: SharedString,
    description: Option<SharedString>,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let (icon, icon_color) = tone_icon(tone, theme);
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .child(
            Icon::new(icon.path())
                .color(gpui_color(icon_color))
                .with_size(px(ICON_SIZE))
                .flex_none(),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_primary))
                        .child(title),
                )
                .children(description.map(|description| {
                    div()
                        .text_xs()
                        .font_features(platform_tabular_numerals())
                        .text_color(gpui_color(colors.text_secondary))
                        .child(description)
                })),
        )
}

/// A chart condition shown in the middle of a chart, drawn like a toast.
pub(super) fn chart_status_card(notice: ChartSurfaceNotice, theme: &AerisTheme) -> Div {
    toast_surface(theme)
        .max_w(px(TOAST_WIDTH))
        .child(toast_body(
            notice.tone.into(),
            SharedString::new_static(notice.label),
            notice.detail.map(SharedString::from),
            theme,
        ))
}

pub(super) struct ToastLayer<'a, H: ToastHost> {
    pub(super) host: &'a Entity<H>,
    pub(super) toaster: &'a Toaster,
    pub(super) stack: ToastStack,
    pub(super) price_axis_width: f32,
    pub(super) theme: &'a AerisTheme,
    pub(super) now: Instant,
}

/// A pane's toast stack, in its top-right corner past the price axis. The stack's area blocks
/// the chart beneath it, gaps included, so moving between toasts keeps it expanded.
pub(super) fn toast_stack<H: ToastHost>(layer: &ToastLayer<'_, H>) -> Option<AnyElement> {
    let ToastLayer {
        host,
        toaster,
        stack,
        price_axis_width,
        theme,
        now,
    } = *layer;
    // Oldest first, so the newest toast paints on top.
    let cards: Vec<AnyElement> = toaster
        .toasts
        .iter()
        .filter(|toast| toast.stack == stack)
        .map(|toast| toast_card(host, toast, theme, now))
        .collect();
    if cards.is_empty() {
        return None;
    }
    let swiping = toaster.swipe.is_some_and(|swipe| swipe.stack == stack);
    let hover_host = host.clone();
    Some(
        div()
            .id(("toast_stack", stack.0.as_u64()))
            .absolute()
            .top(px(STACK_INSET))
            .right(px(price_axis_width + STACK_INSET))
            .w(px(TOAST_WIDTH))
            .h(px(toaster.extent(stack)))
            .occlude()
            .debug_selector(|| "toast_stack".into())
            .on_hover(move |hovered, _, cx| {
                hover_host.update(cx, |host, host_cx| {
                    if host
                        .toaster_mut()
                        .set_hovered(stack, *hovered, Instant::now())
                    {
                        host_cx.notify();
                    }
                });
            })
            .children(cards)
            .when(swiping, |layer| layer.child(swipe_capture(host.clone())))
            .into_any_element(),
    )
}

fn toast_card<H: ToastHost>(
    host: &Entity<H>,
    toast: &Toast,
    theme: &AerisTheme,
    now: Instant,
) -> AnyElement {
    let frame = toast.frame(now);
    let border = f32::from(platform_border_width(theme));
    // A narrower card behind the front one stays centred; its content keeps the full width so
    // nothing reflows while it shrinks.
    let inset = (TOAST_WIDTH - frame.width) / 2.0;
    let id = toast.id;
    let measure_host = host.clone();
    let press_host = host.clone();
    let release_host = host.clone();
    toast_surface(theme)
        .on_children_prepainted(move |bounds, _, cx| {
            let Some(content) = bounds.first() else {
                return;
            };
            let height = f32::from(content.size.height) + 2.0 * border;
            measure_host.update(cx, |host, host_cx| {
                if host.toaster_mut().measure(id, height, Instant::now()) {
                    host_cx.notify();
                }
            });
        })
        .id(("toast", id))
        .absolute()
        .top(px(frame.y))
        .left(px(frame.x + inset))
        .w(px(frame.width))
        .h(px(frame.height))
        .opacity(frame.opacity)
        .overflow_hidden()
        .cursor_pointer()
        .debug_selector(|| "toast".into())
        .when(toast.is_live(), |card| {
            card.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                cx.stop_propagation();
                press_host.update(cx, |host, host_cx| {
                    if host.toaster_mut().begin_swipe(
                        id,
                        f32::from(event.position.x),
                        Instant::now(),
                    ) {
                        host_cx.notify();
                    }
                });
            })
            // A click can be released before the swipe capture is drawn.
            .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                release_host.update(cx, |host, host_cx| {
                    if host
                        .toaster_mut()
                        .end_swipe(f32::from(event.position.x), Instant::now())
                    {
                        host_cx.notify();
                    }
                });
                cx.stop_propagation();
            })
        })
        .child(
            toast_body(
                toast.tone,
                toast.title.clone(),
                toast.description.clone(),
                theme,
            )
            .absolute()
            .top_0()
            .left(px(-inset))
            .w(px(TOAST_WIDTH - 2.0 * border))
            .opacity(frame.content_opacity),
        )
        .into_any_element()
}

/// Follows the pointer anywhere in the window while a toast is being swiped, the way a pointer
/// capture would.
fn swipe_capture<H: ToastHost>(host: Entity<H>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, (), window, _| {
            let move_host = host.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                move_host.update(cx, |host, host_cx| {
                    if host
                        .toaster_mut()
                        .move_swipe(f32::from(event.position.x), Instant::now())
                    {
                        host_cx.notify();
                    }
                });
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                host.update(cx, |host, host_cx| {
                    if host
                        .toaster_mut()
                        .end_swipe(f32::from(event.position.x), Instant::now())
                    {
                        host_cx.notify();
                    }
                });
                cx.stop_propagation();
            });
        },
    )
    .absolute()
    .size_full()
}

impl ToastHost for TerminalApp {
    fn toaster_mut(&mut self) -> &mut Toaster {
        &mut self.toaster
    }
}

impl TerminalApp {
    pub(super) fn announce_fills(
        &mut self,
        snapshot: &aeris_trading_runtime::TradingSnapshot,
        cx: &mut Context<Self>,
    ) {
        let host = toast_host(&self.workspaces[self.active]);
        if self.toaster.announce_fills(
            snapshot,
            host,
            terminal_chrome::current_unix_nanos(),
            Instant::now(),
        ) {
            cx.notify();
        }
    }

    /// Brings the toasts up to date for this frame: each visible pane's chart condition, timers,
    /// exits, the next frame while anything moves and a wake-up for the next expiry.
    pub(super) fn update_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let panes: BTreeSet<ToastStack> = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .map(|pane| ToastStack::of(&pane.surface))
            .collect();
        self.toaster.retain_stacks(|stack| panes.contains(&stack));
        for pane in &self.workspaces[self.active].panes {
            let notice = pane_chart_notice(pane.surface.read(cx), cx);
            self.toaster
                .show_chart_status(ToastStack::of(&pane.surface), notice, now);
        }
        self.toaster
            .tick(now, self.window_active, cx.reduce_motion());
        if self.toaster.is_animating(now) {
            window.request_animation_frame();
        }
        if let Some(deadline) = self.toaster.claim_wake() {
            cx.spawn(async move |terminal, cx| {
                cx.background_executor()
                    .timer(deadline.saturating_duration_since(Instant::now()))
                    .await;
                let _ = terminal.update(cx, |terminal, terminal_cx| {
                    terminal.toaster.wake_fired(deadline);
                    terminal_cx.notify();
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, point};

    const NOW_UNIX: i64 = 1_800_000_000_000_000_000;

    fn stack(id: u64) -> ToastStack {
        ToastStack(EntityId::from(id))
    }

    fn approx(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 0.01
    }

    fn fill(id: &str, side: aeris_trading::OrderSide, executed: i64) -> aeris_trading::Fill {
        aeris_trading::Fill {
            id: aeris_trading::FillId::try_new(id).expect("fill id"),
            order_id: aeris_trading::OrderId::try_new(format!("order-{id}")).expect("order id"),
            account_id: aeris_trading::TradingAccountId::try_new("account").expect("account"),
            instrument_id: aeris_instruments::InstrumentId::try_new("EURUSD").expect("instrument"),
            side,
            price: aeris_trading::FixedPoint::try_new(108_412, 5).expect("price"),
            quantity: aeris_trading::FixedPoint::try_new(1, 2).expect("quantity"),
            execution_unix_nanos: executed,
            provenance: aeris_trading::TradingProvenance {
                venue_id: "simulated".to_string(),
                provider_id: "simulated".to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: executed,
            },
        }
    }

    fn fresh_ids(
        watch: &mut FillWatch,
        revision: u64,
        fills: &[aeris_trading::Fill],
    ) -> Vec<String> {
        watch
            .fresh(revision, fills, NOW_UNIX)
            .into_iter()
            .map(|fill| fill.id.as_str().to_string())
            .collect()
    }

    fn toast(toaster: &mut Toaster, stack: ToastStack, now: Instant) -> u64 {
        toaster.push_fill(
            stack,
            aeris_trading::OrderSide::Buy,
            "Bought 0.01 EURUSD".into(),
            "at 1.08412 · account".into(),
            now,
        )
    }

    fn frame(toaster: &Toaster, id: u64, now: Instant) -> ToastFrame {
        toaster.toasts[toaster.index_of(id).expect("toast")].frame(now)
    }

    fn corner(label: &'static str, detail: &str) -> ChartSurfaceNotice {
        ChartSurfaceNotice {
            label,
            detail: Some(detail.to_string()),
            placement: ChartNoticePlacement::Corner,
            tone: ChartNoticeTone::Warning,
        }
    }

    fn status_toasts(toaster: &Toaster) -> Vec<(&'static str, Option<String>, bool)> {
        toaster
            .toasts
            .iter()
            .filter_map(|toast| match toast.kind {
                ToastKind::ChartStatus(label) => Some((
                    label,
                    toast.description.as_ref().map(ToString::to_string),
                    toast.is_live(),
                )),
                ToastKind::Fill => None,
            })
            .collect()
    }

    #[test]
    fn only_fills_that_arrive_after_the_first_snapshot_are_news() {
        let mut watch = FillWatch::default();
        let history = fill(
            "old",
            aeris_trading::OrderSide::Buy,
            NOW_UNIX - 5_000_000_000,
        );
        assert_eq!(
            fresh_ids(&mut watch, 1, std::slice::from_ref(&history)),
            Vec::<String>::new()
        );
        let new = fill(
            "new",
            aeris_trading::OrderSide::Sell,
            NOW_UNIX - 1_000_000_000,
        );
        let both = [history, new];
        assert_eq!(fresh_ids(&mut watch, 2, &both), ["new"]);
        assert_eq!(
            fresh_ids(&mut watch, 2, &both),
            Vec::<String>::new(),
            "the same revision is adopted once"
        );
        assert_eq!(
            fresh_ids(&mut watch, 3, &both),
            Vec::<String>::new(),
            "an announced fill is not announced again"
        );
    }

    #[test]
    fn fills_caught_up_after_a_reconnect_are_not_news() {
        let mut watch = FillWatch::default();
        fresh_ids(&mut watch, 1, &[]);
        let backfilled = fill(
            "backfilled",
            aeris_trading::OrderSide::Buy,
            NOW_UNIX - 3_600_000_000_000,
        );
        assert_eq!(
            fresh_ids(&mut watch, 2, &[backfilled]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_fill_reads_as_a_sentence_in_the_owners_exact_values() {
        let (title, description) = fill_notice_text(
            &fill("sold", aeris_trading::OrderSide::Sell, NOW_UNIX),
            &[],
            &[],
        );
        assert_eq!(title, "Sold 0.01 EURUSD");
        assert_eq!(description, "at 1.08412 · account");
    }

    #[test]
    fn a_new_toast_drops_in_from_above_and_settles_at_the_top() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let id = toast(&mut toaster, stack(1), start);
        let entering = frame(&toaster, id, start);
        assert!(approx(entering.y, -ESTIMATED_HEIGHT));
        assert!(approx(entering.opacity, 0.0));
        assert!(toaster.is_animating(start));

        let settled = start + MOVE_DURATION;
        let frame = frame(&toaster, id, settled);
        assert!(approx(frame.y, 0.0) && approx(frame.opacity, 1.0));
        assert!(approx(frame.width, TOAST_WIDTH));
        assert!(!toaster.is_animating(settled));
    }

    #[test]
    fn collapsed_older_toasts_tuck_behind_the_newest() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let ids: Vec<u64> = (0..4)
            .map(|_| toast(&mut toaster, stack(1), start))
            .collect();
        toaster.measure(ids[3], 70.0, start);
        toaster.measure(ids[2], 50.0, start);
        let settled = start + MOVE_DURATION;
        let front = frame(&toaster, ids[3], settled);
        assert!(approx(front.y, 0.0) && approx(front.content_opacity, 1.0));

        let behind = frame(&toaster, ids[2], settled);
        assert!(
            approx(behind.y, COLLAPSED_PEEK),
            "peeks out below the front"
        );
        assert!(
            approx(behind.height, 70.0),
            "takes the front toast's height"
        );
        assert!(approx(
            behind.width,
            TOAST_WIDTH * (1.0 - COLLAPSED_SCALE_STEP)
        ));
        assert!(approx(behind.content_opacity, 0.0), "its content is hidden");
        assert!(approx(frame(&toaster, ids[1], settled).opacity, 1.0));
        assert!(
            approx(frame(&toaster, ids[0], settled).opacity, 0.0),
            "only three toasts show while collapsed"
        );
        assert!(approx(
            toaster.extent(stack(1)),
            70.0 + 2.0 * COLLAPSED_PEEK
        ));
    }

    #[test]
    fn hovering_expands_the_stack_into_a_measured_list_and_leaving_collapses_it() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let older = toast(&mut toaster, stack(1), start);
        let newer = toast(&mut toaster, stack(1), start);
        toaster.measure(older, 50.0, start);
        toaster.measure(newer, 70.0, start);
        let hovered = start + MOVE_DURATION;
        assert!(toaster.set_hovered(stack(1), true, hovered));
        assert!(!toaster.set_hovered(stack(1), true, hovered));

        let expanded = hovered + MOVE_DURATION;
        let behind = frame(&toaster, older, expanded);
        assert!(
            approx(behind.y, 70.0 + EXPANDED_GAP),
            "listed below the front"
        );
        assert!(approx(behind.height, 50.0) && approx(behind.width, TOAST_WIDTH));
        assert!(approx(behind.content_opacity, 1.0));
        assert!(approx(toaster.extent(stack(1)), 70.0 + EXPANDED_GAP + 50.0));

        assert!(toaster.set_hovered(stack(1), false, expanded));
        assert!(approx(
            frame(&toaster, older, expanded + MOVE_DURATION).y,
            COLLAPSED_PEEK
        ));
    }

    #[test]
    fn an_interrupted_transition_continues_from_where_it_is() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let older = toast(&mut toaster, stack(1), start);
        let midway = start + MOVE_DURATION / 2;
        let before = frame(&toaster, older, midway).y;
        assert!(before < 0.0, "still entering");
        toast(&mut toaster, stack(1), midway);
        let after = frame(&toaster, older, midway).y;
        assert!(approx(before, after), "{before} jumped to {after}");
        assert!(approx(
            frame(&toaster, older, midway + MOVE_DURATION).y,
            COLLAPSED_PEEK
        ));
    }

    #[test]
    fn timers_pause_while_hovered_or_the_window_is_inactive() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let id = toast(&mut toaster, stack(1), start);
        toaster.tick(start, true, false);
        assert_eq!(toaster.claim_wake(), Some(start + FILL_LIFETIME));
        assert_eq!(toaster.claim_wake(), None, "one wake-up per expiry");

        let hovered = start + Duration::from_secs(2);
        toaster.tick(hovered, true, false);
        toaster.set_hovered(stack(1), true, hovered);
        toaster.tick(hovered + Duration::from_secs(30), true, false);
        assert!(
            toaster.toasts[0].is_live(),
            "a hovered stack holds its toasts"
        );

        let left = hovered + Duration::from_secs(30);
        toaster.set_hovered(stack(1), false, left);
        toaster.tick(left, false, false);
        toaster.tick(left + Duration::from_secs(30), false, false);
        assert!(
            toaster.toasts[0].is_live(),
            "an inactive window holds them too"
        );

        let back = left + Duration::from_secs(30);
        toaster.tick(back, true, false);
        toaster.tick(back + Duration::from_secs(3), true, false);
        assert!(toaster.toasts[0].is_live(), "four seconds remain");
        let expiry = back + Duration::from_secs(4);
        assert!(toaster.tick(expiry, true, false));
        assert!(!toaster.toasts[0].is_live());
        assert!(toaster.tick(expiry + EXIT_DURATION, true, false));
        assert_eq!(toaster.index_of(id), None, "removed once its exit has run");
    }

    #[test]
    fn a_swipe_far_or_fast_enough_dismisses_and_a_short_one_springs_back() {
        let start = Instant::now();
        let slow = Duration::from_secs(2);
        let mut toaster = Toaster::default();
        let id = toast(&mut toaster, stack(1), start);

        assert!(toaster.begin_swipe(id, 100.0, start));
        assert!(
            toaster.is_expanded(stack(1)),
            "a swipe holds the stack open"
        );
        toaster.move_swipe(120.0, start + slow);
        assert!(approx(frame(&toaster, id, start + slow).x, 20.0));
        assert!(toaster.end_swipe(120.0, start + slow));
        assert!(toaster.toasts[0].is_live(), "short and slow springs back");
        assert!(approx(
            frame(&toaster, id, start + slow + SPRING_BACK_DURATION).x,
            0.0
        ));

        toaster.begin_swipe(id, 100.0, start);
        toaster.move_swipe(60.0, start + slow);
        let pulled = frame(&toaster, id, start + slow).x;
        assert!(
            pulled < 0.0 && pulled > -40.0 / 3.0,
            "the wrong way meets friction: {pulled}"
        );
        toaster.end_swipe(60.0, start + slow);
        assert!(toaster.toasts[0].is_live());

        toaster.begin_swipe(id, 100.0, start);
        toaster.end_swipe(100.0 + SWIPE_DISTANCE, start + slow);
        assert!(!toaster.toasts[0].is_live(), "far enough dismisses");
        assert!(!toaster.is_expanded(stack(1)));

        let flicked = toast(&mut toaster, stack(1), start);
        toaster.begin_swipe(flicked, 100.0, start);
        toaster.end_swipe(120.0, start + Duration::from_millis(50));
        assert!(
            !toaster.toasts[toaster.index_of(flicked).expect("flicked")].is_live(),
            "a fast flick dismisses"
        );

        let clicked = toast(&mut toaster, stack(1), start);
        toaster.begin_swipe(clicked, 100.0, start);
        toaster.end_swipe(101.0, start + slow);
        assert!(
            !toaster.toasts[toaster.index_of(clicked).expect("clicked")].is_live(),
            "a click dismisses"
        );
    }

    #[test]
    fn a_chart_condition_stays_while_it_holds_and_lifts_away_when_it_clears() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        toaster.show_chart_status(
            stack(1),
            Some(corner("Reconnecting chart", "retry 1")),
            start,
        );
        toaster.show_chart_status(
            stack(1),
            Some(corner("Reconnecting chart", "retry 2")),
            start,
        );
        assert_eq!(
            status_toasts(&toaster),
            [("Reconnecting chart", Some("retry 2".to_string()), true)],
            "a new detail updates the toast in place"
        );
        let later = start + Duration::from_secs(600);
        toaster.tick(later, true, false);
        assert!(toaster.toasts[0].is_live(), "a condition has no timer");

        toaster.show_chart_status(stack(1), Some(corner("Chart stale", "silent")), later);
        assert_eq!(
            status_toasts(&toaster),
            [
                ("Reconnecting chart", Some("retry 2".to_string()), false),
                ("Chart stale", Some("silent".to_string()), true)
            ]
        );
        toaster.show_chart_status(stack(1), None, later);
        assert!(status_toasts(&toaster).iter().all(|(_, _, live)| !live));
    }

    #[test]
    fn a_dismissed_condition_stays_away_until_it_changes() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let stale = Some(corner("Chart stale", "silent"));
        toaster.show_chart_status(stack(1), stale.clone(), start);
        let id = toaster.toasts[0].id;
        toaster.begin_swipe(id, 0.0, start);
        toaster.end_swipe(0.0, start + Duration::from_secs(1));
        toaster.show_chart_status(stack(1), stale.clone(), start);
        assert_eq!(toaster.live_indices(stack(1)), Vec::<usize>::new());

        toaster.show_chart_status(stack(1), None, start);
        toaster.show_chart_status(stack(1), stale, start);
        assert_eq!(
            toaster.live_indices(stack(1)).len(),
            1,
            "it returns once it recurs"
        );
    }

    #[test]
    fn loading_and_centred_conditions_never_become_toasts() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        let loading = ChartSurfaceNotice {
            label: ChartState::Loading.label(),
            ..corner("", "repairing")
        };
        toaster.show_chart_status(stack(1), Some(loading), start);
        let centred = ChartSurfaceNotice {
            placement: ChartNoticePlacement::Center,
            ..corner("Chart unavailable", "no data")
        };
        toaster.show_chart_status(stack(1), Some(centred), start);
        assert!(toaster.toasts.is_empty());
    }

    #[test]
    fn a_flood_of_fills_stays_bounded_and_keeps_the_chart_condition() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        toaster.show_chart_status(stack(1), Some(corner("Chart stale", "silent")), start);
        for _ in 0..40 {
            toast(&mut toaster, stack(1), start);
        }
        assert_eq!(toaster.live_indices(stack(1)).len(), MAXIMUM_TOASTS);
        assert!(toaster.toasts.len() <= 2 * MAXIMUM_TOASTS);
        assert!(
            status_toasts(&toaster).iter().any(|(_, _, live)| *live),
            "the oldest timed toasts leave first"
        );
    }

    #[test]
    fn toasts_of_closed_panes_are_dropped() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        toast(&mut toaster, stack(1), start);
        toast(&mut toaster, stack(2), start);
        toaster.set_hovered(stack(1), true, start);
        toaster.retain_stacks(|stack_id| stack_id == stack(2));
        assert_eq!(toaster.toasts.len(), 1);
        assert_eq!(toaster.hovered, None);
    }

    #[test]
    fn reduced_motion_settles_every_change_at_once() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        toaster.tick(start, true, true);
        let id = toast(&mut toaster, stack(1), start);
        assert!(approx(frame(&toaster, id, start).opacity, 1.0));
        assert!(!toaster.is_animating(start));
    }

    const PRICE_AXIS_WIDTH: f32 = 64.0;

    struct StackHarness {
        toaster: Toaster,
    }

    impl ToastHost for StackHarness {
        fn toaster_mut(&mut self) -> &mut Toaster {
            &mut self.toaster
        }
    }

    impl Render for StackHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let host = cx.entity();
            div()
                .relative()
                .size_full()
                .children(toast_stack(&ToastLayer {
                    host: &host,
                    toaster: &self.toaster,
                    stack: stack(1),
                    price_axis_width: PRICE_AXIS_WIDTH,
                    theme: &AerisTheme::dark(),
                    now: Instant::now(),
                }))
        }
    }

    fn harness(cx: &mut TestAppContext) -> (Entity<StackHarness>, &mut gpui::VisualTestContext) {
        cx.add_window_view(|_, cx| {
            gpui_base::init(cx);
            let mut toaster = Toaster::default();
            toaster.tick(Instant::now(), true, true);
            toast(&mut toaster, stack(1), Instant::now());
            StackHarness { toaster }
        })
    }

    #[gpui::test]
    fn the_stack_sits_top_right_clear_of_the_price_axis(cx: &mut TestAppContext) {
        let (_, cx) = harness(cx);
        cx.run_until_parked();
        let viewport = cx.update(|window, _| window.viewport_size());
        let stack = cx.debug_bounds("toast_stack").expect("toast stack");
        assert_eq!(f32::from(stack.origin.y), STACK_INSET);
        let right_inset = f32::from(viewport.width - (stack.origin.x + stack.size.width));
        assert!(
            (right_inset - (PRICE_AXIS_WIDTH + STACK_INSET)).abs() <= 0.5,
            "the stack clears the price axis: {right_inset} from the right edge"
        );
    }

    #[gpui::test]
    fn the_pointer_expands_the_stack_and_a_swipe_dismisses(cx: &mut TestAppContext) {
        let (view, cx) = harness(cx);
        cx.run_until_parked();
        let card = cx.debug_bounds("toast").expect("toast");
        let toast_height = f32::from(card.size.height);
        let measured = view.read_with(cx, |harness, _| harness.toaster.toasts[0].height());
        assert!(
            (measured - toast_height).abs() <= 0.5,
            "the toast measured its own height: {measured} vs {toast_height}"
        );

        let centre = card.center();
        cx.simulate_mouse_move(centre, None, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |harness, _| harness.toaster.hovered),
            Some(stack(1)),
            "the toast inside does not hide the stack's hover"
        );

        cx.simulate_mouse_down(centre, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let released = point(centre.x + px(80.0), centre.y);
        cx.simulate_mouse_move(released, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(released, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let (live, carried_right) = view.read_with(cx, |harness, _| {
            let toast = &harness.toaster.toasts[0];
            (toast.is_live(), toast.motion.x.to > 0.0)
        });
        assert!(!live && carried_right, "the swipe carried the toast away");
    }
}
