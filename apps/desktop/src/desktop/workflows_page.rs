//! The Workflows page, one of the header's pages: the owner's workflows in a list beside a
//! board where the user wires a trigger to If and action blocks, and an inspector for the
//! selected block. The desktop only edits a draft and sends the definition the user saved;
//! the trading owner validates it, keeps it paused until it is armed and runs it on its own
//! quotes and clock.

use super::market_screener::{AppView, capture_time_label};
use super::workflow_board::{
    ActionDraft, BoardCamera, BoardHit, BoardItem, BoardPoint, CheckDraft, DraftField,
    DraftProblem, InstrumentScales, LinkRefusal, OutputPort, RepeatDraft, SavedRevision,
    SearchedMarket, TriggerDraft, WorkflowDraft, WorkflowExample,
};
use super::workflow_canvas::board_chrome;
use super::workflow_market_search::{MarketSearchNotice, WorkflowMarketSearch};
use super::*;
use aeris_localization::{tr, workflows};
use aeris_trading::{AccountEnvironment, OrderSide, TradingAccountId};
use aeris_trading_runtime::{
    MAXIMUM_WORKFLOW_CHECKS, MAXIMUM_WORKFLOW_NODES, MAXIMUM_WORKFLOW_TRIGGERS, MAXIMUM_WORKFLOWS,
    MINUTES_PER_DAY, PositionRequirement, PriceDirection, TradingInstrument, TradingService,
    WorkflowActionResult, WorkflowBlocker, WorkflowError, WorkflowFiringOutcome, WorkflowState,
    WorkflowStepOutcome, WorkflowTimeZone, WorkflowView, WorkflowWeekdays, WorkingSet,
};

const ZOOM_STEP: f32 = 1.2;
const WHEEL_ZOOM_RATE: f32 = 0.002;

/// The Workflows page: the open draft, and the published state it last rendered.
#[derive(Default)]
pub(super) struct WorkflowsPage {
    pub(super) open: Option<OpenWorkflow>,
    observed: Option<ObservedWorkflows>,
    next_command: u64,
    pub(super) list_scroll: ScrollHandle,
    pub(super) inspector_scroll: ScrollHandle,
    /// The workflow list and the inspector fold away to give the board the width; the list
    /// still shows while no workflow is open, since it is then the whole page.
    pub(super) list_hidden: bool,
    pub(super) inspector_hidden: bool,
    /// The board's size in pixels when it last painted; zero before it first did.
    pub(super) board_size: Rc<Cell<BoardPoint>>,
    /// The inspector's search for the workflow's market.
    pub(super) markets: WorkflowMarketSearch,
}

/// What the page shows from a publication; one that changes none of it leaves the page as
/// it is.
#[derive(PartialEq)]
struct ObservedWorkflows {
    workflows: Vec<WorkflowView>,
    accounts: Vec<(TradingAccountId, String)>,
    instruments: Vec<aeris_instruments::InstrumentId>,
}

impl ObservedWorkflows {
    fn of(set: &WorkingSet) -> Self {
        Self {
            workflows: set.workflows.clone(),
            accounts: set
                .accounts
                .iter()
                .map(|account| (account.id.clone(), account.display_name.clone()))
                .collect(),
            instruments: set
                .instruments
                .iter()
                .map(|instrument| instrument.instrument_id.clone())
                .collect(),
        }
    }
}

pub(super) struct OpenWorkflow {
    pub(super) draft: WorkflowDraft,
    pub(super) selected: Option<BoardItem>,
    /// The inspector's text inputs, one per field of the selection.
    pub(super) inputs: Vec<(DraftField, Entity<InputState>)>,
    pub(super) camera: BoardCamera,
    pub(super) gesture: Option<BoardGesture>,
    pub(super) status: Option<PageStatus>,
    /// The user command in flight; its reply is the only one that updates the draft.
    pub(super) pending: Option<u64>,
    pub(super) confirm_delete: bool,
}

impl OpenWorkflow {
    fn new(draft: WorkflowDraft) -> Self {
        Self {
            draft,
            selected: None,
            inputs: Vec::new(),
            camera: BoardCamera::default(),
            gesture: None,
            status: None,
            pending: None,
            confirm_delete: false,
        }
    }

    pub(super) fn input(&self, field: DraftField) -> Option<&Entity<InputState>> {
        self.inputs
            .iter()
            .find_map(|(input_field, input)| (*input_field == field).then_some(input))
    }

    /// The text fields the inspector shows for the current selection.
    fn fields(&self) -> Vec<DraftField> {
        match self.selected {
            None => {
                let mut fields = vec![DraftField::Name];
                if self.draft.repeat == RepeatDraft::PerSession {
                    fields.extend([DraftField::SessionMaximum, DraftField::Cooldown]);
                }
                fields
            }
            Some(BoardItem::Trigger) => self
                .draft
                .triggers
                .iter()
                .enumerate()
                .map(|(index, trigger)| trigger.field(index))
                .collect(),
            Some(BoardItem::Node(id)) => self
                .draft
                .node(id)
                .map(super::workflow_board::NodeDraft::fields)
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PageStatus {
    Problem(String),
    Note(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum BoardGesture {
    Move {
        item: BoardItem,
        grab: BoardPoint,
        moved: bool,
    },
    Connect {
        port: OutputPort,
        cursor: BoardPoint,
    },
    Pan {
        last: BoardPoint,
    },
}

/// A workflow command's reply, worded once it reaches the page.
enum WorkflowReply {
    Saved(Box<WorkflowView>),
    LayoutSaved,
    Armed(String),
    Paused(String),
    Deleted { workflow_id: String, name: String },
}

/// How the open draft relates to the owner's accepted revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DraftStatus {
    New,
    /// The definition is the accepted one; only block positions may differ.
    Saved,
    Changed,
}

pub(super) fn instrument<'a>(
    set: Option<&'a WorkingSet>,
    instrument_id: Option<&aeris_instruments::InstrumentId>,
) -> Option<&'a TradingInstrument> {
    let instrument_id = instrument_id?;
    set?.instruments
        .iter()
        .find(|instrument| &instrument.instrument_id == instrument_id)
}

pub(super) fn instrument_scales(
    instrument: Option<&TradingInstrument>,
) -> Option<InstrumentScales> {
    instrument.map(|instrument| InstrumentScales {
        price: instrument.price_scale,
        quantity: instrument.quantity_scale,
    })
}

pub(super) fn symbol_text(
    set: Option<&WorkingSet>,
    instrument_id: Option<&aeris_instruments::InstrumentId>,
) -> String {
    match (instrument(set, instrument_id), instrument_id) {
        (Some(instrument), _) => instrument.contract.provenance.display_symbol.clone(),
        (None, Some(instrument_id)) => instrument_id.as_str().to_string(),
        (None, None) => tr!(workflows::SYMBOL_UNSET).to_string(),
    }
}

/// The scales of the draft's market, from the working set or the market picked by search.
pub(super) fn draft_scales(
    set: Option<&WorkingSet>,
    draft: &WorkflowDraft,
) -> Option<InstrumentScales> {
    instrument_scales(instrument(set, draft.instrument_id.as_ref()))
        .or_else(|| draft.searched_market().map(|market| market.scales))
}

pub(super) fn draft_symbol(set: Option<&WorkingSet>, draft: &WorkflowDraft) -> String {
    match (
        instrument(set, draft.instrument_id.as_ref()),
        draft.searched_market(),
    ) {
        (None, Some(market)) => market.display_symbol.clone(),
        _ => symbol_text(set, draft.instrument_id.as_ref()),
    }
}

pub(super) fn account_text(set: Option<&WorkingSet>, account_id: &TradingAccountId) -> String {
    set.and_then(|set| {
        set.accounts
            .iter()
            .find(|account| &account.id == account_id)
    })
    .map_or_else(
        || account_id.as_str().to_string(),
        |account| account.display_name.clone(),
    )
}

pub(super) fn account_environment(
    set: Option<&WorkingSet>,
    account_id: Option<&TradingAccountId>,
) -> Option<AccountEnvironment> {
    let account_id = account_id?;
    set?.accounts
        .iter()
        .find(|account| &account.id == account_id)
        .map(|account| account.environment)
}

pub(super) fn environment_text(environment: AccountEnvironment) -> &'static str {
    match environment {
        AccountEnvironment::Simulated => tr!(workflows::ENVIRONMENT_PRACTICE),
        AccountEnvironment::Demo => tr!(workflows::ENVIRONMENT_DEMO),
        AccountEnvironment::Live => tr!(workflows::ENVIRONMENT_LIVE),
    }
}

/// The saved view of the open draft, and whether the draft still matches it.
pub(super) fn draft_status(
    draft: &WorkflowDraft,
    set: Option<&WorkingSet>,
) -> (DraftStatus, Option<WorkflowView>) {
    let Some(saved) = draft.saved.as_ref() else {
        return (DraftStatus::New, None);
    };
    let view = set.and_then(|set| {
        set.workflows
            .iter()
            .find(|view| view.definition.workflow_id == saved.workflow_id)
            .cloned()
    });
    let scales = draft_scales(set, draft);
    let unchanged = view.as_ref().is_some_and(|view| {
        draft
            .build(saved.workflow_id.clone(), saved.revision, scales)
            .is_ok_and(|(definition, _)| definition == view.definition)
    });
    let status = if unchanged {
        DraftStatus::Saved
    } else {
        DraftStatus::Changed
    };
    (status, view)
}

pub(super) fn zone_text(zone: WorkflowTimeZone) -> &'static str {
    match zone {
        WorkflowTimeZone::Chicago => tr!(workflows::ZONE_CHICAGO),
        WorkflowTimeZone::Utc => tr!(workflows::ZONE_UTC),
    }
}

pub(super) fn position_text(requirement: PositionRequirement) -> &'static str {
    match requirement {
        PositionRequirement::Flat => tr!(workflows::CONDITION_FLAT),
        PositionRequirement::Long => tr!(workflows::CONDITION_LONG),
        PositionRequirement::Short => tr!(workflows::CONDITION_SHORT),
        PositionRequirement::Open => tr!(workflows::CONDITION_OPEN),
    }
}

pub(super) fn problem_text(problem: DraftProblem) -> String {
    match problem {
        DraftProblem::Name => tr!(workflows::PROBLEM_NAME).to_string(),
        DraftProblem::Account => tr!(workflows::PROBLEM_ACCOUNT).to_string(),
        DraftProblem::Symbol => tr!(workflows::PROBLEM_SYMBOL).to_string(),
        DraftProblem::TriggerLevel { decimals } | DraftProblem::Level { decimals, .. } => {
            tr!(workflows::PROBLEM_LEVEL, decimals = decimals)
        }
        DraftProblem::Triggers => {
            tr!(
                workflows::PROBLEM_TRIGGERS,
                maximum = MAXIMUM_WORKFLOW_TRIGGERS
            )
        }
        DraftProblem::TriggerTime => tr!(workflows::PROBLEM_TIME).to_string(),
        DraftProblem::TriggerMinutes => tr!(workflows::PROBLEM_MINUTES, maximum = MINUTES_PER_DAY),
        DraftProblem::Checks(_) => {
            tr!(workflows::PROBLEM_CHECKS, maximum = MAXIMUM_WORKFLOW_CHECKS)
        }
        DraftProblem::Window(_) => tr!(workflows::PROBLEM_WINDOW).to_string(),
        DraftProblem::Range { decimals, .. } => tr!(workflows::PROBLEM_RANGE, decimals = decimals),
        DraftProblem::NoStart => tr!(workflows::PROBLEM_NO_START).to_string(),
        DraftProblem::Unconnected(_) => tr!(workflows::PROBLEM_UNCONNECTED).to_string(),
        DraftProblem::NoAction => tr!(workflows::PROBLEM_NO_ACTION).to_string(),
        DraftProblem::Quantity(_) => tr!(workflows::PROBLEM_QUANTITY).to_string(),
        DraftProblem::Protection(_) => tr!(workflows::PROBLEM_PROTECTION).to_string(),
        DraftProblem::SessionMaximum => tr!(workflows::PROBLEM_SESSION_MAXIMUM).to_string(),
        DraftProblem::Cooldown => tr!(workflows::PROBLEM_COOLDOWN).to_string(),
    }
}

pub(super) fn blocker_text(blocker: WorkflowBlocker) -> &'static str {
    match blocker {
        WorkflowBlocker::AccountMissing => tr!(workflows::BLOCKER_ACCOUNT_MISSING),
        WorkflowBlocker::InstrumentTermsMissing => tr!(workflows::BLOCKER_TERMS_MISSING),
        WorkflowBlocker::AccountLocked => tr!(workflows::BLOCKER_ACCOUNT_LOCKED),
        WorkflowBlocker::BrokerUnreachable => tr!(workflows::BLOCKER_BROKER_UNREACHABLE),
        WorkflowBlocker::QuoteUnavailable => tr!(workflows::BLOCKER_QUOTE_UNAVAILABLE),
    }
}

fn blockers_text(blockers: &[WorkflowBlocker]) -> String {
    blockers
        .iter()
        .map(|blocker| blocker_text(*blocker))
        .collect::<Vec<_>>()
        .join(", ")
}

fn action_result_text(result: &WorkflowActionResult) -> String {
    match result {
        WorkflowActionResult::Placed { client_order_id } => {
            tr!(workflows::OUTCOME_PLACED, id = client_order_id.as_str())
        }
        WorkflowActionResult::Flattened => tr!(workflows::OUTCOME_FLATTENED).to_string(),
        WorkflowActionResult::Cancelled { orders } => {
            tr!(workflows::OUTCOME_CANCELLED, orders = orders)
        }
        WorkflowActionResult::Locked => tr!(workflows::OUTCOME_LOCKED).to_string(),
        WorkflowActionResult::Refused { detail } => {
            tr!(workflows::OUTCOME_REFUSED, detail = detail)
        }
    }
}

/// Each block a firing passed, in order: the answer of each If and the result of each
/// action.
pub(super) fn outcome_text(outcome: &WorkflowFiringOutcome) -> String {
    match outcome {
        WorkflowFiringOutcome::Executed(steps) => steps
            .iter()
            .map(|step| match &step.outcome {
                WorkflowStepOutcome::Branch(true) => tr!(workflows::OUTCOME_YES).to_string(),
                WorkflowStepOutcome::Branch(false) => tr!(workflows::OUTCOME_NO).to_string(),
                WorkflowStepOutcome::Action(result) => action_result_text(result),
            })
            .collect::<Vec<_>>()
            .join(" → "),
        WorkflowFiringOutcome::Blocked(blockers) => tr!(
            workflows::OUTCOME_BLOCKED,
            reasons = blockers_text(blockers)
        ),
        WorkflowFiringOutcome::Interrupted => tr!(workflows::OUTCOME_INTERRUPTED).to_string(),
    }
}

pub(super) fn firing_time_text(unix_nanos: i64) -> String {
    capture_time_label(unix_nanos.div_euclid(1_000_000))
}

pub(super) fn state_text(view: &WorkflowView) -> String {
    match view.state {
        WorkflowState::Paused => tr!(workflows::STATE_PAUSED).to_string(),
        WorkflowState::Armed if view.blockers.is_empty() => tr!(workflows::STATE_ARMED).to_string(),
        WorkflowState::Armed => tr!(
            workflows::STATE_ARMED_WAITING,
            reasons = blockers_text(&view.blockers)
        ),
        WorkflowState::Completed => tr!(workflows::STATE_COMPLETED).to_string(),
        WorkflowState::Expired => tr!(workflows::STATE_EXPIRED).to_string(),
    }
}

pub(super) fn state_color(view: &WorkflowView, theme: &AerisTheme) -> Hsla {
    let colors = theme.colors;
    gpui_color(match view.state {
        WorkflowState::Armed if view.blockers.is_empty() => colors.text_positive,
        WorkflowState::Armed => colors.text_warning,
        WorkflowState::Paused | WorkflowState::Completed | WorkflowState::Expired => {
            colors.text_secondary
        }
    })
}

/// A refused workflow command, worded for the page and the trading feedback line.
fn workflow_error_text(error: &WorkflowError) -> String {
    match error {
        WorkflowError::Invalid(_) => tr!(workflows::ERROR_INVALID).to_string(),
        WorkflowError::TooManyWorkflows => {
            tr!(workflows::ERROR_TOO_MANY, maximum = MAXIMUM_WORKFLOWS)
        }
        WorkflowError::UnknownWorkflow => tr!(workflows::ERROR_UNKNOWN).to_string(),
        WorkflowError::UnknownAccount => tr!(workflows::ERROR_UNKNOWN_ACCOUNT).to_string(),
        WorkflowError::UnknownInstrument => tr!(workflows::ERROR_UNKNOWN_INSTRUMENT).to_string(),
        WorkflowError::RevisionMustAdvance { .. } | WorkflowError::StaleRevision { .. } => {
            tr!(workflows::ERROR_REVISION).to_string()
        }
        WorkflowError::Completed => tr!(workflows::ERROR_COMPLETED).to_string(),
        WorkflowError::Expired => tr!(workflows::ERROR_EXPIRED).to_string(),
        WorkflowError::NotReady(blockers) => tr!(
            workflows::ERROR_NOT_READY,
            reasons = blockers_text(blockers)
        ),
        WorkflowError::Owner(detail) => tr!(workflows::ERROR_OWNER, detail = detail),
    }
}

impl TerminalApp {
    /// Shows the Workflows page; workspaces and their market demand stay mounted behind it.
    pub(super) fn show_workflows_page(&mut self, cx: &mut Context<Self>) {
        self.pages.view = AppView::Workflows;
        self.pages.workflows.observed = None;
        self.pages.screener.stop_refreshing();
        self.close_chart_context_menu(cx);
        self.close_chart_settings_menu(cx);
        self.close_drawing_tool_menu(cx);
        cx.notify();
    }

    /// Re-renders the shown page when a publication changed a workflow, account or symbol.
    pub(super) fn observe_workflow_publication(&mut self, cx: &mut Context<Self>) {
        if self.pages.view != AppView::Workflows {
            return;
        }
        let Some(observed) = self
            .services
            .trading_store()
            .read(cx)
            .current()
            .map(ObservedWorkflows::of)
        else {
            return;
        };
        if self.pages.workflows.observed.as_ref() != Some(&observed) {
            self.pages.workflows.observed = Some(observed);
            cx.notify();
        }
    }

    /// Opens a new draft on the active chart's symbol and the first practice account.
    pub(super) fn open_new_workflow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (account_id, instrument_id) = self.new_workflow_target(cx);
        let draft = WorkflowDraft::new(
            tr!(workflows::UNTITLED).to_string(),
            account_id,
            instrument_id,
        );
        self.open_workflow_draft(draft, None, window, cx);
    }

    /// Opens an example as an unsaved draft, with its trigger selected so the price it
    /// still needs is the first field shown.
    pub(super) fn open_workflow_example(
        &mut self,
        example: WorkflowExample,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (account_id, instrument_id) = self.new_workflow_target(cx);
        let draft = example.draft(example_name(example).to_string(), account_id, instrument_id);
        self.open_workflow_draft(draft, Some(BoardItem::Trigger), window, cx);
    }

    fn open_workflow_draft(
        &mut self,
        draft: WorkflowDraft,
        selected: Option<BoardItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut open = OpenWorkflow::new(draft);
        open.selected = selected;
        open.camera = BoardCamera::fitting(open.draft.extent(), self.workflow_board_size(window));
        self.pages.workflows.open = Some(open);
        self.ensure_workflow_market_search(window, cx);
        self.refresh_workflow_inputs(window, cx);
        cx.notify();
    }

    /// Starts the market search's consumer and field the first time a workflow opens.
    fn ensure_workflow_market_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace_id = self.tabs.active().id;
        self.pages.workflows.markets.ensure_worker(
            self.services.market_factory(),
            workspace_id,
            self.frames.dirty(),
        );
        if self.pages.workflows.markets.input().is_some() {
            return;
        }
        let input = cx.new(|input_cx| {
            InputState::new(window, input_cx).placeholder(tr!(workflows::MARKET_SEARCH_PLACEHOLDER))
        });
        cx.subscribe(&input, |terminal, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value();
                terminal.pages.workflows.markets.search(&query);
                cx.notify();
            }
        })
        .detach();
        self.pages.workflows.markets.set_input(input);
    }

    pub(super) fn activate_workflow_market(
        &mut self,
        action: &super::symbol_browser::InstrumentMenuAction,
        cx: &mut Context<Self>,
    ) {
        self.pages.workflows.markets.activate(action);
        cx.notify();
    }

    /// Applies the market search's latest messages; an installed market becomes the open
    /// draft's symbol when its terms are complete enough to trade.
    pub(super) fn poll_workflow_markets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let outcome = self.pages.workflows.markets.poll();
        if let Some(instrument) = outcome.installed {
            self.adopt_searched_market(&instrument, window, cx);
        }
        if outcome.changed {
            cx.notify();
        }
    }

    fn adopt_searched_market(
        &mut self,
        instrument: &InstallProviderInstrument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = &mut self.pages.workflows;
        match aeris_market_runtime::market_service::instrument_terms_from_install(instrument) {
            Ok(terms) => {
                page.markets.finish(None);
                if let Some(open) = page.open.as_mut() {
                    open.draft.choose_searched(SearchedMarket {
                        instrument_id: terms.route.instrument_id,
                        display_symbol: terms.route.display_symbol,
                        scales: InstrumentScales {
                            price: terms.route.price_scale,
                            quantity: terms.route.quantity_scale,
                        },
                    });
                    open.status = None;
                    open.confirm_delete = false;
                }
            }
            Err(error) => {
                diagnostic!("Aeris workflow market has no trading terms: {error}");
                page.markets.finish(Some(MarketSearchNotice::NotTradable(
                    instrument.display_symbol.clone(),
                )));
            }
        }
        if let Some(input) = page.markets.input().cloned() {
            input.update(cx, |input, input_cx| input.set_value("", window, input_cx));
        }
        cx.notify();
    }

    pub(super) fn retire_workflow_markets(&mut self, cx: &App) {
        if let Some(retirement) = self.pages.workflows.markets.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }

    /// The board's last painted size, or the window less the page's side panels before the
    /// board has painted.
    fn workflow_board_size(&self, window: &Window) -> BoardPoint {
        let painted = self.pages.workflows.board_size.get();
        if painted.x > 0.0 && painted.y > 0.0 {
            return painted;
        }
        let viewport = window.viewport_size();
        let page = &self.pages.workflows;
        let chrome = board_chrome(!page.list_hidden, !page.inspector_hidden);
        BoardPoint::new(
            f32::from(viewport.width) - chrome.x,
            f32::from(viewport.height) - chrome.y,
        )
    }

    pub(super) fn toggle_workflow_list(&mut self, cx: &mut Context<Self>) {
        let page = &mut self.pages.workflows;
        page.list_hidden = !page.list_hidden;
        cx.notify();
    }

    pub(super) fn toggle_workflow_inspector(&mut self, cx: &mut Context<Self>) {
        let page = &mut self.pages.workflows;
        page.inspector_hidden = !page.inspector_hidden;
        cx.notify();
    }

    /// The account and symbol a new draft starts on.
    fn new_workflow_target(
        &self,
        cx: &App,
    ) -> (
        Option<TradingAccountId>,
        Option<aeris_instruments::InstrumentId>,
    ) {
        let product = self
            .active_surface()
            .read(cx)
            .product
            .as_ref()
            .map(|product| product.instrument_id.clone());
        let store = self.services.trading_store().read(cx);
        let set = store.current();
        let account_id = set.and_then(|set| {
            set.accounts
                .iter()
                .find(|account| account.environment == AccountEnvironment::Simulated)
                .map(|account| account.id.clone())
        });
        let instrument_id = set.and_then(|set| {
            set.instruments
                .iter()
                .find(|instrument| Some(instrument.instrument_id.as_str()) == product.as_deref())
                .or_else(|| set.instruments.first())
                .map(|instrument| instrument.instrument_id.clone())
        });
        (account_id, instrument_id)
    }

    pub(super) fn open_saved_workflow(
        &mut self,
        workflow_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = self
            .services
            .trading_store()
            .read(cx)
            .current()
            .and_then(|set| {
                set.workflows
                    .iter()
                    .find(|view| view.definition.workflow_id == workflow_id)
            })
            .map(WorkflowDraft::from_view);
        if let Some(draft) = draft {
            self.open_workflow_draft(draft, None, window, cx);
        }
    }

    pub(super) fn close_open_workflow(&mut self, cx: &mut Context<Self>) {
        if self.pages.workflows.open.take().is_some() {
            cx.notify();
        }
    }

    /// Recreates the inspector inputs when the selection's fields changed, keeping the
    /// focused input otherwise.
    fn refresh_workflow_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.pages.workflows.open.as_ref() else {
            return;
        };
        let fields = open.fields();
        if open
            .inputs
            .iter()
            .map(|(field, _)| *field)
            .eq(fields.iter().copied())
        {
            return;
        }
        let mut inputs = Vec::with_capacity(fields.len());
        for field in fields {
            let value = open.draft.field(field).unwrap_or_default().to_string();
            let input = cx.new(|input_cx| InputState::new(window, input_cx).default_value(value));
            cx.subscribe(&input, move |terminal, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    if let Some(open) = terminal.pages.workflows.open.as_mut()
                        && open.draft.set_field(field, value)
                    {
                        open.status = None;
                        open.confirm_delete = false;
                        cx.notify();
                    }
                }
            })
            .detach();
            inputs.push((field, input));
        }
        if let Some(open) = self.pages.workflows.open.as_mut() {
            open.inputs = inputs;
        }
    }

    /// Applies one edit of the open workflow from a page control.
    pub(super) fn edit_open_workflow(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut OpenWorkflow),
    ) {
        let Some(open) = self.pages.workflows.open.as_mut() else {
            return;
        };
        open.status = None;
        open.confirm_delete = false;
        change(open);
        self.refresh_workflow_inputs(window, cx);
        cx.notify();
    }

    /// Adds a block after the selection, or after the trigger while nothing follows it.
    pub(super) fn add_workflow_block(
        &mut self,
        kind: super::workflow_board::BlockKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_open_workflow(window, cx, |open| {
            let after = open
                .selected
                .or_else(|| open.draft.start.is_none().then_some(BoardItem::Trigger));
            match open.draft.add_block(kind, after) {
                Some(id) => open.selected = Some(BoardItem::Node(id)),
                None => {
                    open.status = Some(PageStatus::Problem(tr!(
                        workflows::TOO_MANY_BLOCKS,
                        maximum = MAXIMUM_WORKFLOW_NODES
                    )));
                }
            }
        });
    }

    pub(super) fn zoom_workflow_board(&mut self, zoom_in: bool, cx: &mut Context<Self>) {
        if let Some(open) = self.pages.workflows.open.as_mut() {
            let factor = if zoom_in {
                ZOOM_STEP
            } else {
                ZOOM_STEP.recip()
            };
            open.camera.zoom_at(open.camera.pan, factor);
            cx.notify();
        }
    }

    /// Fits every block in view.
    pub(super) fn fit_workflow_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let size = self.workflow_board_size(window);
        if let Some(open) = self.pages.workflows.open.as_mut() {
            open.camera = BoardCamera::fitting(open.draft.extent(), size);
            cx.notify();
        }
    }

    pub(super) fn workflow_pointer_down(
        &mut self,
        at: BoardPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.window.chrome_focus().focus(window, cx);
        let Some(open) = self.pages.workflows.open.as_mut() else {
            return;
        };
        let board = open.camera.to_board(at);
        open.confirm_delete = false;
        match open.draft.hit(board) {
            Some(BoardHit::Output(port)) => {
                open.gesture = Some(BoardGesture::Connect {
                    port,
                    cursor: board,
                });
            }
            Some(BoardHit::Item(item)) => {
                let origin = open.draft.position(item).unwrap_or(board);
                open.gesture = Some(BoardGesture::Move {
                    item,
                    grab: BoardPoint::new(board.x - origin.x, board.y - origin.y),
                    moved: false,
                });
                open.selected = Some(item);
            }
            None => {
                open.gesture = Some(BoardGesture::Pan { last: at });
                open.selected = None;
            }
        }
        self.refresh_workflow_inputs(window, cx);
        cx.notify();
    }

    pub(super) fn workflow_pointer_move(&mut self, at: BoardPoint, cx: &mut Context<Self>) {
        let Some(OpenWorkflow {
            draft,
            camera,
            gesture: Some(gesture),
            ..
        }) = self.pages.workflows.open.as_mut()
        else {
            return;
        };
        match gesture {
            BoardGesture::Move { item, grab, moved } => {
                let board = camera.to_board(at);
                draft.move_item(*item, BoardPoint::new(board.x - grab.x, board.y - grab.y));
                *moved = true;
            }
            BoardGesture::Connect { cursor, .. } => *cursor = camera.to_board(at),
            BoardGesture::Pan { last } => {
                camera.pan_by(at.x - last.x, at.y - last.y);
                *last = at;
            }
        }
        cx.notify();
    }

    /// Ends a gesture: a moved block saves its position, and a link dropped on a block
    /// connects to it, or is removed when dropped on the empty board.
    pub(super) fn workflow_pointer_up(&mut self, at: BoardPoint, cx: &mut Context<Self>) {
        let Some(open) = self.pages.workflows.open.as_mut() else {
            return;
        };
        let Some(gesture) = open.gesture.take() else {
            return;
        };
        match gesture {
            BoardGesture::Move { moved: true, .. } => self.save_unchanged_workflow_layout(cx),
            BoardGesture::Move { .. } | BoardGesture::Pan { .. } => {}
            BoardGesture::Connect { port, .. } => {
                let board = open.camera.to_board(at);
                let target = match open.draft.hit(board) {
                    Some(BoardHit::Item(item)) => Some(item),
                    Some(BoardHit::Output(port)) => Some(port.item()),
                    None => None,
                };
                match target {
                    Some(BoardItem::Node(target)) => {
                        if open.draft.connect(port, target) == Err(LinkRefusal::Loop) {
                            open.status =
                                Some(PageStatus::Problem(tr!(workflows::LINK_LOOP).to_string()));
                        }
                    }
                    Some(BoardItem::Trigger) => {}
                    None => open.draft.disconnect(port),
                }
            }
        }
        cx.notify();
    }

    /// Scrolling pans the board; with Ctrl (Command on macOS) it zooms around the pointer.
    pub(super) fn workflow_wheel(
        &mut self,
        at: BoardPoint,
        delta: BoardPoint,
        zoom: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(open) = self.pages.workflows.open.as_mut() {
            if zoom {
                open.camera.zoom_at(at, (delta.y * WHEEL_ZOOM_RATE).exp());
            } else {
                open.camera.pan_by(delta.x, delta.y);
            }
            cx.notify();
        }
    }

    /// Saves the board positions of an accepted workflow whose blocks did not change; the
    /// owner keeps its revision and state.
    fn save_unchanged_workflow_layout(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.pages.workflows.open.as_ref() else {
            return;
        };
        let store = self.services.trading_store().read(cx);
        let (status, view) = draft_status(&open.draft, store.current());
        let layout = open.draft.layout();
        let (Some(view), DraftStatus::Saved) = (view, status) else {
            return;
        };
        if view.layout == layout {
            return;
        }
        let (workflow_id, revision) = (
            view.definition.workflow_id.clone(),
            view.definition.revision,
        );
        self.send_workflow_command(
            false,
            move |service| {
                service
                    .set_workflow_layout(workflow_id, revision, layout)
                    .map(|_| WorkflowReply::LayoutSaved)
            },
            cx,
        );
    }

    /// Saves the draft: a changed definition as a new paused revision, an unchanged one by
    /// its block positions alone.
    pub(super) fn save_open_workflow(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.pages.workflows.open.as_ref() else {
            return;
        };
        if open.pending.is_some() {
            return;
        }
        let store = self.services.trading_store().read(cx);
        let set = store.current();
        let (status, view) = draft_status(&open.draft, set);
        let scales = draft_scales(set, &open.draft);
        let (workflow_id, revision) = open.draft.saved.as_ref().map_or_else(
            || {
                (
                    format!("wf-{:x}", self.lifecycle.clock().wall_unix_nanos().max(0)),
                    0,
                )
            },
            |saved| (saved.workflow_id.clone(), saved.revision),
        );
        let (mut definition, layout) =
            match open
                .draft
                .build(workflow_id.clone(), revision.max(1), scales)
            {
                Ok(built) => built,
                Err(problem) => {
                    if let Some(open) = self.pages.workflows.open.as_mut() {
                        open.status = Some(PageStatus::Problem(problem_text(problem)));
                    }
                    cx.notify();
                    return;
                }
            };
        let finished = view.as_ref().is_some_and(|view| {
            matches!(
                view.state,
                WorkflowState::Completed | WorkflowState::Expired
            )
        });
        if status == DraftStatus::Saved && !finished {
            self.send_workflow_command(
                true,
                move |service| {
                    service
                        .set_workflow_layout(workflow_id, revision, layout)
                        .map(|view| WorkflowReply::Saved(Box::new(view)))
                },
                cx,
            );
            return;
        }
        definition.revision = revision + 1;
        self.send_workflow_command(
            true,
            move |service| {
                service
                    .register_workflow(definition, layout)
                    .map(|view| WorkflowReply::Saved(Box::new(view)))
            },
            cx,
        );
    }

    pub(super) fn arm_open_workflow(&mut self, arm: bool, cx: &mut Context<Self>) {
        let Some(saved) = self
            .pages
            .workflows
            .open
            .as_ref()
            .filter(|open| open.pending.is_none())
            .and_then(|open| open.draft.saved.clone())
        else {
            return;
        };
        let SavedRevision {
            workflow_id,
            revision,
        } = saved;
        self.send_workflow_command(
            true,
            move |service| {
                if arm {
                    service
                        .arm_workflow(workflow_id, revision)
                        .map(|view| WorkflowReply::Armed(view.definition.name))
                } else {
                    service
                        .pause_workflow(workflow_id, revision)
                        .map(|view| WorkflowReply::Paused(view.definition.name))
                }
            },
            cx,
        );
    }

    /// The first press asks for confirmation; the second deletes the workflow.
    pub(super) fn delete_open_workflow(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.pages.workflows.open.as_mut() else {
            return;
        };
        let Some(saved) = open.draft.saved.clone() else {
            self.pages.workflows.open = None;
            cx.notify();
            return;
        };
        if !open.confirm_delete {
            open.confirm_delete = true;
            cx.notify();
            return;
        }
        open.confirm_delete = false;
        let name = open.draft.name.trim().to_string();
        let workflow_id = saved.workflow_id;
        self.send_workflow_command(
            true,
            move |service| {
                service
                    .delete_workflow(workflow_id.clone())
                    .map(|()| WorkflowReply::Deleted { workflow_id, name })
            },
            cx,
        );
    }

    /// Runs a workflow command off the UI thread. A tracked command blocks further commands
    /// of the open workflow until its reply arrives.
    fn send_workflow_command(
        &mut self,
        tracked: bool,
        command: impl FnOnce(&TradingService) -> Result<WorkflowReply, WorkflowError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let page = &mut self.pages.workflows;
        page.next_command += 1;
        let token = page.next_command;
        if tracked && let Some(open) = page.open.as_mut() {
            open.pending = Some(token);
        }
        let service = self.lifecycle.trading().clone();
        cx.spawn(async move |terminal, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { command(&service) })
                .await;
            // A retired terminal has no page or feedback line left for the reply.
            terminal
                .update(cx, |terminal, terminal_cx| {
                    terminal.finish_workflow_command(token, result, terminal_cx);
                })
                .unwrap_or(());
        })
        .detach();
        cx.notify();
    }

    fn finish_workflow_command(
        &mut self,
        token: u64,
        result: Result<WorkflowReply, WorkflowError>,
        cx: &mut Context<Self>,
    ) {
        let feedback = self.lifecycle.trading_feedback().clone();
        let page = &mut self.pages.workflows;
        let open = page
            .open
            .as_mut()
            .filter(|open| open.pending == Some(token));
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                let text = workflow_error_text(&error);
                match open {
                    Some(open) => {
                        open.pending = None;
                        open.status = Some(PageStatus::Problem(text));
                    }
                    None => feedback.record(Err(text)),
                }
                cx.notify();
                return;
            }
        };
        let message = match reply {
            WorkflowReply::LayoutSaved => None,
            WorkflowReply::Saved(view) => {
                let message = tr!(workflows::SAVED, name = view.definition.name);
                if let Some(open) = open {
                    open.draft.saved = Some(SavedRevision {
                        workflow_id: view.definition.workflow_id.clone(),
                        revision: view.definition.revision,
                    });
                    open.status = Some(PageStatus::Note(message.clone()));
                }
                Some(message)
            }
            WorkflowReply::Armed(name) => Some(tr!(workflows::ARMED, name = name)),
            WorkflowReply::Paused(name) => Some(tr!(workflows::PAUSED, name = name)),
            WorkflowReply::Deleted { workflow_id, name } => {
                let shown = page
                    .open
                    .as_ref()
                    .and_then(|open| open.draft.saved.as_ref());
                if shown.is_some_and(|saved| saved.workflow_id == workflow_id) {
                    page.open = None;
                }
                Some(tr!(workflows::DELETED, name = name))
            }
        };
        if let Some(open) = page
            .open
            .as_mut()
            .filter(|open| open.pending == Some(token))
        {
            open.pending = None;
        }
        if let Some(message) = message {
            feedback.record(Ok(message));
        }
        cx.notify();
    }
}

pub(super) fn example_name(example: WorkflowExample) -> &'static str {
    match example {
        WorkflowExample::Breakout => tr!(workflows::EXAMPLE_BREAKOUT),
        WorkflowExample::SessionDip => tr!(workflows::EXAMPLE_SESSION_DIP),
        WorkflowExample::FlatBeforeClose => tr!(workflows::EXAMPLE_FLAT_BEFORE_CLOSE),
        WorkflowExample::ExitAndLock => tr!(workflows::EXAMPLE_EXIT_AND_LOCK),
    }
}

pub(super) fn example_description(example: WorkflowExample) -> &'static str {
    match example {
        WorkflowExample::Breakout => tr!(workflows::EXAMPLE_BREAKOUT_BODY),
        WorkflowExample::SessionDip => tr!(workflows::EXAMPLE_SESSION_DIP_BODY),
        WorkflowExample::FlatBeforeClose => tr!(workflows::EXAMPLE_FLAT_BEFORE_CLOSE_BODY),
        WorkflowExample::ExitAndLock => tr!(workflows::EXAMPLE_EXIT_AND_LOCK_BODY),
    }
}

/// Trimmed text, or the unset placeholder for none.
fn value_or_unset(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        tr!(workflows::VALUE_UNSET).to_string()
    } else {
        text.to_string()
    }
}

/// One trigger as a short phrase, for a block with several.
pub(super) fn trigger_summary(trigger: &TriggerDraft) -> String {
    match trigger {
        TriggerDraft::Price {
            direction: PriceDirection::Above,
            level,
        } => tr!(workflows::SUMMARY_RISES, price = value_or_unset(level)),
        TriggerDraft::Price {
            direction: PriceDirection::Below,
            level,
        } => tr!(workflows::SUMMARY_FALLS, price = value_or_unset(level)),
        TriggerDraft::Time { zone, time } => tr!(
            workflows::SUMMARY_TIME,
            time = value_or_unset(time),
            zone = zone_text(*zone)
        ),
        TriggerDraft::Every { minutes } => {
            tr!(workflows::SUMMARY_EVERY, minutes = value_or_unset(minutes))
        }
    }
}

/// One check as a sentence, for an If with several.
pub(super) fn check_summary(check: &CheckDraft) -> String {
    match check {
        CheckDraft::Position(requirement) => position_text(*requirement).to_string(),
        CheckDraft::Between { zone, start, end } => tr!(
            workflows::CONDITION_BETWEEN,
            start = value_or_unset(start),
            end = value_or_unset(end),
            zone = zone_text(*zone)
        ),
        CheckDraft::Price {
            direction: PriceDirection::Above,
            level,
        } => tr!(
            workflows::CONDITION_PRICE_ABOVE,
            price = value_or_unset(level)
        ),
        CheckDraft::Price {
            direction: PriceDirection::Below,
            level,
        } => tr!(
            workflows::CONDITION_PRICE_BELOW,
            price = value_or_unset(level)
        ),
        CheckDraft::PriceRange { low, high } => tr!(
            workflows::CONDITION_RANGE,
            low = value_or_unset(low),
            high = value_or_unset(high)
        ),
        CheckDraft::Weekday { zone, days } => tr!(
            workflows::CONDITION_WEEKDAY,
            days = days_text(*days),
            zone = zone_text(*zone)
        ),
    }
}

/// A day's short name; 0 is Monday.
pub(super) fn day_label(day: u8) -> &'static str {
    match day {
        0 => tr!(workflows::DAY_MON),
        1 => tr!(workflows::DAY_TUE),
        2 => tr!(workflows::DAY_WED),
        3 => tr!(workflows::DAY_THU),
        4 => tr!(workflows::DAY_FRI),
        5 => tr!(workflows::DAY_SAT),
        _ => tr!(workflows::DAY_SUN),
    }
}

/// The named days, with a run of three or more days written as a span.
pub(super) fn days_text(days: WorkflowWeekdays) -> String {
    let mut parts = Vec::new();
    let mut day = 0;
    while day < 7 {
        if !days.contains(day) {
            day += 1;
            continue;
        }
        let first = day;
        while day + 1 < 7 && days.contains(day + 1) {
            day += 1;
        }
        if day - first >= 2 {
            parts.push(format!("{}–{}", day_label(first), day_label(day)));
        } else {
            parts.extend((first..=day).map(|day| day_label(day).to_string()));
        }
        day += 1;
    }
    parts.join(", ")
}

/// The sentence an action block shows on the board.
pub(super) fn action_summary(action: &ActionDraft) -> String {
    match action {
        ActionDraft::Enter {
            side,
            quantity,
            stop,
            target,
        } => {
            let quantity = quantity.trim();
            let entry = match side {
                OrderSide::Buy => tr!(workflows::ACTION_BUY, quantity = quantity),
                OrderSide::Sell => tr!(workflows::ACTION_SELL, quantity = quantity),
            };
            if stop.trim().is_empty() && target.trim().is_empty() {
                entry
            } else {
                tr!(
                    workflows::ACTION_PROTECTION,
                    entry = entry,
                    stop = stop.trim(),
                    target = target.trim()
                )
            }
        }
        ActionDraft::Flatten => tr!(workflows::ACTION_FLATTEN).to_string(),
        ActionDraft::CancelOrders => tr!(workflows::ACTION_CANCEL_ORDERS).to_string(),
        ActionDraft::LockAccount => tr!(workflows::ACTION_LOCK).to_string(),
    }
}
