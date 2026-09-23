//! `Asceify`'s stable-pane adapter around Nucleus's authoritative split workspace.

use nucleuscharts_engine::{SplitDirection, Workspace, WorkspaceError, WorkspaceLayout};
use num_traits::ToPrimitive;
use std::collections::{BTreeMap, BTreeSet};

/// Host-facing split direction without leaking Nucleus types into the desktop crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartSplitDirection {
    Horizontal,
    Vertical,
}

/// Stable-pane projection of Nucleus's native split tree.
#[derive(Clone, Debug, PartialEq)]
pub enum ChartWorkspaceLayout {
    Pane {
        pane_id: u64,
    },
    Split {
        direction: ChartSplitDirection,
        ratio: f64,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl ChartWorkspaceLayout {
    /// Returns pane identities in Nucleus's visual traversal order.
    #[must_use]
    pub fn pane_ids(&self) -> Vec<u64> {
        let mut pane_ids = Vec::new();
        self.collect_pane_ids(&mut pane_ids);
        pane_ids
    }

    fn collect_pane_ids(&self, pane_ids: &mut Vec<u64>) {
        match self {
            Self::Pane { pane_id } => pane_ids.push(*pane_id),
            Self::Split { first, second, .. } => {
                first.collect_pane_ids(pane_ids);
                second.collect_pane_ids(pane_ids);
            }
        }
    }

    /// Returns the ratio owned by the split at one adjacent pane boundary.
    #[must_use]
    pub fn boundary_ratio_for_panes(&self, left_pane_id: u64, right_pane_id: u64) -> Option<f64> {
        match self {
            Self::Pane { .. } => None,
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                let first_ids = first.pane_ids();
                let second_ids = second.pane_ids();
                if first_ids.last() == Some(&left_pane_id)
                    && second_ids.first() == Some(&right_pane_id)
                {
                    Some(*ratio)
                } else {
                    first
                        .boundary_ratio_for_panes(left_pane_id, right_pane_id)
                        .or_else(|| second.boundary_ratio_for_panes(left_pane_id, right_pane_id))
                }
            }
        }
    }

    /// Produces normalized legacy pane weights for persisted schema migration.
    #[must_use]
    pub fn pane_basis_points(&self) -> Vec<(u64, u32)> {
        let mut weighted = Vec::new();
        self.collect_weights(1.0, &mut weighted);
        let mut remaining = 10_000_u32;
        let last = weighted.len().saturating_sub(1);
        weighted
            .into_iter()
            .enumerate()
            .map(|(index, (pane_id, weight))| {
                let basis = if index == last {
                    remaining
                } else {
                    let remaining_panes = u32::try_from(last.saturating_sub(index)).unwrap_or(0);
                    let rounded = (weight * 10_000.0)
                        .round()
                        .clamp(1.0, 10_000.0)
                        .to_u32()
                        .unwrap_or(1);
                    let basis = rounded.min(remaining.saturating_sub(remaining_panes));
                    remaining = remaining.saturating_sub(basis);
                    basis
                };
                (pane_id, basis)
            })
            .collect()
    }

    fn collect_weights(&self, weight: f64, weighted: &mut Vec<(u64, f64)>) {
        match self {
            Self::Pane { pane_id } => weighted.push((*pane_id, weight)),
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                first.collect_weights(weight * *ratio, weighted);
                second.collect_weights(weight * (1.0 - *ratio), weighted);
            }
        }
    }
}

/// Nucleus-owned workspace model with stable `Asceify` pane identity mapping.
pub struct NucleusWorkspace {
    workspace: Workspace,
    pane_by_cell: BTreeMap<u64, u64>,
    maximum_panes: usize,
}

impl NucleusWorkspace {
    /// Creates one native Nucleus workspace rooted at `pane_id`.
    #[must_use]
    pub fn new(pane_id: u64, maximum_panes: usize) -> Self {
        Self {
            workspace: Workspace::new(),
            pane_by_cell: BTreeMap::from([(1, pane_id)]),
            maximum_panes,
        }
    }

    /// Replays a persisted stable-pane tree through Nucleus's native split API.
    ///
    /// # Errors
    /// Returns an error for invalid identities, ratios, capacity, or native replay failure.
    pub fn restore(
        layout: &ChartWorkspaceLayout,
        maximum_panes: usize,
    ) -> Result<Self, &'static str> {
        let pane_ids = layout.pane_ids();
        if pane_ids.is_empty()
            || pane_ids.len() > maximum_panes
            || pane_ids.contains(&0)
            || pane_ids.iter().copied().collect::<BTreeSet<_>>().len() != pane_ids.len()
        {
            return Err("Nucleus workspace layout has invalid pane identities");
        }
        let mut restored = Self {
            workspace: Workspace::new(),
            pane_by_cell: BTreeMap::new(),
            maximum_panes,
        };
        restored.replay_layout(1, layout)?;
        Ok(restored)
    }

    fn replay_layout(
        &mut self,
        cell_id: u64,
        layout: &ChartWorkspaceLayout,
    ) -> Result<(u64, u64), &'static str> {
        match layout {
            ChartWorkspaceLayout::Pane { pane_id } => {
                self.pane_by_cell.insert(cell_id, *pane_id);
                Ok((cell_id, cell_id))
            }
            ChartWorkspaceLayout::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                if !ratio.is_finite() || !(0.05..=0.95).contains(ratio) {
                    return Err("Nucleus workspace split ratio is invalid");
                }
                let second_cell = self
                    .workspace
                    .split(cell_id, nucleus_direction(*direction))
                    .map_err(workspace_error)?;
                let first_edge = self.replay_layout(cell_id, first)?;
                let second_edge = self.replay_layout(second_cell, second)?;
                self.workspace
                    .resize_between(first_edge.1, second_edge.0, *ratio - 0.5)
                    .map_err(workspace_error)?;
                Ok((first_edge.0, second_edge.1))
            }
        }
    }

    /// Splits a stable pane through Nucleus and maps the new native cell to `new_pane_id`.
    ///
    /// # Errors
    /// Returns an error for duplicate identities, capacity, or an unknown source pane.
    pub fn split(
        &mut self,
        pane_id: u64,
        direction: ChartSplitDirection,
        new_pane_id: u64,
    ) -> Result<(), &'static str> {
        if new_pane_id == 0
            || self
                .pane_by_cell
                .values()
                .any(|current| *current == new_pane_id)
        {
            return Err("Nucleus workspace received a duplicate pane identity");
        }
        if self.pane_by_cell.len() >= self.maximum_panes {
            return Err("Nucleus workspace is at pane capacity");
        }
        let cell_id = self.cell_for_pane(pane_id)?;
        let new_cell = self
            .workspace
            .split(cell_id, nucleus_direction(direction))
            .map_err(workspace_error)?;
        self.pane_by_cell.insert(new_cell, new_pane_id);
        Ok(())
    }

    /// Removes a stable pane and lets Nucleus collapse its split parent.
    ///
    /// # Errors
    /// Returns an error when the pane is unknown or is the workspace's final pane.
    pub fn remove(&mut self, pane_id: u64) -> Result<(), &'static str> {
        let cell_id = self.cell_for_pane(pane_id)?;
        self.workspace.remove(cell_id).map_err(workspace_error)?;
        self.pane_by_cell.remove(&cell_id);
        Ok(())
    }

    /// Moves one native divider to an absolute normalized ratio.
    ///
    /// # Errors
    /// Returns an error for an invalid ratio or a boundary not owned by the native tree.
    pub fn resize_between(
        &mut self,
        left_pane_id: u64,
        right_pane_id: u64,
        ratio: f64,
    ) -> Result<(), &'static str> {
        if !ratio.is_finite() {
            return Err("Nucleus workspace split ratio is invalid");
        }
        let layout = self.layout();
        let current = layout
            .boundary_ratio_for_panes(left_pane_id, right_pane_id)
            .ok_or("Nucleus workspace divider was not found")?;
        let left_cell = self.cell_for_pane(left_pane_id)?;
        let right_cell = self.cell_for_pane(right_pane_id)?;
        self.workspace
            .resize_between(left_cell, right_cell, ratio.clamp(0.05, 0.95) - current)
            .map_err(workspace_error)
    }

    /// Returns the stable-pane projection of Nucleus's current native layout.
    #[must_use]
    pub fn layout(&self) -> ChartWorkspaceLayout {
        project_layout(&self.workspace.layout(), &self.pane_by_cell)
    }

    fn cell_for_pane(&self, pane_id: u64) -> Result<u64, &'static str> {
        self.pane_by_cell
            .iter()
            .find_map(|(cell_id, current)| (*current == pane_id).then_some(*cell_id))
            .ok_or("Nucleus workspace pane was not found")
    }
}

fn nucleus_direction(direction: ChartSplitDirection) -> SplitDirection {
    match direction {
        ChartSplitDirection::Horizontal => SplitDirection::Horizontal,
        ChartSplitDirection::Vertical => SplitDirection::Vertical,
    }
}

fn workspace_error(error: WorkspaceError) -> &'static str {
    match error {
        WorkspaceError::NotFound => "Nucleus workspace pane was not found",
        WorkspaceError::LastCell => "Nucleus workspace cannot remove its final pane",
        WorkspaceError::InvalidLayout => "Nucleus workspace layout is invalid",
    }
}

fn project_layout(
    layout: &WorkspaceLayout,
    pane_by_cell: &BTreeMap<u64, u64>,
) -> ChartWorkspaceLayout {
    match layout {
        WorkspaceLayout::Cell { id } => ChartWorkspaceLayout::Pane {
            pane_id: pane_by_cell.get(id).copied().unwrap_or(0),
        },
        WorkspaceLayout::Split {
            direction,
            ratio,
            a,
            b,
        } => ChartWorkspaceLayout::Split {
            direction: match direction {
                SplitDirection::Horizontal => ChartSplitDirection::Horizontal,
                SplitDirection::Vertical => ChartSplitDirection::Vertical,
            },
            ratio: *ratio,
            first: Box::new(project_layout(a, pane_by_cell)),
            second: Box::new(project_layout(b, pane_by_cell)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_workspace_preserves_stable_panes_and_nested_direction() {
        let mut workspace = NucleusWorkspace::new(41, 4);
        workspace
            .split(41, ChartSplitDirection::Horizontal, 42)
            .unwrap();
        workspace
            .split(42, ChartSplitDirection::Vertical, 43)
            .unwrap();
        assert_eq!(workspace.layout().pane_ids(), [41, 42, 43]);
        assert!(matches!(
            workspace.layout(),
            ChartWorkspaceLayout::Split {
                direction: ChartSplitDirection::Horizontal,
                second,
                ..
            } if matches!(*second, ChartWorkspaceLayout::Split {
                direction: ChartSplitDirection::Vertical,
                ..
            })
        ));
    }

    #[test]
    fn persisted_tree_restores_through_native_nucleus_operations() {
        let layout = ChartWorkspaceLayout::Split {
            direction: ChartSplitDirection::Horizontal,
            ratio: 0.4,
            first: Box::new(ChartWorkspaceLayout::Pane { pane_id: 7 }),
            second: Box::new(ChartWorkspaceLayout::Split {
                direction: ChartSplitDirection::Vertical,
                ratio: 0.6,
                first: Box::new(ChartWorkspaceLayout::Pane { pane_id: 9 }),
                second: Box::new(ChartWorkspaceLayout::Pane { pane_id: 11 }),
            }),
        };
        let restored = NucleusWorkspace::restore(&layout, 4).unwrap();
        assert_eq!(restored.layout(), layout);
        assert_eq!(
            restored.layout().pane_basis_points(),
            [(7, 4_000), (9, 3_600), (11, 2_400)]
        );
    }

    #[test]
    fn remove_and_resize_delegate_to_nucleus_tree() {
        let mut workspace = NucleusWorkspace::new(1, 4);
        workspace
            .split(1, ChartSplitDirection::Horizontal, 2)
            .unwrap();
        workspace.resize_between(1, 2, 0.7).unwrap();
        assert!(matches!(
            workspace.layout(),
            ChartWorkspaceLayout::Split { ratio, .. } if (ratio - 0.7).abs() < 1e-9
        ));
        workspace.remove(1).unwrap();
        assert_eq!(
            workspace.layout(),
            ChartWorkspaceLayout::Pane { pane_id: 2 }
        );
    }

    #[test]
    fn host_preserves_the_product_pane_capacity() {
        let mut workspace = NucleusWorkspace::new(1, 2);
        workspace
            .split(1, ChartSplitDirection::Horizontal, 2)
            .unwrap();
        assert_eq!(
            workspace.split(2, ChartSplitDirection::Vertical, 3),
            Err("Nucleus workspace is at pane capacity")
        );
        assert_eq!(workspace.layout().pane_ids(), [1, 2]);
    }
}
