use axiusflow_engine_protocol::{
    WorkspaceLayoutState, WorkspacePaneState, WorkspaceSplitAxis, WorkspaceTabState,
};
use std::collections::BTreeSet;

const MINIMUM_RATIO_BASIS_POINTS: u32 = 500;
const MAXIMUM_RATIO_BASIS_POINTS: u32 = 9_500;

pub(super) fn layout_matches_panes(tab: &WorkspaceTabState) -> bool {
    let Some(layout) = tab.layout.as_ref() else {
        return false;
    };
    let expected = tab
        .panes
        .iter()
        .map(|pane| pane.pane_id)
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    valid_node(layout, &mut actual) && actual == expected
}

fn valid_node(layout: &WorkspaceLayoutState, pane_ids: &mut BTreeSet<u64>) -> bool {
    match (&layout.first, &layout.second) {
        (None, None) => layout.pane_id != 0 && pane_ids.insert(layout.pane_id),
        (Some(first), Some(second)) => {
            layout.pane_id == 0
                && WorkspaceSplitAxis::try_from(layout.split_axis).is_ok()
                && (MINIMUM_RATIO_BASIS_POINTS..=MAXIMUM_RATIO_BASIS_POINTS)
                    .contains(&layout.ratio_basis_points)
                && valid_node(first, pane_ids)
                && valid_node(second, pane_ids)
        }
        _ => false,
    }
}

pub(super) fn add_native_layouts(tabs: &mut [WorkspaceTabState]) {
    for tab in tabs {
        tab.layout = flat_layout(&tab.panes, tab.split_axis);
    }
}

fn flat_layout(panes: &[WorkspacePaneState], split_axis: i32) -> Option<WorkspaceLayoutState> {
    let (first, remaining) = panes.split_first()?;
    if remaining.is_empty() {
        return Some(leaf(first.pane_id));
    }
    let total = panes
        .iter()
        .map(|pane| u64::from(pane.size_basis_points))
        .sum::<u64>();
    let ratio = u64::from(first.size_basis_points)
        .checked_mul(10_000)
        .and_then(|scaled| scaled.checked_div(total))
        .and_then(|scaled| u32::try_from(scaled).ok())
        .unwrap_or(5_000)
        .clamp(MINIMUM_RATIO_BASIS_POINTS, MAXIMUM_RATIO_BASIS_POINTS);
    Some(WorkspaceLayoutState {
        pane_id: 0,
        split_axis,
        ratio_basis_points: ratio,
        first: Some(Box::new(leaf(first.pane_id))),
        second: flat_layout(remaining, split_axis).map(Box::new),
    })
}

fn leaf(pane_id: u64) -> WorkspaceLayoutState {
    WorkspaceLayoutState {
        pane_id,
        split_axis: WorkspaceSplitAxis::Horizontal as i32,
        ratio_basis_points: 0,
        first: None,
        second: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_layout_migration_preserves_every_pane_once() {
        let panes = [pane(1, 4_000), pane(2, 3_000), pane(3, 3_000)];
        let mut tab = WorkspaceTabState {
            panes: panes.to_vec(),
            ..WorkspaceTabState::default()
        };
        add_native_layouts(std::slice::from_mut(&mut tab));
        assert!(layout_matches_panes(&tab));
        assert_eq!(tab.layout.as_ref().unwrap().ratio_basis_points, 4_000);
    }

    fn pane(pane_id: u64, size_basis_points: u32) -> WorkspacePaneState {
        WorkspacePaneState {
            pane_id,
            size_basis_points,
            ..WorkspacePaneState::default()
        }
    }
}
