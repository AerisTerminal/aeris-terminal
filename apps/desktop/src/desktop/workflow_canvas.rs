//! The Workflows page's elements: the workflow list, the board with its blocks, links, pan
//! and zoom, the block palette and the inspector of the selection. Every control edits the
//! open draft through `TerminalApp`; nothing here talks to the trading owner.

use super::market_screener::AppView;
use super::symbol_browser::{InstrumentMenuAction, InstrumentMenuEntry};
use super::workflow_board::BoardCamera;
use super::workflow_board::{
    ActionDraft, BlockDraft, BlockKind, BoardItem, BoardPoint, CheckDraft, CheckKind, CheckRef,
    ConditionDraft, ConditionOwner, DraftField, DraftProblem, NODE_HEIGHT, NODE_TAB_HEIGHT,
    NODE_WIDTH, NodeDraft, OutputPort, RepeatDraft, TriggerDraft, TriggerKind, WorkflowExample,
};
use super::workflows_page::{
    BoardGesture, DraftStatus, HeaderMenu, OpenWorkflow, PageStatus, WorkflowsPage,
    account_environment, account_text, action_summary, check_summary, condition_summary, day_label,
    days_text, draft_currency_scale, draft_scales, draft_status, draft_symbol, environment_text,
    example_description, example_name, firing_time_text, outcome_text, problem_text, state_color,
    state_text, symbol_text, trigger_summary, zone_text,
};
use super::*;
use aeris_localization::{tr, tr_count, workflows};
use aeris_trading::AccountEnvironment;
use aeris_trading_runtime::{
    MAXIMUM_WORKFLOW_CHECKS, MAXIMUM_WORKFLOW_NODES, MAXIMUM_WORKFLOW_TRIGGERS, MAXIMUM_WORKFLOWS,
    PositionRequirement, PriceDirection, WorkflowCompare, WorkflowJoin, WorkflowNodeId,
    WorkflowScope, WorkflowState, WorkflowTimeZone, WorkflowView, WorkflowWeekdays, WorkingSet,
};
use gpui::{
    Bounds, ClickEvent, DispatchPhase, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder,
    Point, ScrollWheelEvent, fill, size,
};

const SIDEBAR_WIDTH: f32 = 264.0;
const INSPECTOR_WIDTH: f32 = 296.0;
const EMPTY_STATE_WIDTH: f32 = 440.0;
const OUTPUT_PORT_SIZE: f32 = 12.0;
const INPUT_PORT_SIZE: f32 = 8.0;
const PORT_LABEL_WIDTH: f32 = 36.0;
const PORT_LABEL_SIZE: f32 = 10.0;
const PORT_LABEL_GAP: f32 = 4.0;
/// The room the zoom buttons take in the board's bottom-right corner, kept clear of the hint.
const ZOOM_CONTROLS_WIDTH: f32 = 176.0;
const WIRE_WIDTH: f32 = 2.0;
const WIRE_MINIMUM_BEND: f32 = 48.0;
/// The folder tab's width, and how far its right edge leans.
const CARD_TAB_WIDTH: f32 = 116.0;
const CARD_TAB_SLANT: f32 = 14.0;
const CARD_TAB_RADIUS: f32 = 8.0;
const CARD_RADIUS: f32 = 12.0;
/// How far a slanted corner's rounding reaches along each edge.
const CARD_SLANT_ROUNDING: f32 = 6.0;
/// The accent tab on the top edge, which carries the block's color.
const CARD_ACCENT_WIDTH: f32 = 44.0;
const CARD_ACCENT_HEIGHT: f32 = 12.0;
const CARD_ACCENT_RISE: f32 = 7.0;
const CARD_ACCENT_INSET: f32 = 16.0;
const CARD_ICON_SIZE: f32 = 13.0;
const CARD_TAB_TEXT_SIZE: f32 = 12.0;
const CARD_TITLE_SIZE: f32 = 15.0;
const CARD_LABEL_SIZE: f32 = 12.0;
const CARD_VALUE_SIZE: f32 = 22.0;
const CARD_DETAIL_SIZE: f32 = 11.0;
/// A card's detail wraps to this many lines, enough to list four triggers or checks.
const DETAIL_LINES: usize = 3;
const CARD_PADDING: f32 = 12.0;
const CARD_INSET_PADDING: f32 = 8.0;
const CARD_GAP: f32 = 8.0;
/// Board units between grid dots, and the fewest screen pixels kept between them.
const GRID_SPACING: f32 = 24.0;
const GRID_MINIMUM_SPACING: f32 = 14.0;
const GRID_DOT_SIZE: f32 = 2.0;
/// The most grid dots painted in one frame; past it the spacing doubles.
const GRID_MAXIMUM_DOTS: f32 = 6_000.0;
const PALETTE_ICON_SIZE: f32 = 14.0;
/// The page's header and the workflow toolbar above the board.
const BOARD_CHROME_HEIGHT: f32 = 104.0;
const TOOLBAR_HEIGHT: f32 = 44.0;
const TOOLBAR_DIVIDER_HEIGHT: f32 = 20.0;
/// The name keeps this much room before the account and symbol menus crowd it out.
const TOOLBAR_NAME_MINIMUM_WIDTH: f32 = 72.0;
const MISSING_DOT_SIZE: f32 = 6.0;
const HEADER_MENU_WIDTH: f32 = 280.0;
const HEADER_MENU_MAX_HEIGHT: f32 = 360.0;
const RECENT_FIRINGS_SHOWN: usize = 8;
/// Market search rows the inspector lists; typing more narrows them.
const MARKET_RESULTS_SHOWN: usize = 24;
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

/// The width and height around the board that the shown side panels, the header and the
/// toolbar take, for sizing the board before it first paints.
pub(super) fn board_chrome(list_shown: bool, inspector_shown: bool) -> BoardPoint {
    let width = |shown: bool, width: f32| if shown { width } else { 0.0 };
    BoardPoint::new(
        width(list_shown, SIDEBAR_WIDTH) + width(inspector_shown, INSPECTOR_WIDTH),
        BOARD_CHROME_HEIGHT,
    )
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
        .children((page.open.is_none() || !page.list_hidden).then(|| sidebar(ui, page)))
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

fn check_label(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Position => tr!(workflows::BLOCK_IF_POSITION),
        CheckKind::Time => tr!(workflows::BLOCK_IF_TIME),
        CheckKind::Price => tr!(workflows::BLOCK_IF_PRICE),
        CheckKind::PriceRange => tr!(workflows::BLOCK_IF_PRICE_RANGE),
        CheckKind::Weekday => tr!(workflows::BLOCK_IF_WEEKDAY),
        CheckKind::TimeInTrade => tr!(workflows::BLOCK_IF_TIME_IN_TRADE),
        CheckKind::ProfitTicks => tr!(workflows::BLOCK_IF_PROFIT_TICKS),
        CheckKind::Profit => tr!(workflows::BLOCK_IF_PROFIT),
        CheckKind::Pullback => tr!(workflows::BLOCK_IF_PULLBACK),
        CheckKind::DayProfit => tr!(workflows::BLOCK_IF_DAY_PROFIT),
    }
}

/// An If is named by its check, or by how its checks join when it has several.
fn if_title(condition: &ConditionDraft) -> &'static str {
    match (condition.checks.as_slice(), condition.join) {
        ([check], _) => check_label(check.kind()),
        (_, WorkflowJoin::All) => tr!(workflows::BLOCK_IF_ALL),
        (_, WorkflowJoin::Any) => tr!(workflows::BLOCK_IF_ANY),
    }
}

fn block_title(block: &BlockDraft) -> &'static str {
    match block {
        BlockDraft::If { condition, .. } => if_title(condition),
        BlockDraft::Action { .. } => kind_label(block.kind()),
    }
}

fn kind_label(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::If(check) => check_label(check),
        BlockKind::Buy => tr!(workflows::BLOCK_BUY),
        BlockKind::Sell => tr!(workflows::BLOCK_SELL),
        BlockKind::Flatten => tr!(workflows::BLOCK_FLATTEN),
        BlockKind::CancelOrders => tr!(workflows::BLOCK_CANCEL_ORDERS),
        BlockKind::LockAccount => tr!(workflows::BLOCK_LOCK_ACCOUNT),
        BlockKind::Notify => tr!(workflows::BLOCK_NOTIFY),
    }
}

fn kind_icon(kind: BlockKind) -> HugeIcon {
    match kind {
        BlockKind::If(CheckKind::Position) => HugeIcon::DataPanel,
        BlockKind::If(CheckKind::Time) => HugeIcon::TimeSales,
        BlockKind::If(CheckKind::Price) => HugeIcon::Chart,
        BlockKind::If(CheckKind::PriceRange) => HugeIcon::OrderBook,
        BlockKind::If(CheckKind::Weekday) => HugeIcon::Template,
        BlockKind::If(CheckKind::TimeInTrade) => HugeIcon::Loader,
        BlockKind::If(CheckKind::ProfitTicks) => HugeIcon::ArrowRightDouble,
        BlockKind::If(CheckKind::Profit) => HugeIcon::FlagFilled,
        BlockKind::If(CheckKind::Pullback) => HugeIcon::Undo,
        BlockKind::If(CheckKind::DayProfit) => HugeIcon::Flag,
        BlockKind::Buy | BlockKind::Sell => HugeIcon::OrderEntry,
        BlockKind::Flatten => HugeIcon::CloseBold,
        BlockKind::CancelOrders => HugeIcon::CancelCircle,
        BlockKind::LockAccount => HugeIcon::Lock,
        BlockKind::Notify => HugeIcon::Notification,
    }
}

/// The accent of a block, or of the trigger for `None`.
fn kind_accent(kind: Option<BlockKind>, theme: &AerisTheme) -> Hsla {
    let colors = theme.colors;
    gpui_color(match kind {
        None => colors.primary,
        Some(BlockKind::If(_)) => colors.warning,
        Some(BlockKind::Buy) => colors.buy,
        Some(BlockKind::Sell) => colors.sell,
        Some(BlockKind::Flatten | BlockKind::CancelOrders) => colors.indigo,
        Some(BlockKind::LockAccount) => colors.danger,
        Some(BlockKind::Notify) => colors.purple,
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
        .children(page.open.as_ref().map(|open| palette(ui, open)))
}

fn example_icon(example: WorkflowExample) -> HugeIcon {
    match example {
        WorkflowExample::Breakout => HugeIcon::OrderEntry,
        WorkflowExample::SessionDip => HugeIcon::TimeSales,
        WorkflowExample::FlatBeforeClose => HugeIcon::CloseBold,
        WorkflowExample::ExitAndLock => HugeIcon::Lock,
        WorkflowExample::DeadlineUnlessTarget => HugeIcon::Flag,
        WorkflowExample::DayStop => HugeIcon::CancelCircle,
    }
}

/// An example in the empty page: what it does, opened by its button.
fn example_card(ui: Page<'_>, index: usize, example: WorkflowExample) -> Div {
    let open = ui.terminal.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(radius(1.0))
        .border(border_width(ui.theme))
        .border_color(gpui_color(ui.theme.colors.border_secondary))
        .bg(gpui_color(ui.theme.colors.surface_secondary))
        .text_left()
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .child(example_name(example)),
        )
        .child(secondary_text(example_description(example), ui.theme).flex_1())
        .child(
            Button::new(("workflow_example_open", index), ui.theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .icon(header_icon(example_icon(example)))
                .label(tr!(workflows::EXAMPLE_OPEN))
                .aria_label(tr!(
                    workflows::EXAMPLE_OPEN_ARIA,
                    name = example_name(example)
                ))
                .on_click(move |_, window, cx| {
                    open.update(cx, |terminal, terminal_cx| {
                        terminal.open_workflow_example(example, window, terminal_cx);
                    });
                }),
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
        symbol_text(ui.set, Some(&definition.scope)),
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
                .child(new_workflow_button(ui, "workflow_new_empty", full))
                .child(
                    div()
                        .pt_4()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child(tr!(workflows::EXAMPLES_TITLE)),
                )
                .child(
                    div().w_full().grid().grid_cols(2).gap_3().children(
                        WorkflowExample::ALL
                            .iter()
                            .enumerate()
                            .map(|(index, example)| example_card(ui, index, *example)),
                    ),
                ),
        )
}

fn editor(ui: Page<'_>, page: &WorkflowsPage, open: &OpenWorkflow) -> Div {
    let (status, view) = draft_status(&open.draft, ui.set);
    let scales = draft_scales(ui.set, &open.draft);
    let currency_scale = draft_currency_scale(ui.set, &open.draft);
    let problem = open
        .draft
        .build(String::new(), 1, scales, currency_scale)
        .err();
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(toolbar(ui, page, open, status, view.as_ref(), problem))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(board(
                    ui,
                    page,
                    open,
                    problem.and_then(DraftProblem::item),
                    trigger_badge(status, view.as_ref(), ui.theme),
                ))
                .children(
                    (!page.inspector_hidden).then(|| inspector(ui, page, open, view.as_ref())),
                ),
        )
}

/// The toolbar's buttons for the workflow list, at its start, and the inspector, at its end.
fn panel_toggles(ui: Page<'_>, page: &WorkflowsPage) -> (Button, Button) {
    (
        panel_toggle(
            ui,
            "workflow_list_toggle",
            HugeIcon::SidebarLeft,
            !page.list_hidden,
            (
                tr!(workflows::SHOW_LIST_ARIA),
                tr!(workflows::HIDE_LIST_ARIA),
            ),
            TerminalApp::toggle_workflow_list,
        ),
        panel_toggle(
            ui,
            "workflow_inspector_toggle",
            HugeIcon::Settings,
            !page.inspector_hidden,
            (
                tr!(workflows::SHOW_INSPECTOR_ARIA),
                tr!(workflows::HIDE_INSPECTOR_ARIA),
            ),
            TerminalApp::toggle_workflow_inspector,
        ),
    )
}

/// A toolbar button that shows or hides one of the page's side panels; it reads as pressed
/// while the panel is shown.
fn panel_toggle(
    ui: Page<'_>,
    id: &'static str,
    icon: HugeIcon,
    shown: bool,
    (show, hide): (&'static str, &'static str),
    toggle: fn(&mut TerminalApp, &mut Context<TerminalApp>),
) -> Button {
    let terminal = ui.terminal.clone();
    let label = if shown { hide } else { show };
    Button::new(id, ui.theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .selected(shown)
        .icon(header_icon(icon))
        .aria_label(label)
        .tooltip(TooltipSpec::new(label, ui.theme).show_delay(TOOLTIP_OPEN_DELAY))
        .on_click(move |_, _, cx| terminal.update(cx, toggle))
}

/// One row over the board: the workflow's name and state, the account and symbol it trades
/// with their menus, a note only when something needs attention, and its commands.
fn toolbar(
    ui: Page<'_>,
    page: &WorkflowsPage,
    open: &OpenWorkflow,
    status: DraftStatus,
    view: Option<&WorkflowView>,
    problem: Option<DraftProblem>,
) -> Div {
    let theme = ui.theme;
    let colors = theme.colors;
    let draft = &open.draft;
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
    // A missing account or symbol is said by its own menu button, so only other problems
    // and command replies need a note.
    let note = match (&open.status, problem) {
        (Some(PageStatus::Problem(text)), _) => Some((text.clone(), colors.text_danger)),
        (Some(PageStatus::Note(text)), _) => Some((text.clone(), colors.text_positive)),
        (None, Some(DraftProblem::Account | DraftProblem::Symbol) | None) => None,
        (None, Some(problem)) => Some((problem_text(problem), colors.text_warning)),
    };
    let name = draft.name.trim();
    let name = if name.is_empty() {
        tr!(workflows::UNTITLED).to_string()
    } else {
        name.to_string()
    };
    let environment = account_environment(ui.set, draft.account_id.as_ref());
    let (list_toggle, inspector_toggle) = panel_toggles(ui, page);
    // An arm or pause an order ticket sent holds the page's commands too.
    let busy = draft
        .saved
        .as_ref()
        .is_some_and(|saved| page.is_busy(&saved.workflow_id));
    div()
        .flex_none()
        .w_full()
        .h(px(TOOLBAR_HEIGHT))
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b(border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .child(list_toggle)
        .child(
            div()
                .min_w(px(TOOLBAR_NAME_MINIMUM_WIDTH))
                .truncate()
                .text_sm()
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
        .child(toolbar_divider(theme))
        .child(account_menu(ui, open))
        .children(environment.map(|environment| environment_badge(environment, theme)))
        .child(symbol_menu(ui, open))
        .child(div().flex_1().min_w_0().truncate().text_xs().children(
            note.map(|(text, color)| div().text_color(gpui_color(color)).truncate().child(text)),
        ))
        .child(command_buttons(ui, open, status, view, busy))
        .child(inspector_toggle)
}

fn toolbar_divider(theme: &AerisTheme) -> Div {
    div()
        .flex_none()
        .w(border_width(theme))
        .h(px(TOOLBAR_DIVIDER_HEIGHT))
        .bg(gpui_color(theme.colors.border_secondary))
}

/// A header button that opens one of the workflow's menus, with a warning dot while the
/// workflow still needs that choice.
fn menu_button(
    ui: Page<'_>,
    open: &OpenWorkflow,
    (id, menu): (&'static str, HeaderMenu),
    (icon, label, aria): (HugeIcon, String, &'static str),
    missing: bool,
) -> Button {
    let toggle = ui.terminal.clone();
    let button = Button::new(id, ui.theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Sm)
        .open(open.header_menu == Some(menu))
        .aria_label(aria)
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_workflow_header_menu(menu, terminal_cx);
            });
        });
    if missing {
        button.leading(
            div()
                .size(px(MISSING_DOT_SIZE))
                .rounded_full()
                .bg(gpui_color(ui.theme.colors.text_warning)),
        )
    } else {
        button.icon(header_icon(icon))
    }
}

fn header_menu_panel(id: &'static str, theme: &AerisTheme) -> MenuPanel {
    MenuPanel::new(
        id,
        MenuPlacement::Anchored {
            side: MenuSide::Below,
            align: MenuAlign::Start,
        },
        theme,
    )
    .width(px(HEADER_MENU_WIDTH))
    .max_height(px(HEADER_MENU_MAX_HEIGHT))
}

fn header_menu_anchor(
    ui: Page<'_>,
    id: &'static str,
    trigger: Button,
    menu: Option<MenuPanel>,
) -> MenuAnchor {
    let dismiss = ui.terminal.clone();
    MenuAnchor::new(id, trigger)
        .menu(menu)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, TerminalApp::close_workflow_header_menu);
        })
}

/// The account the workflow trades, and a menu of every trading account.
fn account_menu(ui: Page<'_>, open: &OpenWorkflow) -> MenuAnchor {
    let theme = ui.theme;
    let accounts = ui.set.map_or(&[][..], |set| set.accounts.as_slice());
    let label = open.draft.account_id.as_ref().map_or_else(
        || tr!(workflows::CHOOSE_ACCOUNT).to_string(),
        |account_id| account_text(ui.set, account_id),
    );
    let trigger = menu_button(
        ui,
        open,
        ("workflow_account_menu_button", HeaderMenu::Account),
        (HugeIcon::User, label, tr!(workflows::ACCOUNT_MENU_ARIA)),
        open.draft.account_id.is_none(),
    );
    let menu = (open.header_menu == Some(HeaderMenu::Account)).then(|| {
        let mut menu = header_menu_panel("workflow_account_menu", theme);
        if accounts.is_empty() {
            return menu.child(
                MenuRow::compact(
                    "workflow_account_none",
                    tr!(workflows::NO_ACCOUNTS_TITLE),
                    theme,
                )
                .description(tr!(workflows::NO_ACCOUNTS_DETAIL))
                .disabled(true),
            );
        }
        for (index, account) in accounts.iter().enumerate() {
            let account_id = account.id.clone();
            menu = menu.child(
                MenuRow::compact(
                    ("workflow_account_choice", index),
                    account.display_name.clone(),
                    theme,
                )
                .detail(environment_text(account.environment))
                .checked(open.draft.account_id.as_ref() == Some(&account.id))
                .on_click(edit(ui.terminal, move |open| {
                    open.draft.account_id = Some(account_id.clone());
                    open.header_menu = None;
                })),
            );
        }
        menu
    });
    header_menu_anchor(ui, "workflow_account_menu_anchor", trigger, menu)
}

/// The symbol the workflow trades, a menu of the symbols in use and a way to search for any
/// other market.
fn symbol_menu(ui: Page<'_>, open: &OpenWorkflow) -> MenuAnchor {
    let theme = ui.theme;
    let missing = open.draft.scope.is_none();
    let label = if missing {
        tr!(workflows::CHOOSE_SYMBOL).to_string()
    } else {
        draft_symbol(ui.set, &open.draft)
    };
    let trigger = menu_button(
        ui,
        open,
        ("workflow_symbol_menu_button", HeaderMenu::Symbol),
        (HugeIcon::Chart, label, tr!(workflows::SYMBOL_MENU_ARIA)),
        missing,
    );
    let menu = (open.header_menu == Some(HeaderMenu::Symbol)).then(|| {
        let mut menu = header_menu_panel("workflow_symbol_menu", theme)
            .child(
                MenuRow::compact("workflow_symbol_any", tr!(workflows::SYMBOL_ANY), theme)
                    .checked(open.draft.scope == Some(WorkflowScope::AnySymbol))
                    .on_click(edit(ui.terminal, |open| {
                        open.draft.scope = Some(WorkflowScope::AnySymbol);
                        open.header_menu = None;
                    })),
            )
            .separator();
        let options = symbol_options(ui, open);
        let any = !options.is_empty();
        for (index, (instrument_id, symbol)) in options.into_iter().enumerate() {
            let checked = open.draft.instrument_id() == Some(&instrument_id);
            menu = menu.child(
                MenuRow::compact(("workflow_symbol_choice", index), symbol, theme)
                    .checked(checked)
                    .on_click(edit(ui.terminal, move |open| {
                        open.draft.scope = Some(WorkflowScope::Instrument(instrument_id.clone()));
                        open.header_menu = None;
                    })),
            );
        }
        if any {
            menu = menu.separator();
        }
        let search = ui.terminal.clone();
        menu.child(
            MenuRow::compact(
                "workflow_symbol_search",
                tr!(workflows::SEARCH_ALL_MARKETS),
                theme,
            )
            .leading_icon(header_icon(HugeIcon::Search))
            .on_click(move |_, window, cx| {
                search.update(cx, |terminal, terminal_cx| {
                    terminal.search_workflow_markets(window, terminal_cx);
                });
            }),
        )
    });
    header_menu_anchor(ui, "workflow_symbol_menu_anchor", trigger, menu)
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
    busy: bool,
) -> Div {
    let theme = ui.theme;
    let pending = open.pending.is_some() || busy;
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
        .flex_none()
        .flex()
        .flex_col()
        .gap_3()
        .px_4()
        .pt_3()
        .pb_4()
        .border_t(border_width(theme))
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
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .full_width()
        .justify_start()
        .leading(
            Icon::new(kind_icon(kind).path())
                .with_size(px(PALETTE_ICON_SIZE))
                .color(kind_accent(Some(kind), ui.theme)),
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

fn board(
    ui: Page<'_>,
    page: &WorkflowsPage,
    open: &OpenWorkflow,
    problem: Option<BoardItem>,
    badge: Badge,
) -> Stateful<Div> {
    let theme = ui.theme;
    let camera = open.camera;
    let origin = Rc::new(Cell::new(Point::<Pixels>::default()));
    let wires = board_wires(open, theme);
    let faces = blocks(ui, open, badge);
    let cards = faces
        .iter()
        .map(|(item, at, face)| Card {
            at: camera.to_screen(*at),
            accent: face.accent,
            edge: gpui_color(if open.selected == Some(*item) {
                theme.colors.ring_primary
            } else if problem == Some(*item) {
                theme.colors.text_danger
            } else {
                theme.colors.border
            }),
        })
        .collect::<Vec<_>>();
    let card_fill = gpui_color(theme.colors.surface);
    let grid_color = gpui_color(theme.colors.border);
    let edge_width = border_width(theme);
    let capture = open.gesture.is_some();
    let paint_terminal = ui.terminal.clone();
    let prepaint_origin = Rc::clone(&origin);
    let board_size = Rc::clone(&page.board_size);
    let fit = open.fit_pending.then(|| ui.terminal.clone());
    let links = canvas(
        move |bounds, _, cx| {
            prepaint_origin.set(bounds.origin);
            let size = BoardPoint::new(f32::from(bounds.size.width), f32::from(bounds.size.height));
            board_size.set(size);
            if let Some(terminal) = fit {
                terminal.update(cx, |terminal, terminal_cx| {
                    terminal.finish_workflow_fit(size, terminal_cx);
                });
            }
        },
        move |bounds, (), window, _| {
            paint_grid(window, bounds, camera, grid_color);
            for wire in &wires {
                paint_wire(window, bounds.origin, *wire, camera.zoom);
            }
            for card in &cards {
                paint_card(
                    window,
                    bounds.origin,
                    card,
                    camera.zoom,
                    card_fill,
                    edge_width,
                );
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
        .children(
            faces
                .into_iter()
                .map(|(_, at, face)| block_element(camera.to_screen(at), camera.zoom, face, theme)),
        )
        .children(ports(open, theme))
        .child(board_hint(theme))
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

/// How to work the board, along its bottom edge clear of the zoom buttons.
fn board_hint(theme: &AerisTheme) -> Div {
    div()
        .absolute()
        .bottom_3()
        .left_3()
        .right(px(ZOOM_CONTROLS_WIDTH))
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(tr!(workflows::BOARD_HINT, key = ZOOM_KEY))
}

/// The dots behind the blocks, spaced by the zoom and thinned out so a frame paints a
/// bounded number of them.
fn paint_grid(window: &mut Window, bounds: Bounds<Pixels>, camera: BoardCamera, color: Hsla) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let mut spacing = GRID_SPACING * camera.zoom;
    while spacing < GRID_MINIMUM_SPACING
        || (width / spacing) * (height / spacing) > GRID_MAXIMUM_DOTS
    {
        spacing *= 2.0;
    }
    let first = |pan: f32| pan.rem_euclid(spacing);
    let mut y = first(camera.pan.y);
    while y < height {
        let mut x = first(camera.pan.x);
        while x < width {
            let corner = point(
                bounds.origin.x + px(x - GRID_DOT_SIZE / 2.0),
                bounds.origin.y + px(y - GRID_DOT_SIZE / 2.0),
            );
            window.paint_quad(
                fill(
                    Bounds::new(corner, size(px(GRID_DOT_SIZE), px(GRID_DOT_SIZE))),
                    color,
                )
                .corner_radii(px(GRID_DOT_SIZE / 2.0)),
            );
            x += spacing;
        }
        y += spacing;
    }
}

/// Where one block's card is painted, in board-local screen pixels.
struct Card {
    at: BoardPoint,
    accent: Hsla,
    edge: Hsla,
}

/// The card behind a block: a body with a folder tab on its top-left whose right edge
/// leans out, and the block's accent tab tucked behind the top edge on the right.
fn paint_card(
    window: &mut Window,
    origin: Point<Pixels>,
    card: &Card,
    zoom: f32,
    fill_color: Hsla,
    edge_width: Pixels,
) {
    let left = f32::from(origin.x) + card.at.x;
    let top = f32::from(origin.y) + card.at.y;
    let accent = Bounds::new(
        point(
            px(left + (NODE_WIDTH - CARD_ACCENT_INSET - CARD_ACCENT_WIDTH) * zoom),
            px(top + (NODE_TAB_HEIGHT - CARD_ACCENT_RISE) * zoom),
        ),
        size(px(CARD_ACCENT_WIDTH * zoom), px(CARD_ACCENT_HEIGHT * zoom)),
    );
    window.paint_quad(fill(accent, card.accent).corner_radii(radius(zoom / 2.0)));
    let mut body = PathBuilder::fill();
    card_outline(&mut body, left, top, zoom);
    if let Ok(path) = body.build() {
        window.paint_path(path, fill_color);
    }
    let mut edge = PathBuilder::stroke(edge_width);
    card_outline(&mut edge, left, top, zoom);
    if let Ok(path) = edge.build() {
        window.paint_path(path, card.edge);
    }
}

fn card_outline(path: &mut PathBuilder, left: f32, top: f32, zoom: f32) {
    let at = |x: f32, y: f32| point(px(left + x * zoom), px(top + y * zoom));
    let (width, height, tab) = (NODE_WIDTH, NODE_HEIGHT, NODE_TAB_HEIGHT);
    let (radius, tab_radius) = (CARD_RADIUS, CARD_TAB_RADIUS);
    let lean_start = CARD_TAB_WIDTH - CARD_TAB_SLANT;
    let length = CARD_TAB_SLANT.hypot(tab);
    let (along_x, along_y) = (
        CARD_SLANT_ROUNDING * CARD_TAB_SLANT / length,
        CARD_SLANT_ROUNDING * tab / length,
    );
    path.move_to(at(0.0, height - radius));
    path.line_to(at(0.0, tab_radius));
    path.curve_to(at(tab_radius, 0.0), at(0.0, 0.0));
    path.line_to(at(lean_start - CARD_SLANT_ROUNDING, 0.0));
    path.curve_to(at(lean_start + along_x, along_y), at(lean_start, 0.0));
    path.line_to(at(CARD_TAB_WIDTH - along_x, tab - along_y));
    path.curve_to(
        at(CARD_TAB_WIDTH + CARD_SLANT_ROUNDING, tab),
        at(CARD_TAB_WIDTH, tab),
    );
    path.line_to(at(width - radius, tab));
    path.curve_to(at(width, tab + radius), at(width, tab));
    path.line_to(at(width, height - radius));
    path.curve_to(at(width - radius, height), at(width, height));
    path.line_to(at(radius, height));
    path.curve_to(at(0.0, height - radius), at(0.0, height));
    path.close();
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
                .on_click(move |_, window, cx| {
                    reset.update(cx, |terminal, terminal_cx| {
                        terminal.fit_workflow_board(window, terminal_cx);
                    });
                }),
        )
        .child(zoom_button(
            "workflow_zoom_in",
            HugeIcon::Add,
            tr!(workflows::ZOOM_IN_ARIA),
            true,
        ))
}

/// A small colored label on a block, such as the workflow's state on the trigger.
#[derive(Clone)]
struct Badge {
    text: SharedString,
    foreground: Hsla,
    background: Hsla,
}

/// The trigger's badge: whether the draft is saved, and the saved workflow's state.
fn trigger_badge(status: DraftStatus, view: Option<&WorkflowView>, theme: &AerisTheme) -> Badge {
    let colors = theme.colors;
    let badge = |text: SharedString, foreground, background| Badge {
        text,
        foreground: gpui_color(foreground),
        background: gpui_color(background),
    };
    match (status, view) {
        (DraftStatus::New, _) | (_, None) => badge(
            tr!(workflows::STATUS_NEW).into(),
            colors.text_warning,
            colors.warning_subtle,
        ),
        (DraftStatus::Changed, Some(_)) => badge(
            tr!(workflows::STATUS_CHANGED).into(),
            colors.text_warning,
            colors.warning_subtle,
        ),
        (DraftStatus::Saved, Some(view)) => match view.state {
            WorkflowState::Armed => badge(
                tr!(workflows::STATE_ARMED).into(),
                colors.text_positive,
                colors.positive_subtle,
            ),
            WorkflowState::Paused => badge(
                tr!(workflows::STATE_PAUSED).into(),
                colors.text_secondary,
                colors.hover_bg,
            ),
            WorkflowState::Completed => badge(
                tr!(workflows::STATE_COMPLETED).into(),
                colors.text_muted,
                colors.hover_bg,
            ),
            WorkflowState::Expired => badge(
                tr!(workflows::STATE_EXPIRED).into(),
                colors.text_muted,
                colors.hover_bg,
            ),
        },
    }
}

/// What one block shows: its tab, its title, and the panel inside it with the value it
/// is set to.
struct BlockFace {
    icon: HugeIcon,
    category: &'static str,
    title: SharedString,
    label: SharedString,
    badge: Option<Badge>,
    value: Option<SharedString>,
    detail: Option<SharedString>,
    accent: Hsla,
}

fn text_or_unset(text: &str) -> SharedString {
    let text = text.trim();
    if text.is_empty() {
        tr!(workflows::VALUE_UNSET).into()
    } else {
        text.to_string().into()
    }
}

const fn trigger_icon(trigger: &TriggerDraft) -> HugeIcon {
    match trigger {
        TriggerDraft::Price { .. } => HugeIcon::Chart,
        TriggerDraft::Time { .. } => HugeIcon::TimeSales,
        TriggerDraft::Every { .. } => HugeIcon::RefreshV2,
        TriggerDraft::PositionOpened => HugeIcon::Play,
        TriggerDraft::PositionClosed => HugeIcon::CloseBold,
        TriggerDraft::Condition(_) => HugeIcon::CheckIcon,
    }
}

fn trigger_kind_label(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::Rises => tr!(workflows::TRIGGER_CHOICE_ABOVE),
        TriggerKind::Falls => tr!(workflows::TRIGGER_CHOICE_BELOW),
        TriggerKind::Time => tr!(workflows::TRIGGER_CHOICE_TIME),
        TriggerKind::Every => tr!(workflows::TRIGGER_CHOICE_EVERY),
        TriggerKind::Opened => tr!(workflows::TRIGGER_CHOICE_OPENED),
        TriggerKind::Closed => tr!(workflows::TRIGGER_CHOICE_CLOSED),
        TriggerKind::Condition => tr!(workflows::TRIGGER_CHOICE_CONDITION),
    }
}

/// One trigger shows what it waits for; several list each one under "Any trigger".
fn trigger_face(
    triggers: &[TriggerDraft],
    symbol: &str,
    badge: Badge,
    theme: &AerisTheme,
) -> BlockFace {
    let (title, label, value, detail): (_, SharedString, _, _) = match triggers {
        [trigger] => {
            let label = match trigger {
                TriggerDraft::Price { .. }
                | TriggerDraft::PositionOpened
                | TriggerDraft::PositionClosed
                | TriggerDraft::Condition(_) => symbol.to_string().into(),
                TriggerDraft::Time { zone, .. } => zone_text(*zone).into(),
                TriggerDraft::Every { .. } => tr!(workflows::FIELD_MINUTES).into(),
            };
            let detail = match trigger {
                TriggerDraft::Condition(condition) => Some(condition_summary(condition).into()),
                _ => None,
            };
            (
                trigger_kind_label(trigger.kind()),
                label,
                trigger.text().map(|value| text_or_unset(value)),
                detail,
            )
        }
        triggers => (
            tr!(workflows::TRIGGER_ANY),
            symbol.to_string().into(),
            None,
            Some(
                triggers
                    .iter()
                    .map(trigger_summary)
                    .collect::<Vec<_>>()
                    .join(" · ")
                    .into(),
            ),
        ),
    };
    BlockFace {
        icon: triggers.first().map_or(HugeIcon::Chart, trigger_icon),
        category: tr!(workflows::BLOCK_TRIGGER),
        title: title.into(),
        label,
        badge: Some(badge),
        value,
        detail,
        accent: kind_accent(None, theme),
    }
}

/// The label and value a single check shows on its If block.
fn check_face(check: &CheckDraft, symbol: &str) -> (SharedString, SharedString) {
    match check {
        CheckDraft::Position(requirement) => (
            tr!(workflows::FIELD_POSITION).into(),
            position_choice(*requirement).into(),
        ),
        CheckDraft::Between { zone, start, end } => (
            zone_text(*zone).into(),
            format!("{}–{}", text_or_unset(start), text_or_unset(end)).into(),
        ),
        CheckDraft::Price { direction, level } => (
            match direction {
                PriceDirection::Above => tr!(workflows::CHOICE_ABOVE),
                PriceDirection::Below => tr!(workflows::CHOICE_BELOW),
            }
            .into(),
            text_or_unset(level),
        ),
        CheckDraft::PriceRange { low, high } => (
            symbol.to_string().into(),
            format!("{}–{}", text_or_unset(low), text_or_unset(high)).into(),
        ),
        CheckDraft::Weekday { zone, days } => (zone_text(*zone).into(), days_text(*days).into()),
        CheckDraft::TimeInTrade { compare, minutes } => (
            compare_choice(*compare).into(),
            tr!(workflows::VALUE_MINUTES, minutes = text_or_unset(minutes)).into(),
        ),
        CheckDraft::ProfitTicks { compare, ticks } => {
            (compare_choice(*compare).into(), text_or_unset(ticks))
        }
        CheckDraft::Profit { compare, amount } | CheckDraft::DayProfit { compare, amount } => {
            (compare_choice(*compare).into(), text_or_unset(amount))
        }
        CheckDraft::Pullback { ticks } => (
            tr!(workflows::FIELD_PULLBACK_TICKS).into(),
            text_or_unset(ticks),
        ),
    }
}

fn compare_choice(compare: WorkflowCompare) -> &'static str {
    match compare {
        WorkflowCompare::AtLeast => tr!(workflows::CHOICE_AT_LEAST),
        WorkflowCompare::AtMost => tr!(workflows::CHOICE_AT_MOST),
    }
}

/// `symbol` and `account` name what the workflow trades, for the blocks that act on them.
fn node_face(block: &BlockDraft, symbol: &str, account: &str, theme: &AerisTheme) -> BlockFace {
    let kind = block.kind();
    let (label, value, detail): (SharedString, Option<SharedString>, Option<SharedString>) =
        match block {
            BlockDraft::If { condition, .. } => match condition.checks.as_slice() {
                [check] => {
                    let (label, value) = check_face(check, symbol);
                    (label, Some(value), None)
                }
                checks => (
                    tr!(workflows::FIELD_CHECKS).into(),
                    None,
                    Some(
                        checks
                            .iter()
                            .map(check_summary)
                            .collect::<Vec<_>>()
                            .join(" · ")
                            .into(),
                    ),
                ),
            },
            BlockDraft::Action { action, .. } => match action {
                ActionDraft::Enter {
                    quantity,
                    stop,
                    target,
                    ..
                } => (
                    tr!(workflows::FIELD_QUANTITY).into(),
                    Some(text_or_unset(quantity)),
                    Some(if stop.trim().is_empty() && target.trim().is_empty() {
                        tr!(workflows::DETAIL_NO_BRACKET).into()
                    } else {
                        tr!(
                            workflows::DETAIL_BRACKET,
                            stop = stop.trim(),
                            target = target.trim()
                        )
                        .into()
                    }),
                ),
                ActionDraft::Flatten | ActionDraft::CancelOrders => (
                    tr!(workflows::FIELD_SYMBOL).into(),
                    Some(symbol.to_string().into()),
                    Some(action_summary(action).into()),
                ),
                ActionDraft::LockAccount | ActionDraft::Notify => (
                    tr!(workflows::FIELD_ACCOUNT).into(),
                    Some(account.to_string().into()),
                    Some(action_summary(action).into()),
                ),
            },
        };
    BlockFace {
        icon: kind_icon(kind),
        category: match block {
            BlockDraft::If { .. } => tr!(workflows::TAB_CHECK),
            BlockDraft::Action { .. } => tr!(workflows::TAB_ACTION),
        },
        title: block_title(block).into(),
        label,
        badge: None,
        value,
        detail,
        accent: kind_accent(Some(kind), theme),
    }
}

/// The short answer an If on the position looks for.
fn position_choice(requirement: PositionRequirement) -> &'static str {
    match requirement {
        PositionRequirement::Flat => tr!(workflows::CHOICE_FLAT),
        PositionRequirement::Long => tr!(workflows::CHOICE_LONG),
        PositionRequirement::Short => tr!(workflows::CHOICE_SHORT),
        PositionRequirement::Open => tr!(workflows::CHOICE_OPEN),
    }
}

/// Every block in drawing order, the trigger first, with where it sits on the board.
fn blocks(
    ui: Page<'_>,
    open: &OpenWorkflow,
    badge: Badge,
) -> Vec<(BoardItem, BoardPoint, BlockFace)> {
    let draft = &open.draft;
    let symbol = draft_symbol(ui.set, draft);
    let account = draft.account_id.as_ref().map_or_else(
        || tr!(workflows::VALUE_UNSET).to_string(),
        |account_id| account_text(ui.set, account_id),
    );
    std::iter::once((
        BoardItem::Trigger,
        draft.trigger_at,
        trigger_face(&draft.triggers, &symbol, badge, ui.theme),
    ))
    .chain(draft.nodes.iter().map(|node| {
        (
            BoardItem::Node(node.id),
            node.at,
            node_face(&node.block, &symbol, &account, ui.theme),
        )
    }))
    .collect()
}

/// A block's content over its painted card, scaled with the board.
fn block_element(at: BoardPoint, zoom: f32, face: BlockFace, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let scaled = |value: f32| px(value * zoom);
    let title = face.title.clone();
    div()
        .absolute()
        .left(px(at.x))
        .top(px(at.y))
        .w(scaled(NODE_WIDTH))
        .h(scaled(NODE_HEIGHT))
        .flex()
        .flex_col()
        .child(card_tab(&face, zoom, theme))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(scaled(CARD_GAP))
                .px(scaled(CARD_PADDING))
                .pt(scaled(CARD_GAP))
                .pb(scaled(CARD_PADDING))
                .child(
                    div()
                        .truncate()
                        .text_size(scaled(CARD_TITLE_SIZE))
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_default))
                        .child(title),
                )
                .child(card_inset(face, zoom, theme)),
        )
}

/// The folder tab's icon and category.
fn card_tab(face: &BlockFace, zoom: f32, theme: &AerisTheme) -> Div {
    let scaled = |value: f32| px(value * zoom);
    div()
        .flex_none()
        .w(scaled(CARD_TAB_WIDTH - CARD_TAB_SLANT))
        .h(scaled(NODE_TAB_HEIGHT))
        .flex()
        .items_center()
        .gap(scaled(CARD_GAP * 0.75))
        .pl(scaled(CARD_PADDING * 0.85))
        .child(
            Icon::new(face.icon.path())
                .with_size(scaled(CARD_ICON_SIZE))
                .color(face.accent),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(scaled(CARD_TAB_TEXT_SIZE))
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(face.category),
        )
}

/// The panel inside a card: the label and badge over the block's value and detail.
fn card_inset(face: BlockFace, zoom: f32, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let scaled = |value: f32| px(value * zoom);
    let heading = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(scaled(CARD_GAP))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(scaled(CARD_LABEL_SIZE))
                .text_color(gpui_color(colors.text_secondary))
                .child(face.label),
        )
        .when_some(face.badge, |heading, badge| {
            heading.child(
                div()
                    .flex_none()
                    .px(scaled(CARD_INSET_PADDING * 0.75))
                    .rounded(px(f32::from(RadiusToken::Sm.logical_pixels()) * zoom))
                    .bg(badge.background)
                    .text_size(scaled(CARD_DETAIL_SIZE))
                    .text_color(badge.foreground)
                    .child(badge.text),
            )
        });
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(scaled(CARD_GAP / 4.0))
        .px(scaled(CARD_INSET_PADDING + 2.0))
        .py(scaled(CARD_INSET_PADDING))
        .rounded(radius(zoom))
        .bg(gpui_color(colors.surface_secondary))
        .overflow_hidden()
        .child(heading)
        .when_some(face.value, |inset, value| {
            inset.child(
                div()
                    .truncate()
                    .text_size(scaled(CARD_VALUE_SIZE))
                    .line_height(scaled(CARD_VALUE_SIZE * 1.3))
                    .text_color(gpui_color(colors.text_default))
                    .child(value),
            )
        })
        .when_some(face.detail, |inset, detail| {
            inset.child(
                div()
                    .line_clamp(DETAIL_LINES)
                    .text_size(scaled(CARD_DETAIL_SIZE))
                    .text_color(gpui_color(colors.text_muted))
                    .child(detail),
            )
        })
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
        // The label sits outside the card, above the link it names.
        if let Some(label) = port_label(port) {
            let height = PORT_LABEL_SIZE * 1.4 * zoom;
            elements.push(
                div()
                    .absolute()
                    .left(px(
                        center.x + (OUTPUT_PORT_SIZE / 2.0 + PORT_LABEL_GAP) * zoom
                    ))
                    .top(px(center.y - height - OUTPUT_PORT_SIZE / 2.0 * zoom))
                    .w(px(PORT_LABEL_WIDTH * zoom))
                    .h(px(height))
                    .flex()
                    .items_center()
                    .justify_start()
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
        None => workflow_settings(ui, page, open, view),
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
    page: &WorkflowsPage,
    open: &OpenWorkflow,
    view: Option<&WorkflowView>,
) -> Vec<AnyElement> {
    let theme = ui.theme;
    let mut sections = vec![heading(tr!(workflows::INSPECTOR_WORKFLOW), theme).into_any_element()];
    sections.extend(
        input_field(ui, open, DraftField::Name, tr!(workflows::FIELD_NAME))
            .map(IntoElement::into_any_element),
    );
    sections.push(market_search(ui, page).into_any_element());
    sections.push(repeat_settings(ui, open).into_any_element());
    sections.extend(view.map(|view| firings(view, theme).into_any_element()));
    sections.push(secondary_text(tr!(workflows::SAVED_NOTE), theme).into_any_element());
    sections
}

/// The symbols in use and the market last picked by search, for the header's symbol menu.
fn symbol_options(
    ui: Page<'_>,
    open: &OpenWorkflow,
) -> Vec<(aeris_instruments::InstrumentId, SharedString)> {
    let instruments = ui.set.map_or(&[][..], |set| set.instruments.as_slice());
    let mut options = instruments
        .iter()
        .map(|instrument| {
            (
                instrument.instrument_id.clone(),
                SharedString::from(instrument.contract.provenance.display_symbol.clone()),
            )
        })
        .collect::<Vec<_>>();
    if let Some(market) = open.draft.searched.as_ref()
        && !options.iter().any(|(id, _)| id == &market.instrument_id)
    {
        options.push((
            market.instrument_id.clone(),
            market.display_symbol.clone().into(),
        ));
    }
    options
}

/// The search for any market; a pick becomes the workflow's symbol.
fn market_search(ui: Page<'_>, page: &WorkflowsPage) -> Div {
    let search = &page.markets;
    let results = search.entries();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(secondary_text(tr!(workflows::FIELD_SYMBOL), ui.theme))
        .children(
            search
                .input()
                .map(|input| Input::new(input).platform(ui.theme)),
        )
        .children(
            search
                .status()
                .map(|status| secondary_text(status, ui.theme)),
        )
        .children((!results.is_empty()).then(|| market_results(ui, results)))
}

fn market_results(ui: Page<'_>, entries: Vec<InstrumentMenuEntry>) -> Div {
    let theme = ui.theme;
    div()
        .flex()
        .flex_col()
        .p_1()
        .rounded(radius(1.0))
        .border(border_width(theme))
        .border_color(gpui_color(theme.colors.border_secondary))
        .children(
            entries
                .into_iter()
                .take(MARKET_RESULTS_SHOWN)
                .enumerate()
                .map(|(index, entry)| market_row(ui, index, entry)),
        )
}

fn market_row(ui: Page<'_>, index: usize, entry: InstrumentMenuEntry) -> Stateful<Div> {
    let colors = ui.theme.colors;
    let terminal = ui.terminal.clone();
    let action = entry.action;
    let selectable = action != InstrumentMenuAction::Inert;
    div()
        .id(("workflow_market_result", index))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .rounded(radius(1.0))
        .when(selectable, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(gpui_color(colors.hover_bg)))
                .on_click(move |_, _, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.activate_workflow_market(&action, terminal_cx);
                    });
                })
        })
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child(entry.label),
                )
                .children(
                    entry
                        .description
                        .map(|description| secondary_text(description, ui.theme).truncate()),
                ),
        )
        .children(entry.detail.map(|detail| {
            div()
                .flex_none()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(detail)
        }))
}

fn repeat_settings(ui: Page<'_>, open: &OpenWorkflow) -> Div {
    let options = vec![
        (RepeatDraft::Once, tr!(workflows::REPEAT_CHOICE_ONCE).into()),
        (
            RepeatDraft::PerSession,
            tr!(workflows::REPEAT_CHOICE_SESSION).into(),
        ),
        (
            RepeatDraft::Always,
            tr!(workflows::REPEAT_CHOICE_ALWAYS).into(),
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

/// A button that adds a trigger or a check, disabled once the block holds the most.
fn add_button(
    ui: Page<'_>,
    id: &'static str,
    label: &'static str,
    full: bool,
    add: impl Fn(&mut OpenWorkflow) + 'static,
) -> Button {
    Button::new(id, ui.theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Sm)
        .icon(header_icon(HugeIcon::Add))
        .label(label)
        .disabled(full)
        .on_click(edit(ui.terminal, add))
}

/// The settings of one of a block's triggers or checks. While there are several, each is
/// headed by its number and can be removed; the section's identity keeps its controls apart
/// from those of its siblings.
fn numbered_section(
    ui: Page<'_>,
    id: (&'static str, usize),
    title: Option<String>,
    remove: impl Fn(&mut OpenWorkflow) + 'static,
) -> Stateful<Div> {
    let theme = ui.theme;
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_2()
        .when_some(title, |section, title| {
            section
                .pt_3()
                .border_t(border_width(theme))
                .border_color(gpui_color(theme.colors.border_secondary))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(heading(title, theme))
                        .child(
                            Button::new("workflow_remove_part", theme)
                                .variant(ButtonVariant::Ghost)
                                .button_size(ButtonSize::Sm)
                                .danger_on_hover()
                                .icon(header_icon(HugeIcon::Trash))
                                .label(tr!(workflows::REMOVE))
                                .on_click(edit(ui.terminal, remove)),
                        ),
                )
        })
}

fn trigger_settings(ui: Page<'_>, open: &OpenWorkflow) -> Vec<AnyElement> {
    let theme = ui.theme;
    let triggers = &open.draft.triggers;
    let several = triggers.len() > 1;
    let mut sections = vec![heading(tr!(workflows::BLOCK_TRIGGER), theme).into_any_element()];
    sections.extend(triggers.iter().enumerate().map(|(index, trigger)| {
        trigger_section(ui, open, index, trigger, several).into_any_element()
    }));
    sections.push(
        add_button(
            ui,
            "workflow_add_trigger",
            tr!(workflows::ADD_TRIGGER),
            triggers.len() >= MAXIMUM_WORKFLOW_TRIGGERS,
            |open| {
                open.draft.add_trigger();
            },
        )
        .into_any_element(),
    );
    sections.push(
        secondary_text(
            tr!(
                workflows::TRIGGERS_HINT,
                maximum = MAXIMUM_WORKFLOW_TRIGGERS
            ),
            theme,
        )
        .into_any_element(),
    );
    sections
}

fn trigger_section(
    ui: Page<'_>,
    open: &OpenWorkflow,
    index: usize,
    trigger: &TriggerDraft,
    several: bool,
) -> Stateful<Div> {
    let theme = ui.theme;
    let options = TriggerKind::ALL
        .iter()
        .map(|kind| (*kind, SharedString::from(trigger_kind_label(*kind))))
        .collect();
    let (label, hint) = match trigger {
        TriggerDraft::Price { .. } => (
            tr!(workflows::FIELD_LEVEL),
            tr!(workflows::TRIGGER_PRICE_HINT),
        ),
        TriggerDraft::Time { .. } => (
            tr!(workflows::FIELD_TIME),
            tr!(workflows::TRIGGER_TIME_HINT),
        ),
        TriggerDraft::Every { .. } => (
            tr!(workflows::FIELD_MINUTES),
            tr!(workflows::TRIGGER_EVERY_HINT),
        ),
        TriggerDraft::PositionOpened => ("", tr!(workflows::TRIGGER_OPENED_HINT)),
        TriggerDraft::PositionClosed => ("", tr!(workflows::TRIGGER_CLOSED_HINT)),
        TriggerDraft::Condition(_) => ("", tr!(workflows::TRIGGER_CONDITION_HINT)),
    };
    numbered_section(
        ui,
        ("workflow_trigger_section", index),
        several.then(|| tr!(workflows::TRIGGER_NUMBER, number = index + 1)),
        move |open| open.draft.remove_trigger(index),
    )
    .child(field(
        tr!(workflows::FIELD_TRIGGER),
        choices(
            ui,
            "workflow_trigger",
            options,
            Some(&trigger.kind()),
            move |open, kind| open.draft.set_trigger_kind(index, kind),
        ),
        theme,
    ))
    .children(
        trigger
            .field(index)
            .and_then(|draft_field| input_field(ui, open, draft_field, label)),
    )
    .children(match trigger {
        TriggerDraft::Time { zone, .. } => Some(zone_choices(
            ui,
            "workflow_trigger_zone",
            *zone,
            move |open, zone| {
                if let Some(TriggerDraft::Time { zone: current, .. }) =
                    open.draft.trigger_mut(index)
                {
                    *current = zone;
                }
            },
        )),
        TriggerDraft::Price { .. }
        | TriggerDraft::Every { .. }
        | TriggerDraft::PositionOpened
        | TriggerDraft::PositionClosed
        | TriggerDraft::Condition(_) => None,
    })
    .children(match trigger {
        TriggerDraft::Condition(condition) => {
            condition_settings(ui, open, ConditionOwner::Trigger(index), condition)
        }
        _ => Vec::new(),
    })
    .child(secondary_text(hint, theme))
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
    let mut sections = vec![heading(block_title(&node.block), theme).into_any_element()];
    match &node.block {
        BlockDraft::If { condition, .. } => {
            sections.extend(condition_settings(
                ui,
                open,
                ConditionOwner::Node(id),
                condition,
            ));
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

/// Every check of an If or a condition trigger, how they join while there are several, and
/// the button that adds one.
fn condition_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    owner: ConditionOwner,
    condition: &ConditionDraft,
) -> Vec<AnyElement> {
    let several = condition.checks.len() > 1;
    let mut sections = condition
        .checks
        .iter()
        .enumerate()
        .map(|(index, check)| {
            check_section(ui, open, CheckRef { owner, index }, check, several).into_any_element()
        })
        .collect::<Vec<_>>();
    if several {
        let options = vec![
            (WorkflowJoin::All, tr!(workflows::JOIN_ALL).into()),
            (WorkflowJoin::Any, tr!(workflows::JOIN_ANY).into()),
        ];
        sections.push(
            field(
                tr!(workflows::FIELD_JOIN),
                choices(
                    ui,
                    "workflow_join",
                    options,
                    Some(&condition.join),
                    move |open, join| {
                        if let Some(condition) = open.draft.condition_mut(owner) {
                            condition.join = join;
                        }
                    },
                ),
                ui.theme,
            )
            .into_any_element(),
        );
    }
    sections.push(
        add_button(
            ui,
            "workflow_add_check",
            tr!(workflows::ADD_CHECK),
            condition.checks.len() >= MAXIMUM_WORKFLOW_CHECKS,
            move |open| {
                open.draft.add_check(owner);
            },
        )
        .into_any_element(),
    );
    sections
}

fn check_section(
    ui: Page<'_>,
    open: &OpenWorkflow,
    check: CheckRef,
    draft: &CheckDraft,
    several: bool,
) -> Stateful<Div> {
    let options = CheckKind::ALL
        .iter()
        .map(|kind| (*kind, SharedString::from(check_label(*kind))))
        .collect();
    numbered_section(
        ui,
        ("workflow_check_section", check.index),
        several.then(|| tr!(workflows::CHECK_NUMBER, number = check.index + 1)),
        move |open| open.draft.remove_check(check),
    )
    .child(field(
        tr!(workflows::FIELD_CHECK),
        choices(
            ui,
            "workflow_check",
            options,
            Some(&draft.kind()),
            move |open, kind| open.draft.set_check_kind(check, kind),
        ),
        ui.theme,
    ))
    .children(check_settings(ui, open, check, draft))
}

fn check_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    check: CheckRef,
    draft: &CheckDraft,
) -> Vec<AnyElement> {
    let set_zone = move |open: &mut OpenWorkflow, zone| {
        if let Some(
            CheckDraft::Between { zone: current, .. } | CheckDraft::Weekday { zone: current, .. },
        ) = open.draft.check_mut(check)
        {
            *current = zone;
        }
    };
    match draft {
        CheckDraft::Position(requirement) => position_settings(ui, check, *requirement),
        CheckDraft::Between { zone, .. } => [
            input_field(
                ui,
                open,
                DraftField::WindowStart(check),
                tr!(workflows::FIELD_START),
            ),
            input_field(
                ui,
                open,
                DraftField::WindowEnd(check),
                tr!(workflows::FIELD_END),
            ),
            Some(zone_choices(ui, "workflow_window_zone", *zone, set_zone)),
        ]
        .into_iter()
        .flatten()
        .map(IntoElement::into_any_element)
        .collect(),
        CheckDraft::Price { direction, .. } => price_settings(ui, open, check, *direction),
        CheckDraft::PriceRange { .. } => range_settings(ui, open, check),
        CheckDraft::Weekday { zone, days } => vec![
            day_choices(ui, check, *days).into_any_element(),
            zone_choices(ui, "workflow_weekday_zone", *zone, set_zone).into_any_element(),
        ],
        CheckDraft::TimeInTrade { compare, .. } => trade_settings(
            ui,
            open,
            check,
            Some(*compare),
            DraftField::TradeMinutes(check),
            tr!(workflows::FIELD_TRADE_MINUTES),
        ),
        CheckDraft::ProfitTicks { compare, .. } => trade_settings(
            ui,
            open,
            check,
            Some(*compare),
            DraftField::Ticks(check),
            tr!(workflows::FIELD_PROFIT_TICKS),
        ),
        CheckDraft::Profit { compare, .. } => trade_settings(
            ui,
            open,
            check,
            Some(*compare),
            DraftField::Amount(check),
            tr!(workflows::FIELD_PROFIT_AMOUNT),
        ),
        CheckDraft::Pullback { .. } => trade_settings(
            ui,
            open,
            check,
            None,
            DraftField::Ticks(check),
            tr!(workflows::FIELD_PULLBACK_TICKS),
        ),
        CheckDraft::DayProfit { compare, .. } => {
            let mut settings = compare_and_value(
                ui,
                open,
                check,
                Some(*compare),
                DraftField::Amount(check),
                tr!(workflows::FIELD_DAY_PROFIT_AMOUNT),
            );
            settings
                .push(secondary_text(tr!(workflows::DAY_PROFIT_HINT), ui.theme).into_any_element());
            settings
        }
    }
}

fn range_settings(ui: Page<'_>, open: &OpenWorkflow, check: CheckRef) -> Vec<AnyElement> {
    let inputs = [
        input_field(
            ui,
            open,
            DraftField::RangeLow(check),
            tr!(workflows::FIELD_LOW),
        ),
        input_field(
            ui,
            open,
            DraftField::RangeHigh(check),
            tr!(workflows::FIELD_HIGH),
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
        secondary_text(tr!(workflows::RANGE_HINT), ui.theme).into_any_element(),
    ]
}

/// A check on the open trade: how it compares, when it compares, its one value, and that it
/// never holds while flat.
fn trade_settings(
    ui: Page<'_>,
    open: &OpenWorkflow,
    check: CheckRef,
    compare: Option<WorkflowCompare>,
    value: DraftField,
    label: &'static str,
) -> Vec<AnyElement> {
    let mut settings = compare_and_value(ui, open, check, compare, value, label);
    settings.push(secondary_text(tr!(workflows::TRADE_CHECK_HINT), ui.theme).into_any_element());
    settings
}

/// How a check compares, when it compares, and its one value.
fn compare_and_value(
    ui: Page<'_>,
    open: &OpenWorkflow,
    check: CheckRef,
    compare: Option<WorkflowCompare>,
    value: DraftField,
    label: &'static str,
) -> Vec<AnyElement> {
    let compare = compare.map(|compare| {
        let options = vec![
            (
                WorkflowCompare::AtLeast,
                compare_choice(WorkflowCompare::AtLeast).into(),
            ),
            (
                WorkflowCompare::AtMost,
                compare_choice(WorkflowCompare::AtMost).into(),
            ),
        ];
        field(
            tr!(workflows::FIELD_COMPARE),
            choices(
                ui,
                "workflow_compare",
                options,
                Some(&compare),
                move |open, compare| {
                    if let Some(check) = open.draft.check_mut(check) {
                        check.set_compare(compare);
                    }
                },
            ),
            ui.theme,
        )
    });
    [compare, input_field(ui, open, value, label)]
        .into_iter()
        .flatten()
        .map(IntoElement::into_any_element)
        .collect()
}

/// One toggle per day of the week; the last chosen day cannot be turned off.
fn day_choices(ui: Page<'_>, check: CheckRef, days: WorkflowWeekdays) -> Div {
    field(
        tr!(workflows::FIELD_DAYS),
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children((0..7_u8).map(|day| {
                Button::new(("workflow_day", usize::from(day)), ui.theme)
                    .variant(ButtonVariant::Outline)
                    .button_size(ButtonSize::Sm)
                    .selected(days.contains(day))
                    .label(day_label(day))
                    .on_click(edit(ui.terminal, move |open| {
                        if let Some(check) = open.draft.check_mut(check) {
                            check.toggle_day(day);
                        }
                    }))
            })),
        ui.theme,
    )
}

fn position_settings(
    ui: Page<'_>,
    check: CheckRef,
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
                    if let Some(CheckDraft::Position(current)) = open.draft.check_mut(check) {
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
    check: CheckRef,
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
                    if let Some(CheckDraft::Price {
                        direction: current, ..
                    }) = open.draft.check_mut(check)
                    {
                        *current = direction;
                    }
                },
            ),
            ui.theme,
        )),
        input_field(
            ui,
            open,
            DraftField::Level(check),
            tr!(workflows::FIELD_LEVEL),
        ),
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
        ActionDraft::Notify => {
            vec![secondary_text(tr!(workflows::NOTIFY_HINT), theme).into_any_element()]
        }
        ActionDraft::Flatten | ActionDraft::CancelOrders => Vec::new(),
    }
}
