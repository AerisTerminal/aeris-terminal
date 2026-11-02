//! The order ticket's Workflows section: the selected account's workflows that run on the
//! ticket's symbol or on any symbol. Arm and Pause go through the terminal's one workflow
//! command path, so the Workflows page and every ticket share its one-command-per-workflow
//! guard, and the ticket's pane shows a refusal on its feedback line.

use super::super::workflows_page::{
    WorkflowRunOrigin, firing_time_text, outcome_text, state_color, state_text,
};
use super::*;
use aeris_localization::{tr, workflows};
use aeris_trading_runtime::{WorkflowScope, WorkflowState, WorkflowView};

/// The account's workflows that run on `instrument` or on any symbol, in the owner's order.
fn ticket_workflows<'a>(
    workflows: &'a [WorkflowView],
    account: &aeris_trading::TradingAccountId,
    instrument: Option<&aeris_instruments::InstrumentId>,
) -> Vec<&'a WorkflowView> {
    workflows
        .iter()
        .filter(|view| &view.definition.account_id == account)
        .filter(|view| match &view.definition.scope {
            WorkflowScope::Instrument(instrument_id) => Some(instrument_id) == instrument,
            WorkflowScope::AnySymbol => true,
        })
        .collect()
}

/// The chosen workflow while it is listed, otherwise the first, so a deleted or
/// out-of-scope choice falls back without forgetting it.
fn shown_workflow<'a>(
    listed: &[&'a WorkflowView],
    chosen: Option<&str>,
) -> Option<&'a WorkflowView> {
    chosen
        .and_then(|workflow_id| {
            listed
                .iter()
                .find(|view| view.definition.workflow_id == workflow_id)
        })
        .or_else(|| listed.first())
        .copied()
}

/// Why the run button cannot be pressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunBlock {
    /// Completed or expired: only a new revision runs again.
    Finished,
    /// The Workflows page holds changes to it that are not saved.
    Unsaved,
    /// An arm, pause, save or delete of it is in flight.
    Busy,
}

impl RunBlock {
    fn reason(self) -> &'static str {
        match self {
            Self::Finished => tr!(workflows::TICKET_REASON_FINISHED),
            Self::Unsaved => tr!(workflows::TICKET_REASON_UNSAVED),
            Self::Busy => tr!(workflows::TICKET_REASON_BUSY),
        }
    }
}

/// What the run button sends, and what holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RunChoice {
    arm: bool,
    blocked: Option<RunBlock>,
}

/// Arm for a paused workflow, Pause for an armed one. As on the page, unsaved changes hold
/// only an arm, so a running workflow can always be paused.
fn run_choice(state: WorkflowState, unsaved: bool, busy: bool) -> RunChoice {
    let blocked = match state {
        WorkflowState::Completed | WorkflowState::Expired => Some(RunBlock::Finished),
        _ if busy => Some(RunBlock::Busy),
        WorkflowState::Paused if unsaved => Some(RunBlock::Unsaved),
        WorkflowState::Paused | WorkflowState::Armed => None,
    };
    RunChoice {
        arm: state != WorkflowState::Armed,
        blocked,
    }
}

/// The state a menu row names; the status line under the menu carries the blockers.
fn short_state(state: WorkflowState) -> &'static str {
    match state {
        WorkflowState::Paused => tr!(workflows::STATE_PAUSED),
        WorkflowState::Armed => tr!(workflows::STATE_ARMED),
        WorkflowState::Completed => tr!(workflows::STATE_COMPLETED),
        WorkflowState::Expired => tr!(workflows::STATE_EXPIRED),
    }
}

/// The workflow's state and its newest firing, such as `Armed · Last fired 14:02: Flattened`.
fn status_text(view: &WorkflowView) -> String {
    let firing = view.recent_firings.first().map_or_else(
        || tr!(workflows::NEVER_FIRED).to_string(),
        |firing| {
            tr!(
                workflows::TICKET_LAST_FIRED,
                time = firing_time_text(firing.fired_unix_nanos),
                outcome = outcome_text(&firing.outcome)
            )
        },
    );
    tr!(
        workflows::TICKET_STATUS,
        state = state_text(view),
        firing = firing
    )
}

pub(super) fn workflow_section(
    state: &TradingOrderControlsState<'_>,
    view: &TicketView<'_>,
) -> Div {
    let muted = state.theme.colors.text_muted;
    let section = div().flex().flex_col().gap(px(GAP)).child(
        div()
            .p(px(2.0))
            .font_weight(platform_font_weight(TypographyRole::Emphasis))
            .child(tr!(panels::APP_VIEW_WORKFLOWS)),
    );
    let Some(account) = view.account else {
        return section
            .child(note(tr!(workflows::TICKET_NO_ACCOUNT), muted))
            .child(new_button(state, None));
    };
    let set = state.store.current();
    let listed = ticket_workflows(
        set.map_or(&[], |set| set.workflows.as_slice()),
        &account.id,
        view.instrument.as_ref(),
    );
    let Some(shown) = shown_workflow(&listed, state.controls.workflow.as_deref()) else {
        return section
            .child(note(tr!(workflows::TICKET_EMPTY), muted))
            .child(new_button(state, Some(account.id.clone())));
    };
    let workflow_id = shown.definition.workflow_id.as_str();
    let choice = run_choice(
        shown.state,
        state.workflows.unsaved_workflow(set) == Some(workflow_id),
        state.workflows.is_busy(workflow_id),
    );
    section
        .child(workflow_select(state, &listed, shown))
        .child(
            div()
                .flex()
                .gap(px(GAP))
                .child(run_button(state, shown, choice).flex_1())
                .child(edit_button(state, shown)),
        )
        .child(status_line(shown, state.theme))
        .children(choice.blocked.map(|block| note(block.reason(), muted)))
}

fn note(text: impl Into<SharedString>, color: ThemeColor) -> Div {
    div()
        .mx(px(2.0))
        .text_size(px(11.0))
        .text_color(gpui_color(color))
        .child(text.into())
}

fn workflow_select(
    state: &TradingOrderControlsState<'_>,
    listed: &[&WorkflowView],
    shown: &WorkflowView,
) -> MenuAnchor {
    let theme = state.theme;
    let open = state.controls.open_menu == Some(TicketMenu::Workflow);
    let menu = open.then(|| {
        select_menu("ticket_workflow_menu", theme).children(listed.iter().enumerate().map(
            |(index, view)| {
                let choose = state.app.clone();
                let workflow_id = view.definition.workflow_id.clone();
                MenuRow::compact(
                    ("ticket_workflow_option", index),
                    view.definition.name.clone(),
                    theme,
                )
                .detail(short_state(view.state))
                .checked(view.definition.workflow_id == shown.definition.workflow_id)
                .on_click(move |_, _, cx| {
                    choose.update(cx, |surface, surface_cx| {
                        surface.select_ticket_workflow(workflow_id.clone(), surface_cx);
                    });
                })
            },
        ))
    });
    ticket_select(
        state.app,
        "ticket_workflow_field",
        select_trigger(
            "ticket_workflow",
            None,
            shown.definition.name.clone(),
            theme,
            menu_toggle(state.app, TicketMenu::Workflow),
        ),
        menu,
    )
}

/// Sends the shown revision; the owner refuses it as stale if a newer one was saved since.
fn run_button(
    state: &TradingOrderControlsState<'_>,
    shown: &WorkflowView,
    choice: RunChoice,
) -> Button {
    let name = shown.definition.name.as_str();
    let (id, label, aria, variant) = if choice.arm {
        (
            "ticket_workflow_arm",
            tr!(workflows::ARM),
            tr!(workflows::ARM_ARIA, name = name),
            ButtonVariant::Primary,
        )
    } else {
        (
            "ticket_workflow_pause",
            tr!(workflows::PAUSE),
            tr!(workflows::PAUSE_ARIA, name = name),
            ButtonVariant::Outline,
        )
    };
    let app = state.app.clone();
    let terminal = state.terminal.clone();
    let workflow_id = shown.definition.workflow_id.clone();
    let revision = shown.definition.revision;
    let arm = choice.arm;
    Button::new(id, state.theme)
        .variant(variant)
        .button_size(ButtonSize::Sm)
        .label(label)
        .aria_label(aria)
        .disabled(choice.blocked.is_some())
        .loading(choice.blocked == Some(RunBlock::Busy))
        .on_click(move |_, _, cx| {
            let pane = app.read(cx).trading_pane_token.clone();
            terminal.update(cx, |terminal, terminal_cx| {
                terminal.run_workflow(
                    workflow_id.clone(),
                    revision,
                    arm,
                    WorkflowRunOrigin::Ticket(pane),
                    terminal_cx,
                );
            });
        })
}

fn edit_button(state: &TradingOrderControlsState<'_>, shown: &WorkflowView) -> Button {
    let terminal = state.terminal.clone();
    let workflow_id = shown.definition.workflow_id.clone();
    Button::new("ticket_workflow_edit", state.theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .label(tr!(workflows::TICKET_EDIT))
        .aria_label(tr!(
            workflows::TICKET_EDIT_ARIA,
            name = shown.definition.name.as_str()
        ))
        .on_click(move |_, window, cx| {
            terminal.update(cx, |terminal, terminal_cx| {
                terminal.edit_workflow_on_page(&workflow_id, window, terminal_cx);
            });
        })
}

/// Opens a new draft on the ticket's account, or the first practice account without one.
fn new_button(
    state: &TradingOrderControlsState<'_>,
    account_id: Option<aeris_trading::TradingAccountId>,
) -> Button {
    let terminal = state.terminal.clone();
    Button::new("ticket_workflow_new", state.theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Sm)
        .full_width()
        .label(tr!(workflows::NEW_BUTTON))
        .aria_label(tr!(workflows::NEW_ARIA))
        .on_click(move |_, window, cx| {
            terminal.update(cx, |terminal, terminal_cx| {
                terminal.new_workflow_on_page(account_id.clone(), window, terminal_cx);
            });
        })
}

/// A dot in the state's color, then the state and the newest firing.
fn status_line(shown: &WorkflowView, theme: &AerisTheme) -> Div {
    div()
        .mx(px(2.0))
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(11.0))
        .text_color(gpui_color(theme.colors.text_secondary))
        .child(
            div()
                .flex_none()
                .size(px(6.0))
                .rounded(radius(RadiusToken::Full))
                .bg(state_color(shown, theme)),
        )
        .child(div().min_w_0().child(status_text(shown)))
}

#[cfg(test)]
mod tests {
    use super::super::super::workflow_board::{InstrumentScales, WorkflowExample};
    use super::*;
    use aeris_instruments::InstrumentId;
    use aeris_trading::TradingAccountId;

    fn account(id: &str) -> TradingAccountId {
        TradingAccountId::try_new(id).expect("account")
    }

    fn instrument(id: &str) -> InstrumentId {
        InstrumentId::try_new(id).expect("instrument")
    }

    fn workflow(id: &str, account_id: &str, scope: WorkflowScope) -> WorkflowView {
        let (mut definition, layout) = WorkflowExample::FlatBeforeClose
            .draft(
                id.to_string(),
                Some(account(account_id)),
                Some(instrument("instrument:fixture:A")),
            )
            .build(
                id.to_string(),
                1,
                Some(InstrumentScales {
                    price: 2,
                    quantity: 0,
                }),
                None,
            )
            .expect("the example builds");
        definition.scope = scope;
        WorkflowView {
            definition,
            layout,
            state: WorkflowState::Paused,
            session_firings: 0,
            total_firings: 0,
            last_attempt_unix_nanos: None,
            next_due_unix_nanos: None,
            blockers: Vec::new(),
            recent_firings: Vec::new(),
        }
    }

    fn ids(listed: &[&WorkflowView]) -> Vec<String> {
        listed
            .iter()
            .map(|view| view.definition.workflow_id.clone())
            .collect()
    }

    #[test]
    fn the_ticket_lists_the_accounts_workflows_on_its_symbol_or_any_symbol() {
        let a = instrument("instrument:fixture:A");
        let b = instrument("instrument:fixture:B");
        let workflows = [
            workflow("on-a", "practice-1", WorkflowScope::Instrument(a.clone())),
            workflow("on-b", "practice-1", WorkflowScope::Instrument(b)),
            workflow("any", "practice-1", WorkflowScope::AnySymbol),
            workflow("other-account", "practice-2", WorkflowScope::AnySymbol),
        ];
        let practice = account("practice-1");
        assert_eq!(
            ids(&ticket_workflows(&workflows, &practice, Some(&a))),
            ["on-a", "any"]
        );
        assert_eq!(
            ids(&ticket_workflows(&workflows, &practice, None)),
            ["any"],
            "a ticket without a symbol lists only any-symbol workflows"
        );
        assert_eq!(
            ids(&ticket_workflows(
                &workflows,
                &account("practice-3"),
                Some(&a)
            )),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_missing_choice_falls_back_to_the_first_listed_workflow() {
        let workflows = [
            workflow("first", "practice-1", WorkflowScope::AnySymbol),
            workflow("second", "practice-1", WorkflowScope::AnySymbol),
        ];
        let listed: Vec<_> = workflows.iter().collect();
        let shown = |chosen| {
            shown_workflow(&listed, chosen).map(|view| view.definition.workflow_id.as_str())
        };
        assert_eq!(shown(None), Some("first"));
        assert_eq!(shown(Some("second")), Some("second"));
        assert_eq!(shown(Some("deleted")), Some("first"));
        assert_eq!(shown_workflow(&[], Some("second")), None);
    }

    #[test]
    fn the_run_button_arms_paused_pauses_armed_and_names_what_holds_it() {
        use WorkflowState::{Armed, Completed, Expired, Paused};
        let choice = |arm, blocked| RunChoice { arm, blocked };
        assert_eq!(run_choice(Paused, false, false), choice(true, None));
        assert_eq!(run_choice(Armed, false, false), choice(false, None));
        assert_eq!(
            run_choice(Paused, true, false),
            choice(true, Some(RunBlock::Unsaved))
        );
        assert_eq!(
            run_choice(Armed, true, false),
            choice(false, None),
            "unsaved changes never hold a pause"
        );
        assert_eq!(
            run_choice(Armed, false, true),
            choice(false, Some(RunBlock::Busy))
        );
        assert_eq!(
            run_choice(Paused, true, true),
            choice(true, Some(RunBlock::Busy))
        );
        for finished in [Completed, Expired] {
            assert_eq!(
                run_choice(finished, false, true),
                choice(true, Some(RunBlock::Finished))
            );
        }
    }
}
