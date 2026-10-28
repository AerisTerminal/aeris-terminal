//! The workspace tab strip: the open tabs in display order, the selected tab and the tab a
//! pointer is dragging. Every change that moves, adds or removes a tab keeps the selection on
//! the same tab and refreshes the keyboard focus order; the terminal decides what to repaint,
//! persist and retire.

use std::ops::{Index, IndexMut};

use super::WorkspaceTab;

/// Space between neighboring tabs in the rendered strip.
const WORKSPACE_TAB_GAP: f32 = 2.0;
/// Space between the strip's left edge and its first tab.
const WORKSPACE_TAB_STRIP_PADDING_LEFT: f32 = 8.0;

/// A tab drag in progress: the dragged tab, where the pointer grabbed it and the last pointer
/// position the strip reported.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct WorkspaceDragState {
    tab_id: u64,
    cursor_offset_x: f32,
    pointer_x: Option<f32>,
    strip_left: f32,
}

impl WorkspaceDragState {
    /// How far the dragged tab `tab_id`, shown at `index`, is drawn from its slot so it follows
    /// the pointer without leaving the strip. Other tabs are not translated.
    pub(super) fn translation(self, tab_id: u64, index: usize, tab_widths: &[f32]) -> Option<f32> {
        if self.tab_id != tab_id {
            return None;
        }
        let pointer_x = self.pointer_x?;
        let dragged_width = *tab_widths.get(index)?;
        let index_offset = tab_widths
            .get(..index)?
            .iter()
            .fold(0.0, |offset, width| offset + width + WORKSPACE_TAB_GAP);
        let occupied_width = tab_widths
            .iter()
            .copied()
            .reduce(|total, width| total + WORKSPACE_TAB_GAP + width)?;
        let requested_left =
            pointer_x - self.cursor_offset_x - self.strip_left - WORKSPACE_TAB_STRIP_PADDING_LEFT;
        if !requested_left.is_finite() || !occupied_width.is_finite() {
            return None;
        }
        let bounded_left = requested_left.clamp(0.0, (occupied_width - dragged_width).max(0.0));
        Some(bounded_left - index_offset)
    }
}

/// What a pointer move during a tab drag changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TabDragMove {
    /// No drag of that tab is in progress, or the strip has no slot for the pointer.
    Unchanged,
    /// The dragged tab follows the pointer within its slot.
    Moved,
    /// The dragged tab moved into another slot.
    Reordered,
}

/// What closing a tab takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TabClose {
    /// The tab is the window's only tab, so the window closes with it.
    LastTab,
    /// The tab is not open.
    NotOpen,
    /// The tab can be removed; this tab is selected afterwards.
    Remove { next_active_id: u64 },
}

/// A tab [`TabStrip::remove`] took out of the strip.
pub(super) struct RemovedTab {
    pub(super) tab: WorkspaceTab,
    pub(super) was_active: bool,
    /// The removed tab was being dragged, so the platform drag must stop too.
    pub(super) drag_cancelled: bool,
}

pub(super) struct TabStrip {
    /// Never empty: closing the last tab closes the window instead.
    tabs: Vec<WorkspaceTab>,
    active: usize,
    drag: Option<WorkspaceDragState>,
}

impl TabStrip {
    /// Shows `tabs` in order with the tab `active_id` selected, or the first tab when that tab
    /// is not open.
    pub(super) fn new(tabs: Vec<WorkspaceTab>, active_id: Option<u64>) -> Self {
        let active = active_id
            .and_then(|id| tabs.iter().position(|tab| tab.id == id))
            .unwrap_or(0);
        let mut strip = Self {
            tabs,
            active,
            drag: None,
        };
        strip.refresh_focus_order();
        strip
    }

    pub(super) fn as_slice(&self) -> &[WorkspaceTab] {
        &self.tabs
    }

    pub(super) fn iter(&self) -> std::slice::Iter<'_, WorkspaceTab> {
        self.tabs.iter()
    }

    /// Tabs in display order; the order itself changes only through the strip.
    pub(super) fn iter_mut(&mut self) -> std::slice::IterMut<'_, WorkspaceTab> {
        self.tabs.iter_mut()
    }

    pub(super) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(super) fn get(&self, index: usize) -> Option<&WorkspaceTab> {
        self.tabs.get(index)
    }

    pub(super) fn position(&self, tab_id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == tab_id)
    }

    pub(super) const fn active_index(&self) -> usize {
        self.active
    }

    pub(super) fn active(&self) -> &WorkspaceTab {
        &self.tabs[self.active]
    }

    pub(super) fn active_mut(&mut self) -> &mut WorkspaceTab {
        &mut self.tabs[self.active]
    }

    /// Selects the tab at `next`, reporting the previously and newly selected indexes when the
    /// selection changed.
    pub(super) fn select(&mut self, next: usize) -> Option<(usize, usize)> {
        let switch = workspace_switch(self.active, next, self.tabs.len())?;
        self.active = next;
        Some(switch)
    }

    /// The tab `direction` steps from `current`, wrapping at either end.
    pub(super) fn wrapped_index(&self, current: usize, direction: isize) -> Option<usize> {
        wrapped_workspace_index(current, self.tabs.len(), direction)
    }

    /// Moves the selected tab `direction` slots, reporting whether it moved.
    pub(super) fn move_active(&mut self, direction: isize) -> bool {
        let Some(destination) = self.active.checked_add_signed(direction) else {
            return false;
        };
        if destination >= self.tabs.len() {
            return false;
        }
        self.reorder(self.tabs[self.active].id, destination)
    }

    /// Moves the tab `dragged_id` into the slot `destination_index`, clamped to the last slot,
    /// keeping the same tab selected. Reports whether the order changed.
    pub(super) fn reorder(&mut self, dragged_id: u64, destination_index: usize) -> bool {
        let Some(from) = self.position(dragged_id) else {
            return false;
        };
        let insertion_index = destination_index.min(self.tabs.len() - 1);
        if from == destination_index || insertion_index == from {
            return false;
        }
        let active_id = self.tabs[self.active].id;
        let dragged = self.tabs.remove(from);
        self.tabs.insert(insertion_index, dragged);
        self.active = self.position(active_id).unwrap_or(0);
        self.refresh_focus_order();
        true
    }

    /// Appends `tab` as the last tab and selects it.
    pub(super) fn push_active(&mut self, tab: WorkspaceTab) {
        self.tabs.push(tab);
        self.refresh_focus_order();
        self.active = self.tabs.len() - 1;
    }

    /// What closing the tab `tab_id` takes. A closed selected tab passes the selection to its
    /// right neighbor, or to its left neighbor when it was the last tab.
    pub(super) fn close_plan(&self, tab_id: u64) -> TabClose {
        if self.tabs.len() == 1 && self.tabs[0].id == tab_id {
            return TabClose::LastTab;
        }
        let ids = self.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>();
        active_workspace_after_close(&ids, self.tabs[self.active].id, tab_id)
            .map_or(TabClose::NotOpen, |next_active_id| TabClose::Remove {
                next_active_id,
            })
    }

    /// Removes the tab `tab_id` and selects `next_active_id`, the tab
    /// [`Self::close_plan`] named. A drag of the removed tab ends with it.
    pub(super) fn remove(&mut self, tab_id: u64, next_active_id: u64) -> Option<RemovedTab> {
        let index = self.position(tab_id)?;
        if self.tabs.len() == 1 {
            return None;
        }
        let was_active = self.tabs[self.active].id == tab_id;
        let tab = self.tabs.remove(index);
        let drag_cancelled = self.drag.is_some_and(|drag| drag.tab_id == tab_id);
        if drag_cancelled {
            self.drag = None;
        }
        self.active = self.position(next_active_id).unwrap_or(0);
        self.refresh_focus_order();
        Some(RemovedTab {
            tab,
            was_active,
            drag_cancelled,
        })
    }

    pub(super) const fn drag(&self) -> Option<WorkspaceDragState> {
        self.drag
    }

    pub(super) const fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Starts dragging the tab `tab_id`, grabbed `cursor_offset_x` from its left edge; an
    /// offset that is not finite grabs the tab's middle. Reports whether the tab is open.
    pub(super) fn begin_drag(&mut self, tab_id: u64, cursor_offset_x: f32, tab_width: f32) -> bool {
        if self.position(tab_id).is_none() {
            return false;
        }
        self.drag = Some(WorkspaceDragState {
            tab_id,
            cursor_offset_x: if cursor_offset_x.is_finite() {
                cursor_offset_x.clamp(0.0, tab_width)
            } else {
                tab_width / 2.0
            },
            pointer_x: None,
            strip_left: 0.0,
        });
        true
    }

    /// Follows the pointer of the drag of `tab_id` and moves the tab into the slot under it.
    pub(super) fn move_drag(
        &mut self,
        tab_id: u64,
        pointer_x: f32,
        strip_left: f32,
        tab_widths: &[f32],
    ) -> TabDragMove {
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.tab_id == tab_id) else {
            return TabDragMove::Unchanged;
        };
        let cursor_offset_x = drag.cursor_offset_x;
        drag.pointer_x = Some(pointer_x);
        drag.strip_left = strip_left;
        let Some(destination_index) =
            workspace_drag_destination(pointer_x, strip_left, cursor_offset_x, tab_widths)
        else {
            return TabDragMove::Unchanged;
        };
        if self.reorder(tab_id, destination_index) {
            TabDragMove::Reordered
        } else {
            TabDragMove::Moved
        }
    }

    /// Ends the drag in progress, reporting whether there was one.
    pub(super) const fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    /// Tab handles are keyed by tab, so a focused tab stays focused as tabs move.
    fn refresh_focus_order(&mut self) {
        for tab in &mut self.tabs {
            tab.focus = tab.focus.clone().tab_index(0).tab_stop(true);
        }
    }
}

impl Index<usize> for TabStrip {
    type Output = WorkspaceTab;

    fn index(&self, index: usize) -> &WorkspaceTab {
        &self.tabs[index]
    }
}

impl IndexMut<usize> for TabStrip {
    fn index_mut(&mut self, index: usize) -> &mut WorkspaceTab {
        &mut self.tabs[index]
    }
}

fn workspace_switch(active: usize, next: usize, workspace_count: usize) -> Option<(usize, usize)> {
    (next != active && next < workspace_count).then_some((active, next))
}

fn wrapped_workspace_index(
    current: usize,
    workspace_count: usize,
    direction: isize,
) -> Option<usize> {
    if workspace_count == 0 || current >= workspace_count {
        return None;
    }
    match direction.cmp(&0) {
        std::cmp::Ordering::Less => Some(if current == 0 {
            workspace_count - 1
        } else {
            current - 1
        }),
        std::cmp::Ordering::Equal => Some(current),
        std::cmp::Ordering::Greater => Some((current + 1) % workspace_count),
    }
}

fn active_workspace_after_close(ids: &[u64], active_id: u64, closing_id: u64) -> Option<u64> {
    if ids.len() <= 1 || !ids.contains(&closing_id) {
        return None;
    }
    if active_id != closing_id {
        return Some(active_id);
    }
    let closing = ids.iter().position(|id| *id == closing_id)?;
    ids.get(closing + 1)
        .or_else(|| closing.checked_sub(1).and_then(|index| ids.get(index)))
        .copied()
}

fn workspace_drag_destination(
    pointer_x: f32,
    strip_left: f32,
    cursor_offset_x: f32,
    tab_widths: &[f32],
) -> Option<usize> {
    let last = tab_widths.len().checked_sub(1)?;
    let dragged_left = pointer_x - cursor_offset_x - strip_left - WORKSPACE_TAB_STRIP_PADDING_LEFT;
    if !dragged_left.is_finite() {
        return None;
    }
    let mut destination = 0;
    let mut boundary = f32::midpoint(tab_widths[0], WORKSPACE_TAB_GAP);
    while destination < last && dragged_left >= boundary {
        destination += 1;
        boundary +=
            f32::midpoint(tab_widths[destination - 1], tab_widths[destination]) + WORKSPACE_TAB_GAP;
    }
    Some(destination)
}

#[cfg(test)]
mod tests {
    use gpui::{App, TestAppContext};

    use super::super::{AerisChartWorkspace, CHART_PANE_CAPACITY};
    use super::*;

    fn tab(id: u64, cx: &mut App) -> WorkspaceTab {
        WorkspaceTab {
            id,
            label: format!("Layout {id}"),
            saved_layout_id: id,
            autosaved_snapshot: None,
            panes: Vec::new(),
            active_pane: 0,
            layout: AerisChartWorkspace::new(id, CHART_PANE_CAPACITY).expect("tab layout root"),
            maximized_pane: None,
            swallow_pane_mouse_up: false,
            generation: 1,
            focus: cx.focus_handle(),
        }
    }

    fn strip(ids: &[u64], active_id: Option<u64>, cx: &mut TestAppContext) -> TabStrip {
        cx.update(|cx| {
            let tabs = ids.iter().map(|id| tab(*id, cx)).collect();
            TabStrip::new(tabs, active_id)
        })
    }

    fn ids(strip: &TabStrip) -> Vec<u64> {
        strip.iter().map(|tab| tab.id).collect()
    }

    #[test]
    fn workspace_tabs_switch_one_surface_and_preserve_stable_labels() {
        assert_eq!(workspace_switch(0, 1, 2), Some((0, 1)));
        assert_eq!(workspace_switch(1, 1, 2), None);
        assert_eq!(workspace_switch(0, 2, 2), None);
        assert_eq!(wrapped_workspace_index(0, 3, -1), Some(2));
        assert_eq!(wrapped_workspace_index(2, 3, 1), Some(0));
        assert_eq!(wrapped_workspace_index(1, 3, -1), Some(0));
        assert_eq!(wrapped_workspace_index(1, 3, 1), Some(2));
        assert_eq!(wrapped_workspace_index(0, 0, 1), None);
    }

    #[test]
    fn closing_a_tab_selects_its_right_then_left_neighbor() {
        let ids = [1, 2, 3];
        assert_eq!(active_workspace_after_close(&ids, 2, 1), Some(2));
        assert_eq!(active_workspace_after_close(&ids, 2, 2), Some(3));
        assert_eq!(active_workspace_after_close(&ids, 3, 3), Some(2));
        assert_eq!(active_workspace_after_close(&ids, 3, 99), None);
        assert_eq!(active_workspace_after_close(&[1], 1, 1), None);
    }

    #[test]
    fn workspace_drag_reflows_at_neighbor_slot_boundaries() {
        let strip_left = 100.0;
        let widths = [120.0, 176.0, 96.0];
        let cursor_offset = widths[0] / 2.0;
        let first_center = strip_left + WORKSPACE_TAB_STRIP_PADDING_LEFT + cursor_offset;
        let second_center = first_center + widths[0] + WORKSPACE_TAB_GAP;
        let third_center = second_center + widths[1] + WORKSPACE_TAB_GAP;

        assert_eq!(
            workspace_drag_destination(first_center, strip_left, cursor_offset, &widths),
            Some(0)
        );
        assert_eq!(
            workspace_drag_destination(second_center, strip_left, cursor_offset, &widths),
            Some(1)
        );
        assert_eq!(
            workspace_drag_destination(third_center, strip_left, cursor_offset, &widths),
            Some(2)
        );
        assert_eq!(
            workspace_drag_destination(strip_left - 500.0, strip_left, cursor_offset, &widths),
            Some(0)
        );
        assert_eq!(
            workspace_drag_destination(third_center + 500.0, strip_left, cursor_offset, &widths),
            Some(2)
        );
        assert_eq!(
            workspace_drag_destination(first_center, strip_left, cursor_offset, &[]),
            None
        );
        assert_eq!(
            workspace_drag_destination(f32::NAN, strip_left, cursor_offset, &widths),
            None
        );

        let drag = WorkspaceDragState {
            tab_id: 7,
            cursor_offset_x: cursor_offset,
            pointer_x: Some(second_center + 10.0),
            strip_left,
        };
        assert_eq!(drag.translation(7, 1, &widths), Some(10.0));
        assert_eq!(drag.translation(8, 1, &widths), None);
        assert_eq!(
            WorkspaceDragState {
                pointer_x: Some(strip_left - 500.0),
                ..drag
            }
            .translation(7, 1, &widths),
            Some(-(widths[0] + WORKSPACE_TAB_GAP))
        );
        assert_eq!(
            WorkspaceDragState {
                pointer_x: Some(third_center + 500.0),
                ..drag
            }
            .translation(7, 1, &widths),
            Some(widths[2] + WORKSPACE_TAB_GAP)
        );
        assert_eq!(
            WorkspaceDragState {
                pointer_x: None,
                ..drag
            }
            .translation(7, 1, &widths),
            None
        );
    }

    #[gpui::test]
    fn a_restored_strip_selects_the_saved_tab_or_the_first(cx: &mut TestAppContext) {
        assert_eq!(strip(&[1, 2, 3], Some(2), cx).active_index(), 1);
        assert_eq!(strip(&[1, 2, 3], Some(99), cx).active_index(), 0);
        assert_eq!(strip(&[1, 2, 3], None, cx).active_index(), 0);
    }

    #[gpui::test]
    fn selecting_reports_only_a_changed_selection(cx: &mut TestAppContext) {
        let mut tabs = strip(&[1, 2, 3], Some(1), cx);
        assert_eq!(tabs.select(2), Some((0, 2)));
        assert_eq!(tabs.active().id, 3);
        assert_eq!(tabs.select(2), None);
        assert_eq!(tabs.select(3), None);
        assert_eq!(tabs.active_index(), 2);
        assert_eq!(tabs.wrapped_index(tabs.active_index(), 1), Some(0));
        assert_eq!(tabs.wrapped_index(0, -1), Some(2));
    }

    #[gpui::test]
    fn reordering_keeps_the_selected_tab_selected(cx: &mut TestAppContext) {
        let mut tabs = strip(&[1, 2, 3], Some(2), cx);
        assert!(tabs.reorder(1, 2));
        assert_eq!(ids(&tabs), [2, 3, 1]);
        assert_eq!(tabs.active().id, 2);
        assert!(tabs.reorder(1, 0));
        assert_eq!(ids(&tabs), [1, 2, 3]);
        assert!(tabs.reorder(1, 1));
        assert_eq!(ids(&tabs), [2, 1, 3]);
        assert!(tabs.reorder(1, 0));
        assert!(tabs.reorder(3, 0));
        assert_eq!(ids(&tabs), [3, 1, 2]);
        assert!(
            tabs.reorder(3, 3),
            "a slot past the end clamps to the last slot"
        );
        assert_eq!(ids(&tabs), [1, 2, 3]);
        assert!(!tabs.reorder(3, 3));
        assert!(!tabs.reorder(2, 1));
        assert!(!tabs.reorder(99, 0));
        assert_eq!(tabs.active().id, 2);

        assert!(tabs.move_active(1));
        assert_eq!(ids(&tabs), [1, 3, 2]);
        assert_eq!(tabs.active_index(), 2);
        assert!(!tabs.move_active(1), "the last tab cannot move right");
        assert!(tabs.move_active(-2));
        assert_eq!(ids(&tabs), [2, 1, 3]);
        assert!(!tabs.move_active(-1), "the first tab cannot move left");
    }

    #[gpui::test]
    fn a_new_tab_is_appended_and_selected(cx: &mut TestAppContext) {
        let mut tabs = strip(&[1, 2], Some(1), cx);
        let added = cx.update(|cx| tab(5, cx));
        tabs.push_active(added);
        assert_eq!(ids(&tabs), [1, 2, 5]);
        assert_eq!(tabs.active().id, 5);
    }

    #[gpui::test]
    fn closing_a_tab_passes_the_selection_and_ends_its_drag(cx: &mut TestAppContext) {
        let mut tabs = strip(&[1, 2, 3], Some(2), cx);
        assert_eq!(tabs.close_plan(99), TabClose::NotOpen);
        assert_eq!(tabs.close_plan(1), TabClose::Remove { next_active_id: 2 });
        let TabClose::Remove { next_active_id } = tabs.close_plan(2) else {
            panic!("an open tab of several can close");
        };
        assert_eq!(next_active_id, 3);
        assert!(tabs.begin_drag(2, 10.0, 100.0));
        let removed = tabs.remove(2, next_active_id).expect("tab 2 is open");
        assert_eq!(removed.tab.id, 2);
        assert!(removed.was_active);
        assert!(removed.drag_cancelled);
        assert!(!tabs.is_dragging());
        assert_eq!(ids(&tabs), [1, 3]);
        assert_eq!(tabs.active().id, 3);

        assert!(tabs.begin_drag(1, 10.0, 100.0));
        let removed = tabs.remove(3, 1).expect("tab 3 is open");
        assert!(removed.was_active);
        assert!(!removed.drag_cancelled, "another tab's drag continues");
        assert!(tabs.is_dragging());
        assert_eq!(tabs.close_plan(1), TabClose::LastTab);
        assert!(
            tabs.remove(1, 1).is_none(),
            "the last tab closes with its window"
        );
    }

    #[gpui::test]
    fn a_tab_drag_follows_the_pointer_and_reorders_by_slot(cx: &mut TestAppContext) {
        let mut tabs = strip(&[1, 2, 3], Some(1), cx);
        let widths = [100.0, 100.0, 100.0];
        assert!(!tabs.begin_drag(99, 10.0, 100.0));
        assert_eq!(
            tabs.move_drag(1, 50.0, 0.0, &widths),
            TabDragMove::Unchanged,
            "no drag is in progress"
        );
        assert!(tabs.begin_drag(1, f32::NAN, 100.0));
        assert_eq!(
            tabs.move_drag(2, 50.0, 0.0, &widths),
            TabDragMove::Unchanged,
            "another tab's pointer is ignored"
        );
        assert_eq!(
            tabs.move_drag(1, f32::NAN, 0.0, &widths),
            TabDragMove::Unchanged
        );
        assert_eq!(
            tabs.move_drag(1, 60.0, 0.0, &widths),
            TabDragMove::Moved,
            "the grab point defaults to the tab's middle"
        );
        assert_eq!(
            tabs.drag().and_then(|drag| drag.translation(1, 0, &widths)),
            Some(2.0)
        );
        assert_eq!(
            tabs.move_drag(1, 300.0, 0.0, &widths),
            TabDragMove::Reordered
        );
        assert_eq!(ids(&tabs), [2, 3, 1]);
        assert_eq!(tabs.active().id, 1);
        assert!(tabs.end_drag());
        assert!(!tabs.end_drag());
    }
}
