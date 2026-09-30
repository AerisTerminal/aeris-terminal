//! Reusable dock that lays out the workspace side panels beside the chart.
//!
//! Each [`SidePanel`] declares its own [`SidePanelSizing`]; the dock owns docking order,
//! widths, the resizable split between flexible panels and the shared panel header. The
//! panels themselves live in their own component modules.

use super::order_book_panel::{OrderBookPanelState, order_book_side_panel};
use super::time_sales_panel::{TIME_SALES_PANEL_WIDTH, TimeSalesPanelState, time_sales_panel};
use super::watchlist_panel::{WatchlistPanelState, watchlist_side_panel};
use super::*;
use gpui::AppContext;

pub(super) const SIDE_PANEL_HEADER_HEIGHT: f32 = 30.0;
const SIDE_PANEL_SPLIT_DIVIDER_WIDTH: f32 = 1.0;
const SIDE_PANEL_SPLIT_HANDLE_WIDTH: f32 = 8.0;

/// How one docked panel takes horizontal space in the dock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum SidePanelSizing {
    /// Takes the resizable per-panel width; two flexible panels share it by the split ratio.
    Flexible,
    /// Keeps a content-defined width, including its leading border.
    Fixed(f32),
}

impl SidePanel {
    pub(super) const fn sizing(self) -> SidePanelSizing {
        match self {
            Self::OrderBook | Self::Watchlist => SidePanelSizing::Flexible,
            Self::TimeSales => SidePanelSizing::Fixed(TIME_SALES_PANEL_WIDTH),
        }
    }
}

pub(super) struct WorkspaceSidePanelState<'a> {
    pub(super) app: Entity<WorkspaceSurface>,
    pub(super) terminal: Entity<TerminalApp>,
    pub(super) workspace_id: u64,
    pub(super) visible: SidePanelVisibility,
    pub(super) width: f32,
    pub(super) split_basis_points: u32,
    pub(super) order_book: OrderBookPanelState<'a>,
    pub(super) time_sales: TimeSalesPanelState<'a>,
    pub(super) watchlist: WatchlistPanelState,
    pub(super) theme: &'a AerisTheme,
}

/// Horizontal geometry of the visible docked panels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DockGeometry {
    total_width: f32,
    flexible_count: u8,
    fixed_width: f32,
    /// Fixed panels docked left of the split between two flexible panels.
    fixed_before_split: f32,
}

impl DockGeometry {
    fn new(visible: SidePanelVisibility, width: f32) -> Self {
        let mut flexible_count = 0_u8;
        let mut fixed_width = 0.0;
        let mut fixed_before_split = 0.0;
        for panel in SidePanel::ALL
            .into_iter()
            .filter(|panel| visible.contains(*panel))
        {
            match panel.sizing() {
                SidePanelSizing::Flexible => flexible_count += 1,
                SidePanelSizing::Fixed(panel_width) => {
                    fixed_width += panel_width;
                    if flexible_count < 2 {
                        fixed_before_split += panel_width;
                    }
                }
            }
        }
        let geometry = Self {
            total_width: 0.0,
            flexible_count,
            fixed_width,
            fixed_before_split,
        };
        Self {
            total_width: width * f32::from(flexible_count) + geometry.divider() + fixed_width,
            ..geometry
        }
    }

    const fn split(&self) -> bool {
        self.flexible_count == 2
    }

    const fn divider(&self) -> f32 {
        if self.split() {
            SIDE_PANEL_SPLIT_DIVIDER_WIDTH
        } else {
            0.0
        }
    }

    /// Per-panel width for a left-edge drag, or `None` when no panel is resizable.
    fn width_from_drag(&self, right: f32, pointer_x: f32) -> Option<f32> {
        (self.flexible_count > 0).then(|| {
            clamped_side_panel_width(
                (right - pointer_x - self.divider() - self.fixed_width)
                    / f32::from(self.flexible_count),
            )
        })
    }

    /// Share of the flexible width left of the split for a divider drag.
    fn split_ratio_from_drag(&self, left: f32, pointer_x: f32) -> Option<f32> {
        let content_width = self.total_width - self.divider() - self.fixed_width;
        (self.split() && content_width > 0.0).then(|| {
            (pointer_x - left - self.fixed_before_split - SIDE_PANEL_SPLIT_DIVIDER_WIDTH / 2.0)
                / content_width
        })
    }
}

fn side_panel_horizontal_ratio(width: f32, split_basis_points: u32) -> f32 {
    let requested = split_basis_points.to_f32().unwrap_or(5_000.0) / 10_000.0;
    let total_width = width * 2.0;
    let lower = (SIDE_PANEL_MINIMUM_WIDTH / total_width)
        .max(1.0 - SIDE_PANEL_MAXIMUM_WIDTH / total_width)
        .clamp(0.05, 0.5);
    let upper = (SIDE_PANEL_MAXIMUM_WIDTH / total_width)
        .min(1.0 - SIDE_PANEL_MINIMUM_WIDTH / total_width)
        .clamp(0.5, 0.95);
    requested.clamp(lower, upper)
}

/// Placement of one visible panel in the dock.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DockSlot {
    panel: SidePanel,
    /// The resizable split divider precedes this panel.
    split_before: bool,
    /// A 1 px border separates this panel from the one before it.
    leading_border: bool,
    sizing: DockSlotSizing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DockSlotSizing {
    Grow(f32),
    Fixed(f32),
}

/// Visible panels in docking order. `split_ratio` is the left flexible panel's share
/// when two flexible panels are docked.
fn dock_slots(visible: SidePanelVisibility, split_ratio: Option<f32>) -> Vec<DockSlot> {
    let mut flexible_seen = 0_u8;
    SidePanel::ALL
        .into_iter()
        .filter(|panel| visible.contains(*panel))
        .enumerate()
        .map(|(index, panel)| {
            let (split_before, sizing) = match panel.sizing() {
                SidePanelSizing::Flexible => {
                    let grow = match (split_ratio, flexible_seen) {
                        (Some(ratio), 0) => ratio,
                        (Some(ratio), _) => 1.0 - ratio,
                        (None, _) => 1.0,
                    };
                    let split_before = split_ratio.is_some() && flexible_seen == 1;
                    flexible_seen += 1;
                    (split_before, DockSlotSizing::Grow(grow))
                }
                SidePanelSizing::Fixed(width) => (false, DockSlotSizing::Fixed(width)),
            };
            DockSlot {
                panel,
                split_before,
                leading_border: index > 0 && !split_before,
                sizing,
            }
        })
        .collect()
}

/// Per-panel render inputs; each panel's content is built once by its own component.
struct DockContents<'a> {
    app: Entity<WorkspaceSurface>,
    terminal: Entity<TerminalApp>,
    order_book: OrderBookPanelState<'a>,
    time_sales: Option<TimeSalesPanelState<'a>>,
    watchlist: Option<WatchlistPanelState>,
    theme: &'a AerisTheme,
}

impl DockContents<'_> {
    fn render(&mut self, panel: SidePanel) -> AnyElement {
        let content = match panel {
            SidePanel::OrderBook => {
                Some(order_book_side_panel(&self.order_book).into_any_element())
            }
            SidePanel::TimeSales => self
                .time_sales
                .take()
                .map(|state| time_sales_panel(state, self.theme).into_any_element()),
            SidePanel::Watchlist => self.watchlist.take().map(|state| {
                watchlist_side_panel(self.app.clone(), &self.terminal, state, self.theme)
                    .into_any_element()
            }),
        };
        content.unwrap_or_else(|| div().into_any_element())
    }
}

fn dock_region(slot: DockSlot, border: gpui::Hsla) -> Div {
    let region = match slot.sizing {
        DockSlotSizing::Grow(grow) => div().min_w_0().flex_basis(px(0.0)).flex_grow(grow),
        DockSlotSizing::Fixed(width) => div().w(px(width)).flex_none(),
    };
    region
        .h_full()
        .overflow_hidden()
        .when(slot.leading_border, |region| {
            region.border_l_1().border_color(border)
        })
}

pub(super) fn workspace_side_panel(state: WorkspaceSidePanelState<'_>) -> impl IntoElement + use<> {
    let WorkspaceSidePanelState {
        app,
        terminal,
        workspace_id,
        visible,
        width,
        split_basis_points,
        order_book,
        time_sales,
        watchlist,
        theme,
    } = state;
    let geometry = DockGeometry::new(visible, width);
    let split_ratio = geometry
        .split()
        .then(|| side_panel_horizontal_ratio(width, split_basis_points));
    let border = gpui_color(theme.colors.border);
    let split_drag_app = app.clone();
    let width_drag_app = app.clone();
    let mut contents = DockContents {
        app,
        terminal,
        order_book,
        time_sales: Some(time_sales),
        watchlist: Some(watchlist),
        theme,
    };
    let mut dock = div()
        .id(("workspace_side_panel", workspace_id))
        .w(px(geometry.total_width))
        .h_full()
        .flex_none()
        .relative()
        .flex()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .border_l_1()
        .border_color(border);
    for slot in dock_slots(visible, split_ratio) {
        if slot.split_before {
            dock = dock.child(side_panel_split_handle(workspace_id, border));
        }
        dock = dock.child(dock_region(slot, border).child(contents.render(slot.panel)));
    }
    dock.on_drag_move::<SidePanelSplitDrag>(move |event, _, cx| {
        let Some(ratio) = geometry.split_ratio_from_drag(
            f32::from(event.bounds.left()),
            f32::from(event.event.position.x),
        ) else {
            return;
        };
        split_drag_app.update(cx, |surface, surface_cx| {
            surface.set_side_panel_split_ratio(ratio, surface_cx);
        });
    })
    .on_drag_move::<SidePanelWidthDrag>(move |event, _, cx| {
        let Some(width) = geometry.width_from_drag(
            f32::from(event.bounds.right()),
            f32::from(event.event.position.x),
        ) else {
            return;
        };
        width_drag_app.update(cx, |surface, surface_cx| {
            surface.set_side_panel_width(width, surface_cx);
        });
    })
    .children((geometry.flexible_count > 0).then(|| side_panel_width_resize_handle(workspace_id)))
}

/// Shared header of every docked panel: title, panel-specific actions, then close.
pub(super) fn side_panel_header(
    panel: SidePanel,
    app: Entity<WorkspaceSurface>,
    actions: impl IntoIterator<Item = AnyElement>,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let close_id = match panel {
        SidePanel::OrderBook => "close_order_book_panel",
        SidePanel::TimeSales => "close_time_sales_panel",
        SidePanel::Watchlist => "close_watchlist_panel",
    };
    div()
        .h(px(SIDE_PANEL_HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(div().flex_1().min_w_0().child(panel.title().to_uppercase()))
        .children(actions)
        .child(chrome_tooltip(
            close_id,
            "Close side panel",
            chrome_close_button(close_id, theme, move |_, cx| {
                app.update(cx, |surface, surface_cx| {
                    surface.close_side_panel(panel, surface_cx);
                });
            }),
            theme,
        ))
}

/// Round icon button for a panel header action, highlighted while its feature is open.
pub(super) fn side_panel_header_button<F: Fn(&mut gpui::App) + 'static>(
    id: &'static str,
    label: &'static str,
    icon: HugeIcon,
    active: bool,
    on_press: F,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let tooltip = TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY);
    round_icon_button(id, icon, label)
        .text_color(gpui_color(if active {
            colors.icon_active
        } else {
            colors.icon
        }))
        .cursor_pointer()
        .when(active, |button| {
            button.bg(gpui_color(colors.active_bg.over(colors.surface)))
        })
        .hover(move |button| button.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            on_press(cx);
            cx.stop_propagation();
        })
        .tooltip(tooltip.builder())
        .tooltip_show_delay(tooltip.delay())
        .into_any_element()
}

#[derive(Clone)]
struct SidePanelSplitDrag;

impl Render for SidePanelSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
struct SidePanelWidthDrag;

impl Render for SidePanelWidthDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

fn side_panel_split_handle(workspace_id: u64, border: gpui::Hsla) -> impl IntoElement {
    div()
        .id(("side_panel_split", workspace_id))
        .relative()
        .flex_none()
        .h_full()
        .w(px(SIDE_PANEL_SPLIT_DIVIDER_WIDTH))
        .bg(border)
        .child(
            div()
                .id(("side_panel_split_hit", workspace_id))
                .absolute()
                .top_0()
                .left(px(-(SIDE_PANEL_SPLIT_HANDLE_WIDTH
                    - SIDE_PANEL_SPLIT_DIVIDER_WIDTH)
                    / 2.0))
                .h_full()
                .w(px(SIDE_PANEL_SPLIT_HANDLE_WIDTH))
                .occlude()
                .cursor_col_resize()
                .on_drag(SidePanelSplitDrag, |drag, _, _, cx| {
                    cx.new(|_| drag.clone())
                }),
        )
}

fn side_panel_width_resize_handle(workspace_id: u64) -> impl IntoElement {
    div()
        .id(("side_panel_resize", workspace_id))
        .absolute()
        .occlude()
        .top_0()
        .left(px(-SIDE_PANEL_RESIZE_HANDLE_WIDTH / 2.0))
        .h_full()
        .w(px(SIDE_PANEL_RESIZE_HANDLE_WIDTH))
        .cursor_col_resize()
        .on_drag(SidePanelWidthDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(panels: &[SidePanel]) -> SidePanelVisibility {
        let mut visible = SidePanelVisibility::default();
        for panel in panels {
            visible.set(*panel, true);
        }
        visible
    }

    #[test]
    fn flexible_panels_take_the_panel_width_and_fixed_panels_their_own() {
        let book = DockGeometry::new(visible(&[SidePanel::OrderBook]), 400.0);
        assert!((book.total_width - 400.0).abs() < f32::EPSILON);

        let book_and_watchlist = DockGeometry::new(
            visible(&[SidePanel::OrderBook, SidePanel::Watchlist]),
            400.0,
        );
        assert!(
            (book_and_watchlist.total_width - (800.0 + SIDE_PANEL_SPLIT_DIVIDER_WIDTH)).abs()
                < f32::EPSILON
        );

        let tape_only = DockGeometry::new(visible(&[SidePanel::TimeSales]), 400.0);
        assert!((tape_only.total_width - TIME_SALES_PANEL_WIDTH).abs() < f32::EPSILON);
        assert_eq!(tape_only.width_from_drag(1_000.0, 500.0), None);

        let all = DockGeometry::new(visible(&SidePanel::ALL), 400.0);
        assert!(
            (all.total_width - (800.0 + SIDE_PANEL_SPLIT_DIVIDER_WIDTH + TIME_SALES_PANEL_WIDTH))
                .abs()
                < f32::EPSILON
        );
        assert!((all.fixed_before_split - TIME_SALES_PANEL_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn drags_exclude_fixed_panels_from_resizable_space() {
        let all = DockGeometry::new(visible(&SidePanel::ALL), 400.0);
        let right = 2_000.0;
        let left = right - all.total_width;
        let width = all
            .width_from_drag(right, left)
            .expect("flexible panels resize");
        assert!((width - 400.0).abs() < 0.01);

        // The split handle sits after the book (400 px) and the fixed Time & Sales column.
        let handle_x = left + 400.0 + TIME_SALES_PANEL_WIDTH + SIDE_PANEL_SPLIT_DIVIDER_WIDTH / 2.0;
        let ratio = all
            .split_ratio_from_drag(left, handle_x)
            .expect("two flexible panels split");
        assert!((ratio - 0.5).abs() < 0.01);

        let single = DockGeometry::new(
            visible(&[SidePanel::OrderBook, SidePanel::TimeSales]),
            400.0,
        );
        assert_eq!(single.split_ratio_from_drag(0.0, 100.0), None);
    }

    #[test]
    fn slots_dock_in_order_with_one_divider_between_flexible_panels() {
        let slots = dock_slots(visible(&SidePanel::ALL), Some(0.4));
        let panels = slots.iter().map(|slot| slot.panel).collect::<Vec<_>>();
        assert_eq!(panels, SidePanel::ALL);
        assert_eq!(slots[0].sizing, DockSlotSizing::Grow(0.4));
        assert!(!slots[0].leading_border && !slots[0].split_before);
        assert_eq!(
            slots[1].sizing,
            DockSlotSizing::Fixed(TIME_SALES_PANEL_WIDTH)
        );
        assert!(slots[1].leading_border);
        assert_eq!(slots[2].sizing, DockSlotSizing::Grow(0.6));
        assert!(slots[2].split_before && !slots[2].leading_border);

        let tape_and_watchlist =
            dock_slots(visible(&[SidePanel::TimeSales, SidePanel::Watchlist]), None);
        assert_eq!(tape_and_watchlist[1].sizing, DockSlotSizing::Grow(1.0));
        assert!(tape_and_watchlist[1].leading_border && !tape_and_watchlist[1].split_before);
    }

    #[test]
    fn horizontal_split_keeps_both_panels_renderable() {
        assert!((side_panel_horizontal_ratio(400.0, 5_000) - 0.5).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(400.0, 500) - 0.45).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(400.0, 9_500) - 0.55).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(480.0, 500) - 0.5).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(480.0, 9_500) - 0.5).abs() < f32::EPSILON);
    }
}
