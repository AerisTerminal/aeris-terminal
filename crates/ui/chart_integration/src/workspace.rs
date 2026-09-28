//! Product capacity policy around Aeris Charts' authoritative split workspace.

pub use aeris_charts_engine::{
    SplitDirection as ChartSplitDirection, WorkspaceLayout as ChartWorkspaceLayout,
};
use aeris_charts_engine::{Workspace, WorkspaceError};

/// Aeris Charts-owned workspace with Terminal's product pane-capacity policy.
pub struct AerisChartWorkspace {
    workspace: Workspace,
    maximum_panes: usize,
}

impl AerisChartWorkspace {
    /// Creates one native workspace rooted at a Terminal-issued stable pane identity.
    ///
    /// # Errors
    /// Returns an error when the stable identity is zero or exhausted.
    pub fn new(pane_id: u64, maximum_panes: usize) -> Result<Self, &'static str> {
        Ok(Self {
            workspace: Workspace::new_with_id(pane_id).map_err(workspace_error)?,
            maximum_panes,
        })
    }

    /// Restores a validated stable-pane tree as one native workspace transaction.
    ///
    /// # Errors
    /// Returns an error for an invalid layout or a layout above product capacity.
    pub fn restore(
        layout: &ChartWorkspaceLayout,
        maximum_panes: usize,
    ) -> Result<Self, &'static str> {
        if layout.leaf_ids().len() > maximum_panes {
            return Err("Aeris Charts workspace is at pane capacity");
        }
        Ok(Self {
            workspace: Workspace::from_layout(layout).map_err(workspace_error)?,
            maximum_panes,
        })
    }

    /// Splits a stable pane using the new Terminal-issued identity.
    ///
    /// # Errors
    /// Returns an error for duplicate identities, capacity, or an unknown source pane.
    pub fn split(
        &mut self,
        pane_id: u64,
        direction: ChartSplitDirection,
        new_pane_id: u64,
    ) -> Result<(), &'static str> {
        if self.workspace.chart_count() >= self.maximum_panes {
            return Err("Aeris Charts workspace is at pane capacity");
        }
        self.workspace
            .split_with_id(pane_id, direction, new_pane_id)
            .map_err(workspace_error)
    }

    /// Removes a stable pane and lets Aeris Charts collapse its split parent.
    ///
    /// # Errors
    /// Returns an error when the pane is unknown or is the workspace's final pane.
    pub fn remove(&mut self, pane_id: u64) -> Result<(), &'static str> {
        self.workspace.remove(pane_id).map_err(workspace_error)
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
        self.workspace
            .resize_between_to(left_pane_id, right_pane_id, ratio)
            .map_err(workspace_error)
    }

    /// Returns Aeris Charts' stable-pane layout directly.
    #[must_use]
    pub fn layout(&self) -> ChartWorkspaceLayout {
        self.workspace.layout()
    }
}

fn workspace_error(error: WorkspaceError) -> &'static str {
    match error {
        WorkspaceError::NotFound => "Aeris Charts workspace pane was not found",
        WorkspaceError::LastCell => "Aeris Charts workspace cannot remove its final pane",
        WorkspaceError::InvalidLayout => "Aeris Charts workspace layout is invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_workspace_preserves_stable_panes_and_nested_direction() {
        let mut workspace = AerisChartWorkspace::new(41, 4).unwrap();
        workspace
            .split(41, ChartSplitDirection::Horizontal, 42)
            .unwrap();
        workspace
            .split(42, ChartSplitDirection::Vertical, 43)
            .unwrap();
        assert_eq!(workspace.layout().leaf_ids(), [41, 42, 43]);
        assert!(matches!(
            workspace.layout(),
            ChartWorkspaceLayout::Split {
                direction: ChartSplitDirection::Horizontal,
                b,
                ..
            } if matches!(*b, ChartWorkspaceLayout::Split {
                direction: ChartSplitDirection::Vertical,
                ..
            })
        ));
    }

    #[test]
    fn persisted_tree_restores_as_one_native_transaction() {
        let layout = ChartWorkspaceLayout::Split {
            direction: ChartSplitDirection::Horizontal,
            ratio: 0.4,
            a: Box::new(ChartWorkspaceLayout::Cell { id: 7 }),
            b: Box::new(ChartWorkspaceLayout::Split {
                direction: ChartSplitDirection::Vertical,
                ratio: 0.6,
                a: Box::new(ChartWorkspaceLayout::Cell { id: 9 }),
                b: Box::new(ChartWorkspaceLayout::Cell { id: 11 }),
            }),
        };
        let restored = AerisChartWorkspace::restore(&layout, 4).unwrap();
        assert_eq!(restored.layout(), layout);
        assert_eq!(
            restored.layout().basis_points(),
            [(7, 4_000), (9, 3_600), (11, 2_400)]
        );
    }

    #[test]
    fn remove_and_resize_delegate_to_aeris_charts_tree() {
        let mut workspace = AerisChartWorkspace::new(1, 4).unwrap();
        workspace
            .split(1, ChartSplitDirection::Horizontal, 2)
            .unwrap();
        workspace.resize_between(1, 2, 0.7).unwrap();
        assert!(matches!(
            workspace.layout(),
            ChartWorkspaceLayout::Split { ratio, .. } if (ratio - 0.7).abs() < 1e-9
        ));
        workspace.remove(1).unwrap();
        assert_eq!(workspace.layout(), ChartWorkspaceLayout::Cell { id: 2 });
    }

    #[test]
    fn host_preserves_the_product_pane_capacity() {
        let mut workspace = AerisChartWorkspace::new(1, 2).unwrap();
        workspace
            .split(1, ChartSplitDirection::Horizontal, 2)
            .unwrap();
        assert_eq!(
            workspace.split(2, ChartSplitDirection::Vertical, 3),
            Err("Aeris Charts workspace is at pane capacity")
        );
        assert_eq!(workspace.layout().leaf_ids(), [1, 2]);
    }
}
