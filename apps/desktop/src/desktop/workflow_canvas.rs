//! The Workflows page's elements: the workflow list, the board with its blocks, links, pan
//! and zoom, the block palette and the inspector of the selection. Every control edits the
//! open draft through `TerminalApp`; nothing here talks to the trading owner.

use super::market_screener::AppView;
use super::workflow_board::{
    ActionDraft, BlockDraft, BlockKind, BoardItem, BoardPoint, ConditionDraft, DraftField,
    DraftProblem, NODE_HEIGHT, NODE_WIDTH, NodeDraft, OutputPort, RepeatDraft, TriggerKind,
};
use super::workflows_page::{
    BoardGesture, DraftStatus, OpenWorkflow, PageStatus, WorkflowsPage, account_environment,
    account_text, block_summary, draft_status, environment_text, firing_time_text, instrument,
    instrument_scales, outcome_text, problem_text, state_color, state_text, symbol_text,
    trigger_summary,
};
use super::*;
use aeris_localization::{tr, tr_count, workflows};
use aeris_trading::{AccountEnvironment, TradingAccountId};
use aeris_trading_runtime::{
    MAXIMUM_WORKFLOW_NODES, MAXIMUM_WORKFLOWS, PositionRequirement, PriceDirection, WorkflowNodeId,
    WorkflowState, WorkflowTimeZone, WorkflowView, WorkingSet,
};
use gpui::{
    ClickEvent, DispatchPhase, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Point,
    ScrollWheelEvent,
};

const SIDEBAR_WIDTH: f32 = 264.0;
const PALETTE_WIDTH: f32 = 184.0;
const INSPECTOR_WIDTH: f32 = 320.0;
const EMPTY_STATE_WIDTH: f32 = 440.0;
const OUTPUT_PORT_SIZE: f32 = 12.0;
const INPUT_PORT_SIZE: f32 = 8.0;
const PORT_LABEL_WIDTH: f32 = 36.0;
const PORT_LABEL_SIZE: f32 = 10.0;
const WIRE_WIDTH: f32 = 2.0;
const WIRE_MINIMUM_BEND: f32 = 48.0;
const BLOCK_ACCENT_WIDTH: f32 = 4.0;
const BLOCK_TITLE_SIZE: f32 = 11.0;
const BLOCK_TEXT_SIZE: f32 = 12.0;
const BLOCK_LINE_HEIGHT: f32 = 16.0;
const BLOCK_PADDING_X: f32 = 10.0;
const BLOCK_PADDING_Y: f32 = 8.0;
const BLOCK_GAP: f32 = 4.0;
const PALETTE_DOT_SIZE: f32 = 8.0;
const RECENT_FIRINGS_SHOWN: usize = 8;
#[cfg(target_os = "macos")]
const ZOOM_KEY: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const ZOOM_KEY: &str = "Ctrl";

/// What every part of the page reads while it renders.
#[derive(Clone, Copy)]
struct Page<'a> {
    terminal: &'a Entity<TerminalApp>,
    set: Option<&'a WorkingSet>,
    theme: &'a AerisTheme,
}

/// The page body under the title bar: the workflow list beside the open workflow.
pub(super) fn workflows_page(
    terminal: &Entity<TerminalApp>,
    page: &WorkflowsPage,
    set: Option<&WorkingSet>,
    theme: &AerisTheme,
) -> AnyElement {
    let ui = Page {
        terminal,
        set,
        theme,
    };
    let body = match page.open.as_ref() {
        Some(open) => editor(ui, page, open).into_any_element(),
        None => empty_state(ui).into_any_element(),
    };
    div()
        .id("workflows_page")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .bg(gpui_color(theme.colors.surface))
        .child(sidebar(ui, page))
        .child(body)
        .into_any_element()
}

/// A click handler that applies one edit to the open workflow.
fn edit(
    terminal: &Entity<TerminalApp>,
    change: impl Fn(&mut OpenWorkflow) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let terminal = terminal.clone();
    move |_, window, cx| {
        terminal.update(cx, |terminal, terminal_cx| {
            terminal.edit_open_workflow(window, terminal_cx, &change);
        });
    }
}

fn border_width(theme: &AerisTheme) -> Pixels {
    px(theme.dimensions.border_width)
}

fn radius(scale: f32) -> Pixels {
    px(f32::from(RadiusToken::Default.logical_pixels()) * scale)
}

fn secondary_text(text: impl Into<SharedString>, theme: &AerisTheme) -> Div {
    div()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_secondary))
        .child(text.into())
}

fn heading(text: impl Into<SharedString>, theme: &AerisTheme) -> Div {
    div()
        .text_sm()
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .text_color(gpui_color(theme.colors.text_default))
        .child(text.into())
}

fn field(label: &'static str, control: impl IntoElement, theme: &AerisTheme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(secondary_text(label, theme))
        .child(control)
}

/// One button per choice; the selected one is highlighted.
fn choices<T: Clone + PartialEq + 'static>(
    ui: Page<'_>,
    id: &'static str,
    options: Vec<(T, SharedString)>,
    selected: Option<&T>,
    apply: impl Fn(&mut OpenWorkflow, T) + 'static,
) -> Div {
    let apply = Rc::new(apply);
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(
            options
                .into_iter()
                .enumerate()
                .map(|(index, (choice, label))| {
                    let apply = Rc::clone(&apply);
                    let is_selected = selected == Some(&choice);
                    Button::new((id, index), ui.theme)
                        .variant(ButtonVariant::Outline)
                        .button_size(ButtonSize::Sm)
                        .selected(is_selected)
                        .label(label)
                        .on_click(edit(ui.terminal, move |open| apply(open, choice.clone())))
                }),
        )
}

fn input_field(
    ui: Page<'_>,
    open: &OpenWorkflow,
    draft_field: DraftField,
    label: &'static str,
) -> Option<Div> {
    let input = open.input(draft_field)?;
    Some(field(label, Input::new(input).platform(ui.theme), ui.theme))
}

fn kind_label(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::IfPosition => tr!(workflows::BLOCK_IF_POSITION),
        BlockKind::IfTime => tr!(workflows::BLOCK_IF_TIME),
        BlockKind::IfPrice => tr!(workflows::BLOCK_IF_PRICE),
        BlockKind::Buy => tr!(workflows::BLOCK_BUY),
        BlockKind::Sell => tr!(workflows::BLOCK_SELL),
        BlockKind::Flatten => tr!(workflows::BLOCK_FLATTEN),
        BlockKind::CancelOrders => tr!(workflows::BLOCK_CANCEL_ORDERS),
        BlockKind::LockAccount => tr!(workflows::BLOCK_LOCK_ACCOUNT),
    }
}

/// The accent of a block, or of the trigger for `None`.
fn kind_accent(kind: Option<BlockKind>, theme: &AerisTheme) -> Hsla {
    let colors = theme.colors;
    gpui_color(match kind {
        None => colors.primary,
        Some(BlockKind::IfPosition | BlockKind::IfTime | BlockKind::IfPrice) => colors.warning,
        Some(BlockKind::Buy) => colors.buy,
        Some(BlockKind::Sell) => colors.sell,
        Some(BlockKind::Flatten | BlockKind::CancelOrders) => colors.indigo,
        Some(BlockKind::LockAccount) => colors.danger,
    })
}

fn sidebar(ui: Page<'_>, page: &WorkflowsPage) -> Div {
    let theme = ui.theme;
    let workflows = ui.set.map_or(&[][..], |set| set.workflows.as_slice());
    let shown = page
        .open
        .as_ref()
        .and_then(|open| open.draft.saved.as_ref())
        .map(|saved| saved.workflow_id.as_str());
    let rows = workflows
        .iter()
        .enumerate()
        .map(|(index, view)| {
            list_row(
                ui,
                index,
                view,
                shown == Some(view.definition.workflow_id.as_str()),
            )
        })
        .collect::<Vec<_>>();
    div()
        .w(px(SIDEBAR_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .border_r(border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_4()
                .pt_4()
                .pb_3()
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(platform_font_weight(TypographyRole::Strong))
                                .child(AppView::Workflows.label()),
                        )
                        .child(secondary_text(
                            tr_count!(workflows::COUNT, workflows.len()),
                            theme,
                        )),
                )
                .child(new_workflow_button(
                    ui,
                    "workflow_new",
                    workflows.len() >= MAXIMUM_WORKFLOWS,
                )),
        )
        .child(
            div()
                .id("workflow_list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&page.list_scroll)
                .px_2()
                .pb_2()
                .flex()
                .flex_col()
                .gap_1()
                .when(rows.is_empty(), |list| {
                    list.child(
                        div()
                            .px_2()
                            .py_3()
                            .child(secondary_text(tr!(workflows::LIST_EMPTY), theme)),
                    )
                })
                .children(rows),
        )
}

fn new_workflow_button(ui: Page<'_>, id: &'static str, full: bool) -> Button {
    let open = ui.terminal.clone();
    Button::new(id, ui.theme)
        .variant(ButtonVariant::Primary)
        .button_size(ButtonSize::Sm)
        .icon(header_icon(HugeIcon::Add))
        .label(tr!(workflows::NEW_BUTTON))
        .aria_label(tr!(workflows::NEW_ARIA))
        .disabled(full)
        .on_click(move |_, window, cx| {
            open.update(cx, |terminal, terminal_cx| {
                terminal.open_new_workflow(window, terminal_cx);
            });
        })
}

fn list_row(ui: Page<'_>, index: usize, view: &WorkflowView, shown: bool) -> Stateful<Div> {
    let colors = ui.theme.colors;
    let definition = &view.definition;
    let open = ui.terminal.clone();
    let workflow_id = definition.workflow_id.clone();
    let detail = format!(
        "{} · {}",
        symbol_text(ui.set, Some(&definition.instrument_id)),
        account_text(ui.set, &definition.account_id)
    );
    div()
        .id(("workflow_list_row", index))
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded(radius(1.0))
        .cursor_pointer()
        .when(shown, |row| row.bg(gpui_color(colors.active_bg)))
        .when(!shown, |row| {
            row.hover(|style| style.bg(gpui_color(colors.hover_bg)))
        })
        .on_click(move |_, window, cx| {
            open.update(cx, |terminal, terminal_cx| {
                terminal.open_saved_workflow(&workflow_id, window, terminal_cx);
            });
        })
        .child(
            div()
                .text_sm()
                .truncate()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .child(definition.name.clone()),
        )
        .child(secondary_text(detail, ui.theme).truncate())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(state_color(view, ui.theme))
                        .child(state_text(view)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(gpui_color(colors.text_muted))
                        .child(tr!(
                            workflows::FIRED_VALUE,
                            session = view.session_firings,
                            total = view.total_firings
                        )),
                ),
        )
}

fn empty_state(ui: Page<'_>) -> Div {
    let theme = ui.theme;
    let full = ui
        .set
        .is_some_and(|set| set.workflows.len() >= MAXIMUM_WORKFLOWS);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            div()
                .max_w(px(EMPTY_STATE_WIDTH))
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .text_center()
                .child(
                    div()
                        .text_xl()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child(tr!(workflows::EMPTY_TITLE)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(theme.colors.text_secondary))
                        .child(tr!(workflows::EMPTY_BODY)),
                )
                .child(new_workflow_button(ui, "workflow_new_empty", full)),
        )
}

fn editor(ui: Page<'_>, page: &WorkflowsPage, open: &OpenWorkflow) -> Div {
    let (status, view) = draft_status(&open.draft, ui.set);
    let scales = instrument_scales(instrument(ui.set, open.draft.instrument_id.as_ref()));
    let problem = open.draft.build(String::new(), 1, scales).err();
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(toolbar(ui, open, status, view.as_ref(), problem))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(palette(ui, open))
                .child(board(ui, open, problem.and_then(DraftProblem::item)))
                .child(inspector(ui, page, open, view.as_ref())),
        )
}

fn toolbar(
    ui: Page<'_>,
    open: &OpenWorkflow,
    status: DraftStatus,
    view: Option<&WorkflowView>,
    problem: Option<DraftProblem>,
) -> Div {
    let theme = ui.theme;
    let colors = theme.colors;
    let draft = &open.draft;
    let mut detail = vec![symbol_text(ui.set, draft.instrument_id.as_ref())];
    detail.extend(
        draft
            .account_id
            .as_ref()
            .map(|account_id| account_text(ui.set, account_id)),
    );
    let (status_text, status_color) = match (status, view) {
        (DraftStatus::Saved, Some(view)) => (state_text(view), state_color(view, theme)),
        (DraftStatus::Changed, _) => (
            tr!(workflows::STATUS_CHANGED).to_string(),
            gpui_color(colors.text_warning),
        ),
        (DraftStatus::New | DraftStatus::Saved, _) => (
            tr!(workflows::STATUS_NEW).to_string(),
            gpui_color(colors.text_secondary),
        ),
    };
    let (readiness, readiness_color) = match (&open.status, problem) {
        (Some(PageStatus::Problem(text)), _) => (text.clone(), colors.text_danger),
        (Some(PageStatus::Note(text)), _) => (text.clone(), colors.text_positive),
        (None, Some(problem)) => (problem_text(problem), colors.text_warning),
        (None, None) => (tr!(workflows::READY).to_string(), colors.text_secondary),
    };
    let name = draft.name.trim();
    let name = if name.is_empty() {
        tr!(workflows::UNTITLED).to_string()
    } else {
        name.to_string()
    };
    let environment = account_environment(ui.set, draft.account_id.as_ref());
    div()
        .flex_none()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .border_b(border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_lg()
                                .font_weight(platform_font_weight(TypographyRole::Strong))
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(status_color)
                                .child(status_text),
                        )
                        .children(
                            environment.map(|environment| environment_badge(environment, theme)),
                        ),
                )
                .child(secondary_text(detail.join(" · "), theme).truncate())
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(gpui_color(readiness_color))
                        .child(readiness),
                ),
        )
        .child(command_buttons(ui, open, status, view))
}

/// Practice and demo accounts are named quietly; a live account is called out, because its
/// orders reach the broker.
fn environment_badge(environment: AccountEnvironment, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let (text, color) = match environment {
        AccountEnvironment::Live => (tr!(workflows::LIVE_WARNING), colors.text_warning),
        AccountEnvironment::Simulated | AccountEnvironment::Demo => {
            (environment_text(environment), colors.text_secondary)
        }
    };
    div()
        .flex_none()
        .px_2()
        .rounded(radius(1.0))
        .border(border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .text_xs()
        .text_color(gpui_color(color))
        .child(text)
}

fn command_buttons(
    ui: Page<'_>,
    open: &OpenWorkflow,
    status: DraftStatus,
    view: Option<&WorkflowView>,
) -> Div {
    let theme = ui.theme;
    let pending = open.pending.is_some();
    let saved = status == DraftStatus::Saved;
    let save = ui.terminal.clone();
    let save_button = Button::new("workflow_save", theme)
        .variant(if saved {
            ButtonVariant::Outline
        } else {
            ButtonVariant::Primary
        })
        .button_size(ButtonSize::Sm)
        .label(tr!(workflows::SAVE))
        .disabled(pending)
        .loading(pending)
        .on_click(move |_, _, cx| save.update(cx, TerminalApp::save_open_workflow));
    let run = view.and_then(|view| run_button(ui, view, saved, pending));
    let remove = ui.terminal.clone();
    let delete_button = Button::new("workflow_delete", theme)
        .variant(if open.confirm_delete {
            ButtonVariant::Destructive
        } else {
            ButtonVariant::Ghost
        })
        .button_size(ButtonSize::Sm)
        .danger_on_hover()
        .icon(header_icon(HugeIcon::Trash))
        .label(if open.confirm_delete {
            tr!(workflows::DELETE_CONFIRM)
        } else {
            tr!(workflows::DELETE)
        })
        .disabled(pending)
        .on_click(move |_, _, cx| remove.update(cx, TerminalApp::delete_open_workflow));
    let close = ui.terminal.clone();
    let close_button = Button::new("workflow_close", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .icon(header_icon(HugeIcon::Close))
        .aria_label(tr!(workflows::CLOSE_ARIA))
        .on_click(move |_, _, cx| close.update(cx, TerminalApp::close_open_workflow));
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .child(save_button)
        .children(run)
        .child(delete_button)
        .child(close_button)
}

/// Arm for a paused workflow whose draft is the saved one, Pause for an armed one; a
/// finished workflow has neither.
fn run_button(ui: Page<'_>, view: &WorkflowView, saved: bool, pending: bool) -> Option<Button> {
    let theme = ui.theme;
    let run = ui.terminal.clone();
    let name = view.definition.name.as_str();
    let (id, label, aria, variant, arm) = match view.state {
        WorkflowState::Paused => (
            "workflow_arm",
            tr!(workflows::ARM),
            tr!(workflows::ARM_ARIA, name = name),
            ButtonVariant::Primary,
            true,
        ),
        WorkflowState::Armed => (
            "workflow_pause",
            tr!(workflows::PAUSE),
            tr!(workflows::PAUSE_ARIA, name = name),
            ButtonVariant::Outline,
            false,
        ),
        WorkflowState::Completed | WorkflowState::Expired => return None,
    };
    let blocked = arm && !saved;
    let button = Button::new(id, theme)
        .variant(variant)
        .button_size(ButtonSize::Sm)
        .label(label)
        .aria_label(aria)
        .disabled(pending || blocked)
        .on_click(move |_, _, cx| {
            run.update(cx, |terminal, terminal_cx| {
                terminal.arm_open_workflow(arm, terminal_cx);
            });
        });
    Some(if blocked {
        button.tooltip(
            TooltipSpec::new(tr!(workflows::ARM_HINT), theme).show_delay(TOOLTIP_OPEN_DELAY),
        )
    } else {
        button
    })
}

fn palette(ui: Page<'_>, open: &OpenWorkflow) -> Div {
    let theme = ui.theme;
    let full = open.draft.nodes.len() >= MAXIMUM_WORKFLOW_NODES;
    let group = |title: &'static str, id: &'static str, kinds: &[BlockKind]| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(secondary_text(title, theme))
            .children(
                kinds
                    .iter()
                    .enumerate()
                    .map(|(index, kind)| palette_button(ui, (id, index), *kind, full)),
            )
    };
    div()
        .w(px(PALETTE_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .gap_4()
        .p_3()
        .border_r(border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(heading(tr!(workflows::ADD_BLOCK), theme))
                .child(secondary_text(tr!(workflows::PALETTE_HINT), theme)),
        )
        .child(group(
            tr!(workflows::PALETTE_CHECKS),
            "workflow_add_check",
            &BlockKind::CHECKS,
        ))
        .child(group(
            tr!(workflows::PALETTE_ACTIONS),
            "workflow_add_action",
            &BlockKind::ACTIONS,
        ))
}

fn palette_button(ui: Page<'_>, id: (&'static str, usize), kind: BlockKind, full: bool) -> Button {
    let add = ui.terminal.clone();
    Button::new(id, ui.theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Sm)
        .full_width()
        .leading(
            div()
                .size(px(PALETTE_DOT_SIZE))
                .rounded_full()
                .bg(kind_accent(Some(kind), ui.theme)),
        )
        .label(kind_label(kind))
        .disabled(full)
        .on_click(move |_, window, cx| {
            add.update(cx, |terminal, terminal_cx| {
                terminal.add_workflow_block(kind, window, terminal_cx);
            });
        })
}

/// A link drawn between two board-local screen points.
#[derive(Clone, Copy)]
struct Wire {
    from: BoardPoint,
    to: BoardPoint,
    color: Hsla,
}

fn port_color(port: OutputPort, theme: &AerisTheme) -> Hsla {
    let colors = theme.colors;
    gpui_color(match port {
        OutputPort::Then(_) => colors.positive,
        OutputPort::Otherwise(_) => colors.negative,
        OutputPort::Trigger | OutputPort::Next(_) => colors.text_secondary,
    })
}

fn port_label(port: OutputPort) -> Option<&'static str> {
    match port {
        OutputPort::Then(_) => Some(tr!(workflows::PORT_YES)),
        OutputPort::Otherwise(_) => Some(tr!(workflows::PORT_NO)),
        OutputPort::Trigger | OutputPort::Next(_) => None,
    }
}

fn board_wires(open: &OpenWorkflow, theme: &AerisTheme) -> Vec<Wire> {
    let draft = &open.draft;
    let camera = open.camera;
    let mut wires = draft
        .links()
        .into_iter()
        .filter_map(|(port, target)| {
            Some(Wire {
                from: camera.to_screen(draft.output_anchor(port)?),
                to: camera.to_screen(draft.input_anchor(target)?),
                color: port_color(port, theme),
            })
        })
        .collect::<Vec<_>>();
    if let Some(BoardGesture::Connect { port, cursor }) = open.gesture
        && let Some(from) = draft.output_anchor(port)
    {
        wires.push(Wire {
            from: camera.to_screen(from),
            to: camera.to_screen(cursor),
            color: gpui_color(theme.colors.primary),
        });
    }
    wires
}

fn local_point(position: Point<Pixels>, origin: Point<Pixels>) -> BoardPoint {
    BoardPoint::new(
        f32::from(position.x - origin.x),
        f32::from(position.y - origin.y),
    )
}

fn paint_wire(window: &mut Window, origin: Point<Pixels>, wire: Wire, zoom: f32) {
    let start = point(origin.x + px(wire.from.x), origin.y + px(wire.from.y));
    let end = point(origin.x + px(wire.to.x), origin.y + px(wire.to.y));
    let bend = px(((wire.to.x - wire.from.x).abs() / 2.0).max(WIRE_MINIMUM_BEND * zoom));
    let mut path = PathBuilder::stroke(px(WIRE_WIDTH));
    path.move_to(start);
    path.cubic_bezier_to(
        end,
        point(start.x + bend, start.y),
        point(end.x - bend, end.y),
    );
    // A degenerate curve has no outline to paint.
    if let Ok(path) = path.build() {
        window.paint_path(path, wire.color);
    }
}

/// Follows the pointer anywhere in the window while a block, link or the board is dragged,
/// the way a pointer capture would; a release outside the window ends the drag on the next
/// move.
fn capture_pointer(window: &mut Window, origin: Point<Pixels>, terminal: &Entity<TerminalApp>) {
    let moving = terminal.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let at = local_point(event.position, origin);
        let released = event.pressed_button != Some(MouseButton::Left);
        moving.update(cx, |terminal, terminal_cx| {
            if released {
                terminal.workflow_pointer_up(at, terminal_cx);
            } else {
                terminal.workflow_pointer_move(at, terminal_cx);
            }
        });
    });
    let releasing = terminal.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        let at = local_point(event.position, origin);
        releasing.update(cx, |terminal, terminal_cx| {
            terminal.workflow_pointer_up(at, terminal_cx);
        });
    });
}

fn board(ui: Page<'_>, open: &OpenWorkflow, problem: Option<BoardItem>) -> Stateful<Div> {
    let theme = ui.theme;
    let camera = open.camera;
    let origin = Rc::new(Cell::new(Point::<Pixels>::default()));
    let wires = board_wires(open, theme);
    let capture = open.gesture.is_some();
    let paint_terminal = ui.terminal.clone();
    let prepaint_origin = Rc::clone(&origin);
    let links = canvas(
        move |bounds, _, _| prepaint_origin.set(bounds.origin),
        move |bounds, (), window, _| {
            for wire in &wires {
                paint_wire(window, bounds.origin, *wire, camera.zoom);
            }
            if capture {
                capture_pointer(window, bounds.origin, &paint_terminal);
            }
        },
    )
    .absolute()
    .size_full();
    let down_origin = Rc::clone(&origin);
    let down = ui.terminal.clone();
    let wheel = ui.terminal.clone();
    div()
        .id("workflow_board")
        .relative()
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface_secondary))
        .child(links)
        .children(blocks(ui, open, problem))
        .children(ports(open, theme))
        .child(
            div()
                .absolute()
                .bottom_3()
                .left_3()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(tr!(workflows::BOARD_HINT, key = ZOOM_KEY)),
        )
        .child(zoom_controls(ui, camera.zoom))
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, cx| {
                let at = local_point(event.position, down_origin.get());
                down.update(cx, |terminal, terminal_cx| {
                    terminal.workflow_pointer_down(at, window, terminal_cx);
                });
            },
        )
        .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
            let at = local_point(event.position, origin.get());
            let delta = event.delta.pixel_delta(window.line_height());
            let delta = BoardPoint::new(f32::from(delta.x), f32::from(delta.y));
            let zoom = event.modifiers.secondary();
            wheel.update(cx, |terminal, terminal_cx| {
                terminal.workflow_wheel(at, delta, zoom, terminal_cx);
            });
        })
}

fn zoom_controls(ui: Page<'_>, zoom: f32) -> Div {
    let theme = ui.theme;
    let zoom_button = |id: &'static str, icon: HugeIcon, aria: &'static str, zoom_in: bool| {
        let terminal = ui.terminal.clone();
        Button::new(id, theme)
            .variant(ButtonVariant::Outline)
            .button_size(ButtonSize::Sm)
            .icon(header_icon(icon))
            .aria_label(aria)
            .on_click(move |_, _, cx| {
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.zoom_workflow_board(zoom_in, terminal_cx);
                });
            })
    };
    let reset = ui.terminal.clone();
    div()
        .absolute()
        .bottom_3()
        .right_3()
        .flex()
        .items_center()
        .gap_1()
        .child(zoom_button(
            "workflow_zoom_out",
            HugeIcon::Minus,
            tr!(workflows::ZOOM_OUT_ARIA),
            false,
        ))
        .child(
            Button::new("workflow_zoom_reset", theme)
                .variant(ButtonVariant::Outline)
                .button_size(ButtonSize::Sm)
                .tabular_numerals()
                .label(format!("{:.0}%", zoom * 100.0))
                .aria_label(tr!(workflows::ZOOM_RESET_ARIA))
                .on_click(move |_, _, cx| {
                    reset.update(cx, TerminalApp::reset_workflow_board_view);
                }),
        )
        .child(zoom_button(
            "workflow_zoom_in",
            HugeIcon::Add,
            tr!(workflows::ZOOM_IN_ARIA),
            true,
        ))
}

/// The look of one block on the board.
struct BlockLook {
    title: &'static str,
    summary: String,
    accent: Hsla,
    /// An If keeps room on its right for the Yes and No labels.
    branches: bool,
}

fn blocks(ui: Page<'_>, open: &OpenWorkflow, problem: Option<BoardItem>) -> Vec<Div> {
    let draft = &open.draft;
    let symbol = symbol_text(ui.set, draft.instrument_id.as_ref());
    let trigger = BlockLook {
        title: tr!(workflows::BLOCK_TRIGGER),
        summary: trigger_summary(&draft.trigger, &symbol),
        accent: kind_accent(None, ui.theme),
        branches: false,
    };
    std::iter::once((BoardItem::Trigger, draft.trigger_at, trigger))
        .chain(draft.nodes.iter().map(|node| {
            let kind = node.block.kind();
            let look = BlockLook {
                title: kind_label(kind),
                summary: block_summary(&node.block),
                accent: kind_accent(Some(kind), ui.theme),
                branches: matches!(node.block, BlockDraft::If { .. }),
            };
            (BoardItem::Node(node.id), node.at, look)
        }))
        .map(|(item, at, look)| {
            block_element(open, item, at, look, problem == Some(item), ui.theme)
        })
        .collect()
}

fn block_element(
    open: &OpenWorkflow,
    item: BoardItem,
    at: BoardPoint,
    look: BlockLook,
    problem: bool,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let zoom = open.camera.zoom;
    let screen = open.camera.to_screen(at);
    let border = if open.selected == Some(item) {
        colors.ring_primary
    } else if problem {
        colors.text_danger
    } else {
        colors.border_secondary
    };
    let right_padding = if look.branches {
        PORT_LABEL_WIDTH + OUTPUT_PORT_SIZE
    } else {
        BLOCK_PADDING_X
    };
    div()
        .absolute()
        .left(px(screen.x))
        .top(px(screen.y))
        .w(px(NODE_WIDTH * zoom))
        .h(px(NODE_HEIGHT * zoom))
        .flex()
        .overflow_hidden()
        .rounded(radius(zoom))
        .border(border_width(theme))
        .border_color(gpui_color(border))
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .flex_none()
                .w(px(BLOCK_ACCENT_WIDTH * zoom))
                .h_full()
                .bg(look.accent),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(BLOCK_GAP * zoom))
                .pl(px(BLOCK_PADDING_X * zoom))
                .pr(px(right_padding * zoom))
                .py(px(BLOCK_PADDING_Y * zoom))
                .child(
                    div()
                        .truncate()
                        .text_size(px(BLOCK_TITLE_SIZE * zoom))
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(look.accent)
                        .child(look.title),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .text_size(px(BLOCK_TEXT_SIZE * zoom))
                        .line_height(px(BLOCK_LINE_HEIGHT * zoom))
                        .text_color(gpui_color(colors.text_default))
                        .child(look.summary),
                ),
        )
}

fn dot(center: BoardPoint, size: f32, fill: Hsla, edge: Hsla, theme: &AerisTheme) -> Div {
    div()
        .absolute()
        .left(px(center.x - size / 2.0))
        .top(px(center.y - size / 2.0))
        .size(px(size))
        .rounded_full()
        .bg(fill)
        .border(border_width(theme))
        .border_color(edge)
}

/// The output dots links leave from, filled once linked, with an If's Yes and No labels,
/// and the input dot of every block.
fn ports(open: &OpenWorkflow, theme: &AerisTheme) -> Vec<Div> {
    let draft = &open.draft;
    let camera = open.camera;
    let zoom = camera.zoom;
    let surface = gpui_color(theme.colors.surface);
    let mut elements = Vec::new();
    for port in draft.output_ports() {
        let Some(anchor) = draft.output_anchor(port) else {
            continue;
        };
        let center = camera.to_screen(anchor);
        let color = port_color(port, theme);
        let fill = if draft.link(port).is_some() {
            color
        } else {
            surface
        };
        elements.push(dot(center, OUTPUT_PORT_SIZE * zoom, fill, color, theme));
        if let Some(label) = port_label(port) {
            let height = PORT_LABEL_SIZE * 1.4 * zoom;
            elements.push(
                div()
                    .absolute()
                    .left(px(
                        center.x - (PORT_LABEL_WIDTH + OUTPUT_PORT_SIZE / 2.0) * zoom
                    ))
                    .top(px(center.y - height / 2.0))
                    .w(px(PORT_LABEL_WIDTH * zoom))
                    .h(px(height))
                    .flex()
                    .items_center()
                    .justify_end()
                    .text_size(px(PORT_LABEL_SIZE * zoom))
                    .text_color(color)
                    .child(label),
            );
        }
    }
    let edge = gpui_color(theme.colors.border);
    elements.extend(draft.nodes.iter().filter_map(|node| {
        let anchor = draft.input_anchor(node.id)?;
        Some(dot(
            camera.to_screen(anchor),
            INPUT_PORT_SIZE * zoom,
            surface,
            edge,
            theme,
        ))
    }));
    elements
}

fn inspector(
    ui: Page<'_>,
    page: &WorkflowsPage,
    open: &OpenWorkflow,
    view: Option<&WorkflowView>,
) -> Stateful<Div> {
    let theme = ui.theme;
    let sections = match open.selected {
        None => workflow_settings(ui, open, view),
        Some(BoardItem::Trigger) => trigger_settings(ui, open),
        Some(BoardItem::Node(id)) => open
            .draft
            .node(id)
            .map(|node| node_settings(ui, open, node))
            .unwrap_or_default(),
    };
    div()
        .id("workflow_inspector")
        .w(px(INSPECTOR_WIDTH))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .track_scroll(&page.inspector_scroll)
        .border_l(border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .p_4()
        .flex()
        .flex_col()
        .gap_4()
        .children(sections)
}

fn workflow_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    view: Option<&WorkflowView>,
) -> Vec<AnyElement> {
    let theme = ui.theme;
    let mut sections = vec![heading(tr!(workflows::INSPECTOR_WORKFLOW), theme).into_any_element()];
    sections.extend(
        input_field(ui, open, DraftField::Name, tr!(workflows::FIELD_NAME))
            .map(IntoElement::into_any_element),
    );
    sections.push(account_picker(ui, open).into_any_element());
    sections.push(symbol_picker(ui, open).into_any_element());
    sections.push(repeat_settings(ui, open).into_any_element());
    sections.extend(view.map(|view| firings(view, theme).into_any_element()));
    sections.push(secondary_text(tr!(workflows::SAVED_NOTE), theme).into_any_element());
    sections
}

fn account_picker(ui: Page<'_>, open: &OpenWorkflow) -> Div {
    let accounts = ui.set.map_or(&[][..], |set| set.accounts.as_slice());
    if accounts.is_empty() {
        return field(
            tr!(workflows::FIELD_ACCOUNT),
            secondary_text(tr!(workflows::NO_ACCOUNTS), ui.theme),
            ui.theme,
        );
    }
    let options = accounts
        .iter()
        .map(|account| {
            (
                account.id.clone(),
                SharedString::from(format!(
                    "{} · {}",
                    account.display_name,
                    environment_text(account.environment)
                )),
            )
        })
        .collect();
    field(
        tr!(workflows::FIELD_ACCOUNT),
        choices(
            ui,
            "workflow_account",
            options,
            open.draft.account_id.as_ref(),
            |open, account_id: TradingAccountId| open.draft.account_id = Some(account_id),
        ),
        ui.theme,
    )
}

fn symbol_picker(ui: Page<'_>, open: &OpenWorkflow) -> Div {
    let instruments = ui.set.map_or(&[][..], |set| set.instruments.as_slice());
    let options = instruments
        .iter()
        .map(|instrument| {
            (
                instrument.instrument_id.clone(),
                SharedString::from(instrument.contract.provenance.display_symbol.clone()),
            )
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(field(
            tr!(workflows::FIELD_SYMBOL),
            choices(
                ui,
                "workflow_symbol",
                options,
                open.draft.instrument_id.as_ref(),
                |open, instrument_id| open.draft.instrument_id = Some(instrument_id),
            ),
            ui.theme,
        ))
        .child(secondary_text(tr!(workflows::SYMBOL_HINT), ui.theme))
}

fn repeat_settings(ui: Page<'_>, open: &OpenWorkflow) -> Div {
    let options = vec![
        (RepeatDraft::Once, tr!(workflows::REPEAT_CHOICE_ONCE).into()),
        (
            RepeatDraft::PerSession,
            tr!(workflows::REPEAT_CHOICE_SESSION).into(),
        ),
    ];
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(field(
            tr!(workflows::FIELD_REPEAT),
            choices(
                ui,
                "workflow_repeat",
                options,
                Some(&open.draft.repeat),
                |open, repeat| open.draft.repeat = repeat,
            ),
            ui.theme,
        ))
        .children(input_field(
            ui,
            open,
            DraftField::SessionMaximum,
            tr!(workflows::FIELD_SESSION_MAXIMUM),
        ))
        .children(input_field(
            ui,
            open,
            DraftField::Cooldown,
            tr!(workflows::FIELD_COOLDOWN),
        ))
}

fn firings(view: &WorkflowView, theme: &AerisTheme) -> Div {
    let rows = view
        .recent_firings
        .iter()
        .take(RECENT_FIRINGS_SHOWN)
        .map(|firing| {
            div()
                .flex()
                .flex_col()
                .child(secondary_text(
                    firing_time_text(firing.fired_unix_nanos),
                    theme,
                ))
                .child(div().text_xs().child(outcome_text(&firing.outcome)))
        })
        .collect::<Vec<_>>();
    field(
        tr!(workflows::RECENT_FIRINGS),
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().child(tr!(
                workflows::FIRED_VALUE,
                session = view.session_firings,
                total = view.total_firings
            )))
            .when(rows.is_empty(), |list| {
                list.child(secondary_text(tr!(workflows::NEVER_FIRED), theme))
            })
            .children(rows),
        theme,
    )
}

fn zone_choices(
    ui: Page<'_>,
    id: &'static str,
    zone: WorkflowTimeZone,
    apply: impl Fn(&mut OpenWorkflow, WorkflowTimeZone) + 'static,
) -> Div {
    field(
        tr!(workflows::FIELD_ZONE),
        choices(
            ui,
            id,
            vec![
                (
                    WorkflowTimeZone::Chicago,
                    tr!(workflows::ZONE_CHICAGO).into(),
                ),
                (WorkflowTimeZone::Utc, tr!(workflows::ZONE_UTC).into()),
            ],
            Some(&zone),
            apply,
        ),
        ui.theme,
    )
}

fn trigger_settings(ui: Page<'_>, open: &OpenWorkflow) -> Vec<AnyElement> {
    let theme = ui.theme;
    let trigger = &open.draft.trigger;
    let options = vec![
        (
            TriggerKind::Rises,
            tr!(workflows::TRIGGER_CHOICE_ABOVE).into(),
        ),
        (
            TriggerKind::Falls,
            tr!(workflows::TRIGGER_CHOICE_BELOW).into(),
        ),
        (
            TriggerKind::Time,
            tr!(workflows::TRIGGER_CHOICE_TIME).into(),
        ),
    ];
    let mut sections = vec![
        heading(tr!(workflows::BLOCK_TRIGGER), theme).into_any_element(),
        field(
            tr!(workflows::FIELD_TRIGGER),
            choices(
                ui,
                "workflow_trigger",
                options,
                Some(&trigger.kind()),
                |open, kind| open.draft.set_trigger_kind(kind),
            ),
            theme,
        )
        .into_any_element(),
    ];
    match trigger {
        super::workflow_board::TriggerDraft::Price { .. } => {
            sections.extend(
                input_field(
                    ui,
                    open,
                    DraftField::TriggerLevel,
                    tr!(workflows::FIELD_LEVEL),
                )
                .map(IntoElement::into_any_element),
            );
            sections
                .push(secondary_text(tr!(workflows::TRIGGER_PRICE_HINT), theme).into_any_element());
        }
        super::workflow_board::TriggerDraft::Time { zone, .. } => {
            sections.extend(
                input_field(
                    ui,
                    open,
                    DraftField::TriggerTime,
                    tr!(workflows::FIELD_TIME),
                )
                .map(IntoElement::into_any_element),
            );
            sections.push(
                zone_choices(ui, "workflow_trigger_zone", *zone, |open, zone| {
                    if let super::workflow_board::TriggerDraft::Time { zone: current, .. } =
                        &mut open.draft.trigger
                    {
                        *current = zone;
                    }
                })
                .into_any_element(),
            );
            sections
                .push(secondary_text(tr!(workflows::TRIGGER_TIME_HINT), theme).into_any_element());
        }
    }
    sections
}

fn kind_choices(
    ui: Page<'_>,
    id: &'static str,
    kinds: &[BlockKind],
    current: BlockKind,
    node: WorkflowNodeId,
) -> Div {
    choices(
        ui,
        id,
        kinds
            .iter()
            .map(|kind| (*kind, SharedString::from(kind_label(*kind))))
            .collect(),
        Some(&current),
        move |open, kind| open.draft.change_block(node, kind),
    )
}

fn node_settings(ui: Page<'_>, open: &OpenWorkflow, node: &NodeDraft) -> Vec<AnyElement> {
    let theme = ui.theme;
    let id = node.id;
    let kind = node.block.kind();
    let mut sections = vec![heading(kind_label(kind), theme).into_any_element()];
    match &node.block {
        BlockDraft::If { condition, .. } => {
            sections.push(
                field(
                    tr!(workflows::FIELD_CHECK),
                    kind_choices(ui, "workflow_check", &BlockKind::CHECKS, kind, id),
                    theme,
                )
                .into_any_element(),
            );
            sections.extend(condition_settings(ui, open, id, condition));
            sections.push(secondary_text(tr!(workflows::IF_HINT), theme).into_any_element());
        }
        BlockDraft::Action { action, .. } => {
            sections.push(
                field(
                    tr!(workflows::FIELD_ACTION),
                    kind_choices(ui, "workflow_action", &BlockKind::ACTIONS, kind, id),
                    theme,
                )
                .into_any_element(),
            );
            sections.extend(action_settings(ui, open, id, action));
        }
    }
    sections.push(
        Button::new("workflow_remove_block", theme)
            .variant(ButtonVariant::Outline)
            .button_size(ButtonSize::Sm)
            .danger_on_hover()
            .icon(header_icon(HugeIcon::Trash))
            .label(tr!(workflows::REMOVE_BLOCK))
            .on_click(edit(ui.terminal, move |open| {
                open.draft.remove_node(id);
                open.selected = None;
            }))
            .into_any_element(),
    );
    sections
}

fn condition_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    id: WorkflowNodeId,
    condition: &ConditionDraft,
) -> Vec<AnyElement> {
    match condition {
        ConditionDraft::Position(requirement) => position_settings(ui, id, *requirement),
        ConditionDraft::Between { zone, .. } => [
            input_field(
                ui,
                open,
                DraftField::WindowStart(id),
                tr!(workflows::FIELD_START),
            ),
            input_field(
                ui,
                open,
                DraftField::WindowEnd(id),
                tr!(workflows::FIELD_END),
            ),
            Some(zone_choices(
                ui,
                "workflow_window_zone",
                *zone,
                move |open, zone| {
                    if let Some(ConditionDraft::Between { zone: current, .. }) =
                        open.draft.condition_mut(id)
                    {
                        *current = zone;
                    }
                },
            )),
        ]
        .into_iter()
        .flatten()
        .map(IntoElement::into_any_element)
        .collect(),
        ConditionDraft::Price { direction, .. } => price_settings(ui, open, id, *direction),
    }
}

fn position_settings(
    ui: Page<'_>,
    id: WorkflowNodeId,
    requirement: PositionRequirement,
) -> Vec<AnyElement> {
    let options = vec![
        (
            PositionRequirement::Flat,
            tr!(workflows::CHOICE_FLAT).into(),
        ),
        (
            PositionRequirement::Long,
            tr!(workflows::CHOICE_LONG).into(),
        ),
        (
            PositionRequirement::Short,
            tr!(workflows::CHOICE_SHORT).into(),
        ),
        (
            PositionRequirement::Open,
            tr!(workflows::CHOICE_OPEN).into(),
        ),
    ];
    vec![
        field(
            tr!(workflows::FIELD_POSITION),
            choices(
                ui,
                "workflow_position",
                options,
                Some(&requirement),
                move |open, requirement| {
                    if let Some(ConditionDraft::Position(current)) = open.draft.condition_mut(id) {
                        *current = requirement;
                    }
                },
            ),
            ui.theme,
        )
        .into_any_element(),
    ]
}

fn price_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    id: WorkflowNodeId,
    direction: PriceDirection,
) -> Vec<AnyElement> {
    let options = vec![
        (PriceDirection::Above, tr!(workflows::CHOICE_ABOVE).into()),
        (PriceDirection::Below, tr!(workflows::CHOICE_BELOW).into()),
    ];
    [
        Some(field(
            tr!(workflows::FIELD_DIRECTION),
            choices(
                ui,
                "workflow_direction",
                options,
                Some(&direction),
                move |open, direction| {
                    if let Some(ConditionDraft::Price {
                        direction: current, ..
                    }) = open.draft.condition_mut(id)
                    {
                        *current = direction;
                    }
                },
            ),
            ui.theme,
        )),
        input_field(ui, open, DraftField::Level(id), tr!(workflows::FIELD_LEVEL)),
    ]
    .into_iter()
    .flatten()
    .map(IntoElement::into_any_element)
    .collect()
}

fn action_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    id: WorkflowNodeId,
    action: &ActionDraft,
) -> Vec<AnyElement> {
    let theme = ui.theme;
    match action {
        ActionDraft::Enter { .. } => {
            let inputs = [
                input_field(
                    ui,
                    open,
                    DraftField::Quantity(id),
                    tr!(workflows::FIELD_QUANTITY),
                ),
                input_field(
                    ui,
                    open,
                    DraftField::StopTicks(id),
                    tr!(workflows::FIELD_STOP_TICKS),
                ),
                input_field(
                    ui,
                    open,
                    DraftField::TargetTicks(id),
                    tr!(workflows::FIELD_TARGET_TICKS),
                ),
            ];
            vec![
                div()
                    .flex()
                    .gap_2()
                    .children(
                        inputs
                            .into_iter()
                            .flatten()
                            .map(|input| input.flex_1().min_w_0()),
                    )
                    .into_any_element(),
                secondary_text(tr!(workflows::PROTECTION_HINT), theme).into_any_element(),
            ]
        }
        ActionDraft::LockAccount => {
            vec![secondary_text(tr!(workflows::LOCK_HINT), theme).into_any_element()]
        }
        ActionDraft::Flatten | ActionDraft::CancelOrders => Vec::new(),
    }
}
