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
//! Chart status toasts stay while their condition holds. Fill toasts announce new executions
//! and order toasts announce broker orders the broker ended on its own, with its reason; the
//! trading owner stays authoritative for both, and this module only remembers what it has
//! already announced.

use super::notification_center::{
    NoticeAction, NoticeKey, NoticeLifetime, NoticeTone, NotificationCenter, UserNotice,
};
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
/// Matches the operating-system notification's timeout.
const PRICE_ALERT_LIFETIME: Duration = Duration::from_secs(10);
/// A swipe this far right dismisses however slowly it moved.
const SWIPE_DISTANCE: f32 = 45.0;
/// A swipe faster than this, in logical pixels per millisecond, dismisses however short it was.
const SWIPE_VELOCITY: f32 = 0.11;
/// A press that travels less than this is a click.
const CLICK_SLOP: f32 = 3.0;
const ICON_SIZE: f32 = 16.0;
/// A fill or order event first reported this long after it happened is history the owner
/// caught up on after a reconnect or restart, not news.
const NEWS_FRESHNESS_NANOS: i64 = 60_000_000_000;
const ORDER_END_LIFETIME: Duration = Duration::from_secs(10);

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
    PriceAlert,
    Notice,
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
    notice_key: Option<NoticeKey>,
    tone: ToastTone,
    title: SharedString,
    description: Option<SharedString>,
    lifetime: Option<Duration>,
}

#[derive(Clone, Debug)]
struct Toast {
    id: u64,
    notice_key: Option<NoticeKey>,
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

/// Characterization fixture for the former snapshot news behavior.
#[cfg(test)]
#[derive(Debug)]
struct NewsWatch<K> {
    revision: Option<u64>,
    /// `None` until the first snapshot, whose facts are history.
    seen: Option<BTreeSet<K>>,
}

#[cfg(test)]
impl<K> Default for NewsWatch<K> {
    fn default() -> Self {
        Self {
            revision: None,
            seen: None,
        }
    }
}

#[cfg(test)]
impl<K: Ord + Clone> NewsWatch<K> {
    /// The items that are news in this snapshot, oldest first by `happened`.
    fn fresh<'a, T>(
        &mut self,
        revision: u64,
        items: &'a [T],
        key: impl Fn(&T) -> &K,
        happened: impl Fn(&T) -> i64,
        now_unix_nanos: i64,
    ) -> Vec<&'a T> {
        if self.revision == Some(revision) {
            return Vec::new();
        }
        self.revision = Some(revision);
        let current = items.iter().map(|item| key(item).clone()).collect();
        let Some(seen) = self.seen.replace(current) else {
            return Vec::new();
        };
        let mut fresh: Vec<_> = items
            .iter()
            .filter(|item| {
                !seen.contains(key(item))
                    && now_unix_nanos.saturating_sub(happened(item)) <= NEWS_FRESHNESS_NANOS
            })
            .collect();
        fresh.sort_by_key(|item| happened(item));
        fresh
    }
}

/// Every toast in the window, oldest first, and the pointer state of their stacks.
#[derive(Debug, Default)]
pub(super) struct Toaster {
    toasts: Vec<Toast>,
    notices: NotificationCenter,
    next_id: u64,
    hovered: Option<ToastStack>,
    swipe: Option<Swipe>,
    /// Timers run only while the window is active; it counts as inactive until the first tick.
    window_active: bool,
    reduce_motion: bool,
    /// A chart condition the trader dismissed stays dismissed until it changes or clears.
    dismissed_status: BTreeMap<ToastStack, &'static str>,
    /// The timer expiry a wake-up is already scheduled for.
    wake: Option<Instant>,
}

impl Toaster {
    #[cfg(test)]
    pub(super) fn notices(&self) -> &[UserNotice] {
        self.notices.notices()
    }

    #[cfg(test)]
    pub(super) fn live_notice_cards(&self, stack: ToastStack) -> usize {
        self.toasts
            .iter()
            .filter(|toast| toast.stack == stack && toast.notice_key.is_some() && toast.is_live())
            .count()
    }

    /// A keyed notice has one presentation card even if the failure repeats. The center is the
    /// owner of its lifetime and bound; the toast is only the animated visible projection.
    pub(super) fn post_notice(&mut self, stack: ToastStack, notice: UserNotice, now: Instant) {
        let key = notice.key.clone();
        let evicted = self.notices.post(notice);
        if let Some(evicted) = evicted {
            self.remove_notice_toast(&evicted, now);
        }
        let Some(notice) = self.notices.notices().iter().find(|item| item.key == key) else {
            return;
        };
        let tone = match notice.tone {
            NoticeTone::Warning => ToastTone::Warning,
            NoticeTone::Loss => ToastTone::Loss,
        };
        let title = notice.title.into();
        let description = Some(notice.detail.clone().into());
        let lifetime = match notice.lifetime {
            NoticeLifetime::Timed(duration) => Some(duration),
            NoticeLifetime::UntilCleared | NoticeLifetime::UntilDismissed => None,
        };
        if let Some(toast) = self
            .toasts
            .iter_mut()
            .find(|toast| toast.notice_key.as_ref() == Some(&key) && toast.is_live())
        {
            toast.tone = tone;
            toast.title = title;
            toast.description = description;
            toast.timer = lifetime.map(|remaining| ToastTimer {
                remaining,
                running_since: None,
            });
            self.sync_timers(now);
            return;
        }
        self.push(
            stack,
            ToastContent {
                kind: ToastKind::Notice,
                notice_key: Some(key),
                tone,
                title,
                description,
                lifetime,
            },
            now,
        );
    }

    fn remove_notice_toast(&mut self, key: &NoticeKey, now: Instant) {
        if let Some(index) = self
            .toasts
            .iter()
            .position(|toast| toast.notice_key.as_ref() == Some(key) && toast.is_live())
        {
            self.begin_exit(index, ExitPath::Lift, now);
        }
    }

    pub(super) fn clear_notice(&mut self, key: &NoticeKey, now: Instant) {
        if self.notices.clear(key) {
            self.remove_notice_toast(key, now);
            self.relayout(now);
        }
    }

    fn notice_action(&self, key: &NoticeKey) -> Option<NoticeAction> {
        self.notices
            .notices()
            .iter()
            .find(|notice| &notice.key == key)
            .and_then(|notice| notice.action.clone())
    }

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
            notice_key,
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
            notice_key,
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
        let notice_key = toast.notice_key.clone();
        if let ToastKind::ChartStatus(label) = toast.kind {
            self.dismissed_status.insert(toast.stack, label);
        }
        if let Some(key) = notice_key {
            self.notices.clear(&key);
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
                notice_key: None,
                tone: notice.tone.into(),
                title: SharedString::new_static(notice.label),
                description: notice.detail.map(SharedString::from),
                lifetime: None,
            },
            now,
        );
    }

    /// Projects the store's already deduplicated event IDs. This never compares snapshots.
    fn post_trading_announcements(
        &mut self,
        announcements: &[trading_store::TradingAnnouncement],
        snapshot: &aeris_trading_runtime::WorkingSet,
        host: Option<ToastStack>,
        now_unix_nanos: i64,
        now: Instant,
    ) -> bool {
        let Some(host) = host else {
            return false;
        };
        let mut posted = false;
        for item in announcements {
            match item {
                trading_store::TradingAnnouncement::Fill(fill) => {
                    if now_unix_nanos.saturating_sub(fill.execution_unix_nanos)
                        > NEWS_FRESHNESS_NANOS
                    {
                        continue;
                    }
                    let (title, description) =
                        fill_notice_text(fill, &snapshot.instruments, &snapshot.accounts);
                    self.push_fill(host, fill.side, title, description, now);
                }
                trading_store::TradingAnnouncement::Order { event, order } => {
                    if now_unix_nanos.saturating_sub(event.event_unix_nanos) > NEWS_FRESHNESS_NANOS
                    {
                        continue;
                    }
                    let order = order.as_deref();
                    let (tone, title, description) =
                        order_warning_notice(event, order, &snapshot.accounts)
                            .or_else(|| order_end_notice(event, order, &snapshot.accounts))
                            .unwrap_or_else(|| {
                                let (title, tone) = match event.reason {
                                    Some(aeris_trading::OrderEventReason::ProtectionNotPlaced) => {
                                        ("Protection not placed", ToastTone::Loss)
                                    }
                                    Some(aeris_trading::OrderEventReason::KillSwitch) => {
                                        ("Risk lock", ToastTone::Warning)
                                    }
                                    _ => ("Order rejected", ToastTone::Loss),
                                };
                                (
                                    tone,
                                    title.to_string(),
                                    event.detail.clone().unwrap_or_default(),
                                )
                            });
                    if event.reason == Some(aeris_trading::OrderEventReason::RiskWarning) {
                        self.post_notice(host, risk_warning_user_notice(event, description), now);
                    } else {
                        self.push(
                            host,
                            ToastContent {
                                kind: ToastKind::Notice,
                                notice_key: None,
                                tone,
                                title: title.into(),
                                description: Some(description.into()),
                                lifetime: Some(ORDER_END_LIFETIME),
                            },
                            now,
                        );
                    }
                }
            }
            posted = true;
        }
        posted
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
                notice_key: None,
                tone,
                title: title.into(),
                description: Some(description.into()),
                lifetime: Some(FILL_LIFETIME),
            },
            now,
        )
    }

    fn push_price_alert(
        &mut self,
        stack: ToastStack,
        announcement: PriceAlertAnnouncement,
        now: Instant,
    ) -> u64 {
        self.push(
            stack,
            ToastContent {
                kind: ToastKind::PriceAlert,
                notice_key: None,
                tone: ToastTone::Warning,
                title: announcement.title.into(),
                description: Some(announcement.description.into()),
                lifetime: Some(PRICE_ALERT_LIFETIME),
            },
            now,
        )
    }

    /// Drops the toasts of panes that no longer exist.
    fn retain_stacks(&mut self, live: impl Fn(ToastStack) -> bool) {
        let retired_keys: Vec<_> = self
            .toasts
            .iter()
            .filter(|toast| !live(toast.stack))
            .filter_map(|toast| toast.notice_key.clone())
            .collect();
        for key in retired_keys {
            self.notices.clear(&key);
        }
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
            if let Some(key) = &self.toasts[index].notice_key {
                self.notices.clear(key);
            }
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

fn risk_warning_user_notice(event: &aeris_trading::OrderEvent, detail: String) -> UserNotice {
    use std::hash::{Hash, Hasher};
    let mut key = std::collections::hash_map::DefaultHasher::new();
    event.id.as_str().hash(&mut key);
    UserNotice {
        key: NoticeKey {
            scope: key.finish(),
            kind: "trading_risk_warning",
        },
        severity: notification_center::NoticeSeverity::Warning,
        tone: NoticeTone::Warning,
        source: notification_center::NoticeSource::Workspace,
        title: "Risk warning",
        detail,
        action: None,
        lifetime: NoticeLifetime::Timed(ORDER_END_LIFETIME),
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

/// "Broker cancelled Buy 1 ESZ5" over the broker's reason and the account, for a broker order
/// the broker cancelled, expired or refused on its own; "Buy 1 ESZ5 expired" over the owner's
/// detail and the account, for a practice order whose time in force ended; `None` for every
/// other event, including the cancels Aeris requested.
fn order_end_notice(
    event: &aeris_trading::OrderEvent,
    order: Option<&trading_store::AnnouncedOrder>,
    accounts: &[aeris_trading::TradingAccount],
) -> Option<(ToastTone, String, String)> {
    if event.reason == Some(aeris_trading::OrderEventReason::UserCancel) {
        return None;
    }
    let placed = order.map(|order| &order.order);
    if event.provenance.venue_id != aeris_trading::venue::BROKER_VENUE_ID {
        if event.kind != aeris_trading::OrderEventKind::Expired {
            return None;
        }
        let title = order.map_or_else(
            || "Order expired".to_string(),
            |order| format!("{} expired", order_label(order)),
        );
        let reason = event.detail.as_deref().unwrap_or("Its time in force ended");
        return Some((
            ToastTone::Warning,
            title,
            with_account(reason, placed, accounts),
        ));
    }
    let (verb, tone) = match event.kind {
        aeris_trading::OrderEventKind::Cancelled => ("cancelled", ToastTone::Warning),
        aeris_trading::OrderEventKind::Expired => ("expired", ToastTone::Warning),
        aeris_trading::OrderEventKind::Rejected => ("rejected", ToastTone::Loss),
        _ => return None,
    };
    let title = order.map_or_else(
        || format!("Broker {verb} an order"),
        |order| format!("Broker {verb} {}", order_label(order)),
    );
    // The owner stores the bare state name when the broker gave no explanation.
    let reason = event
        .detail
        .as_deref()
        .filter(|detail| *detail != verb && *detail != "cancel rejected")
        .unwrap_or("The broker gave no reason");
    Some((tone, title, with_account(reason, placed, accounts)))
}

/// "Risk warning on Buy 1 ESZ5" over the owner's warnings and the account, for an order the
/// trading owner accepted with risk warnings on any venue; `None` for every other event.
fn order_warning_notice(
    event: &aeris_trading::OrderEvent,
    order: Option<&trading_store::AnnouncedOrder>,
    accounts: &[aeris_trading::TradingAccount],
) -> Option<(ToastTone, String, String)> {
    if event.kind != aeris_trading::OrderEventKind::Accepted
        || event.reason != Some(aeris_trading::OrderEventReason::RiskWarning)
    {
        return None;
    }
    let warnings = event.detail.as_deref()?;
    let title = order.map_or_else(
        || "Risk warning".to_string(),
        |order| format!("Risk warning on {}", order_label(order)),
    );
    Some((
        ToastTone::Warning,
        title,
        with_account(
            &capitalized(warnings),
            order.map(|order| &order.order),
            accounts,
        ),
    ))
}

/// "Buy 1 ESZ5": the order's side, quantity and display symbol.
fn order_label(announced: &trading_store::AnnouncedOrder) -> String {
    let order = &announced.order;
    let side = match order.side {
        aeris_trading::OrderSide::Buy => "Buy",
        aeris_trading::OrderSide::Sell => "Sell",
    };
    let symbol = announced
        .instrument
        .as_ref()
        .map_or(order.instrument_id.as_str(), |instrument| {
            instrument.contract.provenance.display_symbol.as_str()
        });
    let quantity = market_price_text(order.quantity.units(), u32::from(order.quantity.scale()));
    format!("{side} {quantity} {symbol}")
}

fn with_account(
    text: &str,
    order: Option<&aeris_trading::Order>,
    accounts: &[aeris_trading::TradingAccount],
) -> String {
    order
        .and_then(|order| {
            accounts
                .iter()
                .find(|account| account.id == order.account_id)
        })
        .map_or_else(
            || text.to_string(),
            |account| format!("{text} · {}", account.display_name),
        )
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The view that owns the toaster its stacks render and update.
pub(super) trait ToastHost: 'static {
    fn toaster_mut(&mut self) -> &mut Toaster;
    fn activate_notice_action(&mut self, action: NoticeAction, cx: &mut Context<Self>)
    where
        Self: Sized;
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

fn toast_radius() -> Pixels {
    px(f32::from(RadiusToken::Default.logical_pixels()))
}

/// The card every toast is drawn on. Its first child must stay the measured content; the
/// popover highlight is appended last.
fn toast_surface(theme: &AerisTheme) -> Div {
    popover_surface(div(), theme, toast_radius())
}

/// A toast's content: its icon, title and description.
fn toast_body(
    tone: ToastTone,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
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
                        .text_color(gpui_color(colors.text_default))
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
        .children(action)
}

/// A chart condition shown in the middle of a chart, drawn like a toast.
pub(super) fn chart_status_card(notice: ChartSurfaceNotice, theme: &AerisTheme) -> Div {
    toast_surface(theme)
        .max_w(px(TOAST_WIDTH))
        .child(toast_body(
            notice.tone.into(),
            SharedString::new_static(notice.label),
            notice.detail.map(SharedString::from),
            None,
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
        .map(|toast| {
            let action = toast
                .notice_key
                .as_ref()
                .and_then(|key| toaster.notice_action(key));
            toast_card(host, toast, action, theme, now)
        })
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
    action: Option<NoticeAction>,
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
    let action_host = host.clone();
    let action_key = toast.notice_key.clone();
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
        // A swipe surface, not a click target; the arrow also keeps the chart's crosshair from
        // showing through.
        .cursor_default()
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
                action.map(|action| {
                    div()
                        .id(("toast_action", id))
                        .debug_selector(|| "toast_action".into())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new(("toast_notice_action", id), theme)
                                .variant(ButtonVariant::Secondary)
                                .button_size(ButtonSize::Sm)
                                .label(action.label())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let Some(key) = action_key.as_ref() else {
                                        return;
                                    };
                                    action_host.update(cx, |host, host_cx| {
                                        if host.toaster_mut().notice_action(key).as_ref()
                                            == Some(&action)
                                        {
                                            host.activate_notice_action(action.clone(), host_cx);
                                        }
                                    });
                                }),
                        )
                        .into_any_element()
                }),
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
        self.notices.toaster_mut()
    }

    fn activate_notice_action(&mut self, action: NoticeAction, cx: &mut Context<Self>) {
        match action {
            NoticeAction::OpenFolder(path) => {
                cx.background_executor()
                    .spawn(async move {
                        if let Err(error) = aeris_platform_runtime::reveal_in_file_manager(&path) {
                            diagnostic!(
                                "Aeris could not open the folder {}: {error}",
                                path.display()
                            );
                        }
                    })
                    .detach();
            }
        }
    }
}

impl TerminalApp {
    pub(super) fn post_trading_announcements(
        &mut self,
        announcements: &[trading_store::TradingAnnouncement],
        snapshot: &aeris_trading_runtime::WorkingSet,
        cx: &mut Context<Self>,
    ) {
        let host = toast_host(self.tabs.active());
        if self.notices.toaster_mut().post_trading_announcements(
            announcements,
            snapshot,
            host,
            self.lifecycle.clock().wall_unix_nanos(),
            Instant::now(),
        ) {
            cx.notify();
        }
    }

    /// Shows every pane's triggered alerts. An alert from a chart in another workspace tab lands
    /// on the visible workspace so it is not missed.
    fn announce_price_alerts(&mut self, now: Instant, cx: &mut Context<Self>) {
        let visible_host = toast_host(self.tabs.active());
        for (index, workspace) in self.tabs.iter().enumerate() {
            for pane in &workspace.panes {
                let announcements = pane
                    .surface
                    .update(cx, |surface, _| surface.take_price_alert_announcements());
                let host = if index == self.tabs.active_index() {
                    Some(ToastStack::of(&pane.surface))
                } else {
                    visible_host
                };
                let Some(host) = host else {
                    continue;
                };
                for announcement in announcements {
                    self.notices
                        .toaster_mut()
                        .push_price_alert(host, announcement, now);
                }
            }
        }
    }

    /// Brings the toasts up to date for this frame: each visible pane's chart condition, timers,
    /// exits, the next frame while anything moves and a wake-up for the next expiry.
    pub(super) fn update_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let panes: BTreeSet<ToastStack> = self
            .tabs
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .map(|pane| ToastStack::of(&pane.surface))
            .collect();
        self.notices
            .toaster_mut()
            .retain_stacks(|stack| panes.contains(&stack));
        for pane in &self.tabs.active().panes {
            let notice = pane_chart_notice(pane.surface.read(cx), cx);
            self.notices.toaster_mut().show_chart_status(
                ToastStack::of(&pane.surface),
                notice,
                now,
            );
        }
        self.announce_price_alerts(now, cx);
        let window_active = self.window.is_active();
        self.notices
            .toaster_mut()
            .tick(now, window_active, cx.reduce_motion());
        if self.notices.toaster().is_animating(now) {
            window.request_animation_frame();
        }
        if let Some(deadline) = self.notices.toaster_mut().claim_wake() {
            cx.spawn(async move |terminal, cx| {
                cx.background_executor()
                    .timer(deadline.saturating_duration_since(Instant::now()))
                    .await;
                if let Err(error) = terminal.update(cx, |terminal, terminal_cx| {
                    terminal.notices.toaster_mut().wake_fired(deadline);
                    terminal_cx.notify();
                }) && terminal.upgrade().is_some()
                {
                    diagnostic!("Live workspace toaster timer could not update: {error}");
                }
            })
            .detach();
        }
    }
}

#[cfg(test)]
pub(super) fn test_trading_toast_counts(
    announcements: &[trading_store::TradingAnnouncement],
    set: &aeris_trading_runtime::WorkingSet,
    now_unix_nanos: i64,
) -> (usize, usize) {
    let mut toaster = Toaster::default();
    let stack = ToastStack(EntityId::from(1));
    toaster.post_trading_announcements(
        announcements,
        set,
        Some(stack),
        now_unix_nanos,
        Instant::now(),
    );
    (
        toaster
            .toasts
            .iter()
            .filter(|toast| toast.kind == ToastKind::Fill)
            .count(),
        toaster.notices().len(),
    )
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
        watch: &mut NewsWatch<aeris_trading::FillId>,
        revision: u64,
        fills: &[aeris_trading::Fill],
    ) -> Vec<String> {
        watch
            .fresh(
                revision,
                fills,
                |fill| &fill.id,
                |fill| fill.execution_unix_nanos,
                NOW_UNIX,
            )
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

    #[test]
    fn notification_toaster_renders_one_card_for_repeated_failure() {
        let mut toaster = Toaster::default();
        let now = Instant::now();
        let notice = notification_center::UserNotice {
            key: notification_center::NoticeKey {
                scope: 0,
                kind: "save",
            },
            severity: notification_center::NoticeSeverity::Error,
            tone: notification_center::NoticeTone::Loss,
            source: notification_center::NoticeSource::Persistence,
            title: "Preferences unavailable",
            detail: "Try again".into(),
            action: None,
            lifetime: notification_center::NoticeLifetime::UntilDismissed,
        };
        toaster.post_notice(stack(1), notice.clone(), now);
        toaster.post_notice(stack(1), notice, now);
        assert_eq!(toaster.notices.notices().len(), 1);
        assert_eq!(toaster.live_indices(stack(1)).len(), 1);
        assert_eq!(toaster.toasts[0].title.as_ref(), "Preferences unavailable");
        toaster.dismiss(0, ExitPath::Lift, now);
        assert_eq!(toaster.notices.notices(), []);
    }

    #[test]
    fn notification_success_clears_persistent_save_failure() {
        let mut toaster = Toaster::default();
        let now = Instant::now();
        let key = notification_center::NoticeKey {
            scope: 0,
            kind: "workspace_save",
        };
        toaster.post_notice(
            stack(1),
            notification_center::UserNotice {
                key: key.clone(),
                severity: notification_center::NoticeSeverity::Error,
                tone: notification_center::NoticeTone::Loss,
                source: notification_center::NoticeSource::Persistence,
                title: "Workspace could not be saved",
                detail: "Your latest changes may not survive a restart.".into(),
                action: None,
                lifetime: notification_center::NoticeLifetime::UntilCleared,
            },
            now,
        );
        toaster.clear_notice(&key, now);
        assert_eq!(toaster.notices.notices(), []);
        assert_eq!(toaster.live_indices(stack(1)).len(), 0);
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
                ToastKind::Fill | ToastKind::PriceAlert | ToastKind::Notice => None,
            })
            .collect()
    }

    #[test]
    fn only_fills_that_arrive_after_the_first_snapshot_are_news() {
        let mut watch = NewsWatch::default();
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
        let mut watch = NewsWatch::default();
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
    fn trading_projection_baseline_fill_and_broker_rejection_news() {
        use aeris_trading::OrderEventKind;
        let mut fills = NewsWatch::default();
        let mut order_ends = NewsWatch::default();
        let fill = fill(
            "new-fill",
            aeris_trading::OrderSide::Sell,
            NOW_UNIX - 1_000_000_000,
        );
        let rejected = order_event(
            OrderEventKind::Rejected,
            None,
            aeris_trading::venue::BROKER_VENUE_ID,
            "margin exceeded",
        );
        assert_eq!(fresh_ids(&mut fills, 10, &[]), Vec::<String>::new());
        assert_eq!(
            order_ends
                .fresh(
                    10,
                    &[],
                    |event: &aeris_trading::OrderEvent| &event.id,
                    |event| event.event_unix_nanos,
                    NOW_UNIX
                )
                .len(),
            0
        );
        assert_eq!(
            fresh_ids(&mut fills, 11, std::slice::from_ref(&fill)),
            ["new-fill"]
        );
        assert_eq!(
            fill_notice_text(&fill, &[], &[]),
            (
                "Sold 0.01 EURUSD".to_string(),
                "at 1.08412 · account".to_string()
            )
        );
        let events = [rejected];
        let first = order_ends.fresh(
            11,
            &events,
            |event| &event.id,
            |event| event.event_unix_nanos,
            NOW_UNIX,
        );
        assert_eq!(first.len(), 1);
        assert_eq!(
            order_end_notice(first[0], None, &[]),
            Some((
                ToastTone::Loss,
                "Broker rejected an order".to_string(),
                "margin exceeded".to_string(),
            ))
        );
        assert_eq!(
            fresh_ids(&mut fills, 11, std::slice::from_ref(&fill)),
            Vec::<String>::new()
        );
        assert_eq!(
            order_ends
                .fresh(
                    11,
                    &events,
                    |event| &event.id,
                    |event| event.event_unix_nanos,
                    NOW_UNIX
                )
                .len(),
            0
        );
    }

    fn order_event(
        kind: aeris_trading::OrderEventKind,
        reason: Option<aeris_trading::OrderEventReason>,
        venue_id: &str,
        detail: &str,
    ) -> aeris_trading::OrderEvent {
        aeris_trading::OrderEvent {
            id: aeris_trading::OrderEventId::try_new("event-1").expect("event id"),
            order_id: aeris_trading::OrderId::try_new("order-1").expect("order id"),
            sequence: 1,
            kind,
            reason,
            event_unix_nanos: NOW_UNIX,
            detail: Some(detail.to_string()),
            provenance: aeris_trading::TradingProvenance {
                venue_id: venue_id.to_string(),
                provider_id: venue_id.to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: NOW_UNIX,
            },
        }
    }

    #[test]
    fn a_risk_warned_acceptance_is_announced_with_its_warnings() {
        use aeris_trading::{OrderEventKind as Kind, OrderEventReason as Reason};
        let notice = |event: &aeris_trading::OrderEvent| order_warning_notice(event, None, &[]);
        assert_eq!(
            notice(&order_event(
                Kind::Accepted,
                Some(Reason::RiskWarning),
                "aeris-sim",
                "fast re-entry within 60 seconds of a filled stop"
            )),
            Some((
                ToastTone::Warning,
                "Risk warning".to_string(),
                "Fast re-entry within 60 seconds of a filled stop".to_string()
            ))
        );
        assert_eq!(
            notice(&order_event(
                Kind::Accepted,
                None,
                "aeris-sim",
                "accepted by local simulated venue"
            )),
            None,
            "a plain acceptance is not news"
        );
    }

    #[test]
    fn risk_warning_event_has_one_keyed_warning_notice() {
        let event = order_event(
            aeris_trading::OrderEventKind::Accepted,
            Some(aeris_trading::OrderEventReason::RiskWarning),
            "aeris-sim",
            "risk threshold",
        );
        let notice = risk_warning_user_notice(&event, "Risk threshold".into());
        assert_eq!(
            notice.severity,
            notification_center::NoticeSeverity::Warning
        );
        let mut toaster = Toaster::default();
        let now = Instant::now();
        toaster.post_notice(stack(1), notice.clone(), now);
        toaster.post_notice(stack(1), notice, now);
        assert_eq!(toaster.notices().len(), 1);
        assert_eq!(toaster.live_notice_cards(stack(1)), 1);
    }

    #[test]
    fn only_orders_the_broker_ended_itself_are_announced_with_its_reason() {
        use aeris_trading::{OrderEventKind as Kind, OrderEventReason as Reason};
        let broker = aeris_trading::venue::BROKER_VENUE_ID;
        let notice = |event: &aeris_trading::OrderEvent| order_end_notice(event, None, &[]);
        assert_eq!(
            notice(&order_event(
                Kind::Cancelled,
                None,
                broker,
                "margin exceeded"
            )),
            Some((
                ToastTone::Warning,
                "Broker cancelled an order".to_string(),
                "margin exceeded".to_string()
            ))
        );
        assert_eq!(
            notice(&order_event(Kind::Rejected, None, broker, "rejected")),
            Some((
                ToastTone::Loss,
                "Broker rejected an order".to_string(),
                "The broker gave no reason".to_string()
            ))
        );
        assert_eq!(
            notice(&order_event(
                Kind::Cancelled,
                Some(Reason::UserCancel),
                broker,
                "cancelled"
            )),
            None,
            "a cancel Aeris asked for is not news"
        );
        assert_eq!(
            notice(&order_event(
                Kind::Cancelled,
                None,
                "aeris-sim",
                "cancelled"
            )),
            None
        );
        assert_eq!(
            notice(&order_event(Kind::Accepted, None, broker, "accepted")),
            None
        );
    }

    fn practice_order() -> trading_store::AnnouncedOrder {
        let point = |units, scale| aeris_trading::FixedPoint::try_new(units, scale).expect("point");
        trading_store::AnnouncedOrder {
            order: aeris_trading::Order {
                id: aeris_trading::OrderId::try_new("order-1").expect("order id"),
                client_order_id: aeris_trading::ClientOrderId::try_new("client-1")
                    .expect("client order id"),
                account_id: aeris_trading::TradingAccountId::try_new("account").expect("account"),
                instrument_id: aeris_instruments::InstrumentId::try_new("SOL-USDC")
                    .expect("instrument"),
                side: aeris_trading::OrderSide::Buy,
                order_type: aeris_trading::OrderType::Limit,
                time_in_force: aeris_trading::TimeInForce::Day,
                quantity: point(1, 1),
                filled_quantity: point(0, 1),
                limit_price: Some(point(18_000, 2)),
                stop_price: None,
                status: aeris_trading::OrderStatus::Working,
                submitted_unix_nanos: NOW_UNIX,
                revision: 1,
                accepted_unix_nanos: NOW_UNIX,
                updated_unix_nanos: NOW_UNIX,
                eligible_after_unix_nanos: NOW_UNIX,
                triggered_unix_nanos: None,
                expires_at_unix_nanos: Some(NOW_UNIX),
                terminal_unix_nanos: None,
                provenance: order_event(
                    aeris_trading::OrderEventKind::Accepted,
                    None,
                    "aeris-sim",
                    "accepted",
                )
                .provenance,
            },
            instrument: None,
        }
    }

    #[test]
    fn a_practice_order_whose_time_in_force_ended_is_announced() {
        use aeris_trading::{OrderEventKind as Kind, OrderEventReason as Reason};
        let expired = order_event(Kind::Expired, None, "aeris-sim", "DAY session ended");
        assert_eq!(
            order_end_notice(&expired, None, &[]),
            Some((
                ToastTone::Warning,
                "Order expired".to_string(),
                "DAY session ended".to_string()
            ))
        );
        let order = practice_order();
        assert_eq!(
            order_end_notice(&expired, Some(&order), &[]),
            Some((
                ToastTone::Warning,
                "Buy 0.1 SOL-USDC expired".to_string(),
                "DAY session ended".to_string()
            ))
        );
        assert_eq!(
            order_end_notice(
                &order_event(
                    Kind::Cancelled,
                    Some(Reason::Cascade),
                    "aeris-sim",
                    "cancelled"
                ),
                Some(&order),
                &[]
            ),
            None,
            "a practice group cancel is not news"
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
    fn a_triggered_price_alert_is_announced_for_its_full_lifetime() {
        let start = Instant::now();
        let mut toaster = Toaster::default();
        toaster.push_price_alert(
            stack(1),
            PriceAlertAnnouncement {
                title: "NQZ6 crossed 31258.00".to_string(),
                description: "Last trade 31258.25".to_string(),
            },
            start,
        );
        toaster.tick(start, true, false);
        let toast = &toaster.toasts[0];
        assert_eq!(toast.kind, ToastKind::PriceAlert);
        assert_eq!(toast.title.as_ref(), "NQZ6 crossed 31258.00");
        assert_eq!(toaster.claim_wake(), Some(start + PRICE_ALERT_LIFETIME));
        toaster.tick(start + PRICE_ALERT_LIFETIME / 2, true, false);
        assert!(toaster.toasts[0].is_live());
        toaster.tick(start + PRICE_ALERT_LIFETIME, true, false);
        assert!(
            !toaster.toasts[0].is_live(),
            "it leaves once its time is up"
        );
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
        toaster.post_notice(
            stack(1),
            notification_center::UserNotice {
                key: notification_center::NoticeKey {
                    scope: 1,
                    kind: "order_book_demand",
                },
                severity: notification_center::NoticeSeverity::Warning,
                tone: notification_center::NoticeTone::Warning,
                source: notification_center::NoticeSource::Pane(1),
                title: "Order book unavailable",
                detail: "Try again later".into(),
                action: None,
                lifetime: notification_center::NoticeLifetime::UntilCleared,
            },
            start,
        );
        toaster.set_hovered(stack(1), true, start);
        toaster.retain_stacks(|stack_id| stack_id == stack(2));
        assert_eq!(toaster.toasts.len(), 1);
        assert_eq!(toaster.notices.notices(), []);
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
        actions: Vec<NoticeAction>,
    }

    impl ToastHost for StackHarness {
        fn toaster_mut(&mut self) -> &mut Toaster {
            &mut self.toaster
        }

        fn activate_notice_action(&mut self, action: NoticeAction, _: &mut Context<Self>) {
            self.actions.push(action);
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
            StackHarness {
                toaster,
                actions: Vec::new(),
            }
        })
    }

    #[gpui::test]
    fn action_button_dispatches_without_dismissing_the_notice(cx: &mut TestAppContext) {
        let (view, cx) = harness(cx);
        let key = notification_center::NoticeKey {
            scope: 0,
            kind: "workspace_save",
        };
        let action = NoticeAction::OpenFolder(std::path::PathBuf::from("workspace"));
        view.update(cx, |host, cx| {
            host.toaster.post_notice(
                stack(1),
                notification_center::UserNotice {
                    key: key.clone(),
                    severity: notification_center::NoticeSeverity::Error,
                    tone: notification_center::NoticeTone::Loss,
                    source: notification_center::NoticeSource::Persistence,
                    title: "Workspace could not be saved",
                    detail: "Your changes may not survive a restart.".into(),
                    action: Some(action.clone()),
                    lifetime: notification_center::NoticeLifetime::UntilCleared,
                },
                Instant::now(),
            );
            cx.notify();
        });
        cx.run_until_parked();
        let button = cx
            .debug_bounds("toast_action")
            .expect("rendered notice action");
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked();
        view.read_with(cx, |host, _| {
            assert_eq!(host.actions, [action]);
            assert_eq!(
                host.toaster.notices().len(),
                1,
                "the action awaits its result"
            );
            assert_eq!(host.toaster.live_notice_cards(stack(1)), 1);
        });
        view.update(cx, |host, cx| {
            host.toaster.clear_notice(&key, Instant::now());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("toast_action").is_none());
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
