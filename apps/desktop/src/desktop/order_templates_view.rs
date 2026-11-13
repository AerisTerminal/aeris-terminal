//! Order templates (ATM strategies): the template the order ticket, the trading shortcuts and
//! the chart trade bar place their brackets with, and the dialog that creates, edits and
//! duplicates a template, with its delete confirmation. The trading owner holds and validates
//! every template; the chart chrome keeps only which one orders use.

use super::*;
use aeris_desktop::trading::{TicketBracket, TicketProtection};
use aeris_localization::{common, order_templates, tr};
use aeris_trading_runtime::{
    MAXIMUM_BRACKET_TARGETS, MAXIMUM_ORDER_TEMPLATE_NAME_CHARS, MAXIMUM_ORDER_TEMPLATE_QUANTITY,
    MAXIMUM_ORDER_TEMPLATE_TICKS, MAXIMUM_ORDER_TEMPLATES, OrderTemplate, OrderTemplateError,
    OrderTemplateInvalid, OrderTemplateTarget, TrailingStopRule, order_template_id,
    order_template_key,
};
use std::num::NonZeroU64;

/// The template orders use, as the chart chrome names it and the trading owner publishes it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum OrderTemplateChoice {
    /// No template: the ticket's own bracket, if any, protects its orders.
    None,
    Template(OrderTemplate),
    /// The chosen template is not among the owner's saved templates, so no order may stand in
    /// for it.
    Missing,
}

impl OrderTemplateChoice {
    /// `saved` is `None` until the trading owner has published its templates.
    pub(super) fn resolve(saved: Option<&[OrderTemplate]>, key: Option<NonZeroU64>) -> Self {
        let Some(key) = key else {
            return Self::None;
        };
        let template_id = order_template_id(key);
        saved
            .and_then(|saved| {
                saved
                    .iter()
                    .find(|template| template.template_id == template_id)
            })
            .cloned()
            .map_or(Self::Missing, Self::Template)
    }

    /// The template an account order is placed with, or why none may be placed.
    pub(super) fn template_id(&self) -> Result<Option<String>, &'static str> {
        match self {
            Self::None => Ok(None),
            Self::Template(template) => Ok(Some(template.template_id.clone())),
            Self::Missing => Err(tr!(order_templates::SELECTED_MISSING)),
        }
    }

    /// The exits an order-ticket entry carries: the template's bracket, or else the ticket's.
    pub(super) fn ticket_protection(
        &self,
        ticket: Option<TicketBracket>,
    ) -> Result<TicketProtection, &'static str> {
        Ok(match self.template_id()? {
            Some(template_id) => TicketProtection::Template(template_id),
            None => ticket.map_or(TicketProtection::None, TicketProtection::Bracket),
        })
    }

    /// The stop and target a bar replay trades: the replay holds one of each, so only a
    /// template with one target and no trailing stop can trade there.
    pub(super) fn replay_bracket(
        &self,
        ticket: Option<TicketBracket>,
    ) -> Result<Option<TicketBracket>, &'static str> {
        match self {
            Self::None => Ok(ticket),
            Self::Template(template) => match template.targets.as_slice() {
                [target] if template.trailing_stop.is_none() => Ok(Some(TicketBracket {
                    stop_ticks: template.stop_offset_ticks,
                    target_ticks: target.offset_ticks,
                })),
                _ => Err(tr!(order_templates::REPLAY_UNSUPPORTED)),
            },
            Self::Missing => Err(tr!(order_templates::SELECTED_MISSING)),
        }
    }
}

impl WorkspaceSurface {
    /// The template this surface's orders use, resolved at the click or key.
    pub(super) fn order_template_choice(&self, cx: &App) -> OrderTemplateChoice {
        let key = self.chart_chrome.order_template;
        match self.lifecycle.trading_store() {
            Some(store) => OrderTemplateChoice::resolve(
                store
                    .read(cx)
                    .current()
                    .map(|set| set.order_templates.as_slice()),
                key,
            ),
            None => OrderTemplateChoice::resolve(None, key),
        }
    }
}

/// A fresh template identity from the clock, stepping past any identity already saved; with
/// at most [`MAXIMUM_ORDER_TEMPLATES`] saved, that many steps always find a free one.
fn fresh_order_template_id(now_unix_nanos: i64, saved: &[OrderTemplate]) -> String {
    let mut key = u64::try_from(now_unix_nanos).unwrap_or(1);
    let mut template_id = order_template_id(NonZeroU64::new(key).unwrap_or(NonZeroU64::MIN));
    for _ in 0..=MAXIMUM_ORDER_TEMPLATES {
        if !saved
            .iter()
            .any(|template| template.template_id == template_id)
        {
            break;
        }
        key = key.wrapping_add(1);
        template_id = order_template_id(NonZeroU64::new(key).unwrap_or(NonZeroU64::MIN));
    }
    template_id
}

/// One editor field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditorField {
    Name,
    Quantity,
    Stop,
    TargetTicks(usize),
    TargetQuantity(usize),
    TrailingActivation,
    TrailingDistance,
}

const EDITOR_FIELD_COUNT: usize = 5 + 2 * MAXIMUM_BRACKET_TARGETS;

impl EditorField {
    const fn index(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Quantity => 1,
            Self::Stop => 2,
            Self::TrailingActivation => 3,
            Self::TrailingDistance => 4,
            Self::TargetTicks(target) => 5 + 2 * target,
            Self::TargetQuantity(target) => 6 + 2 * target,
        }
    }
}

/// Why the editor could not save its template.
#[derive(Clone, Debug, PartialEq)]
enum EditorProblem {
    /// A number field that does not hold a whole number.
    NotANumber(EditorField),
    Refused(OrderTemplateError),
}

impl EditorProblem {
    fn marks(&self, field: EditorField) -> bool {
        use OrderTemplateInvalid as Invalid;
        match self {
            Self::NotANumber(problem) => *problem == field,
            Self::Refused(OrderTemplateError::Invalid(invalid)) => matches!(
                (invalid, field),
                (Invalid::Name, EditorField::Name)
                    | (
                        Invalid::Quantity | Invalid::TargetTotal,
                        EditorField::Quantity
                    )
                    | (Invalid::StopOffset, EditorField::Stop)
                    | (
                        Invalid::TargetOffset | Invalid::TargetOrder,
                        EditorField::TargetTicks(_)
                    )
                    | (
                        Invalid::TargetQuantity | Invalid::TargetTotal | Invalid::TargetSplit,
                        EditorField::TargetQuantity(_),
                    )
                    | (
                        Invalid::TrailingStop,
                        EditorField::TrailingActivation | EditorField::TrailingDistance,
                    )
            ),
            Self::Refused(_) => false,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::NotANumber(_) => tr!(order_templates::WHOLE_NUMBER_REQUIRED).to_string(),
            Self::Refused(error) => order_template_error_text(error),
        }
    }
}

/// The text a refused template command shows, chosen by the refusal's kind.
fn order_template_error_text(error: &OrderTemplateError) -> String {
    match error {
        OrderTemplateError::Invalid(invalid) => order_template_invalid_text(*invalid),
        OrderTemplateError::TooManyTemplates => {
            tr!(order_templates::TOO_MANY, maximum = MAXIMUM_ORDER_TEMPLATES)
        }
        OrderTemplateError::UnknownTemplate => tr!(order_templates::UNKNOWN_TEMPLATE).to_string(),
        OrderTemplateError::RevisionMustAdvance { .. } => {
            tr!(order_templates::REVISION_MUST_ADVANCE).to_string()
        }
        OrderTemplateError::Owner(detail) => {
            tr!(order_templates::SAVE_FAILED, error = detail)
        }
    }
}

fn order_template_invalid_text(invalid: OrderTemplateInvalid) -> String {
    let ticks = MAXIMUM_ORDER_TEMPLATE_TICKS;
    match invalid {
        OrderTemplateInvalid::Identity => tr!(order_templates::INVALID_IDENTITY).to_string(),
        OrderTemplateInvalid::Revision => tr!(order_templates::INVALID_REVISION).to_string(),
        OrderTemplateInvalid::Name => tr!(
            order_templates::INVALID_NAME,
            maximum = MAXIMUM_ORDER_TEMPLATE_NAME_CHARS
        ),
        OrderTemplateInvalid::Quantity => tr!(
            order_templates::INVALID_QUANTITY,
            maximum = MAXIMUM_ORDER_TEMPLATE_QUANTITY
        ),
        OrderTemplateInvalid::StopOffset => tr!(order_templates::INVALID_STOP, maximum = ticks),
        OrderTemplateInvalid::TargetCount => tr!(
            order_templates::INVALID_TARGET_COUNT,
            maximum = MAXIMUM_BRACKET_TARGETS
        ),
        OrderTemplateInvalid::TargetOffset => {
            tr!(order_templates::INVALID_TARGET_OFFSET, maximum = ticks)
        }
        OrderTemplateInvalid::TargetOrder => tr!(order_templates::INVALID_TARGET_ORDER).to_string(),
        OrderTemplateInvalid::TargetQuantity => {
            tr!(order_templates::INVALID_TARGET_QUANTITY).to_string()
        }
        OrderTemplateInvalid::TargetTotal => tr!(order_templates::INVALID_TARGET_TOTAL).to_string(),
        OrderTemplateInvalid::TargetSplit => tr!(order_templates::INVALID_TARGET_SPLIT).to_string(),
        OrderTemplateInvalid::TrailingStop => {
            tr!(order_templates::INVALID_TRAILING, maximum = ticks)
        }
    }
}

fn parse_whole(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The template the editor's fields describe, checked by the owner's own validation so the
/// editor refuses exactly what the owner would.
fn draft_template(
    template_id: String,
    revision: u32,
    target_count: usize,
    trailing: bool,
    text: impl Fn(EditorField) -> String,
) -> Result<OrderTemplate, EditorProblem> {
    let number = |field| parse_whole(&text(field)).ok_or(EditorProblem::NotANumber(field));
    let quantity = number(EditorField::Quantity)?;
    let stop_offset_ticks = number(EditorField::Stop)?;
    let targets = (0..target_count.min(MAXIMUM_BRACKET_TARGETS))
        .map(|target| {
            Ok(OrderTemplateTarget {
                offset_ticks: number(EditorField::TargetTicks(target))?,
                quantity: number(EditorField::TargetQuantity(target))?,
            })
        })
        .collect::<Result<Vec<_>, EditorProblem>>()?;
    let trailing_stop = if trailing {
        Some(TrailingStopRule {
            activation_ticks: number(EditorField::TrailingActivation)?,
            distance_ticks: number(EditorField::TrailingDistance)?,
        })
    } else {
        None
    };
    let template = OrderTemplate {
        template_id,
        revision,
        name: text(EditorField::Name).trim().to_string(),
        quantity,
        stop_offset_ticks,
        targets,
        trailing_stop,
    };
    template
        .validate()
        .map_err(|invalid| EditorProblem::Refused(OrderTemplateError::Invalid(invalid)))?;
    Ok(template)
}

/// What the editor opens on.
pub(super) enum OrderTemplateDraft {
    New,
    Edit(OrderTemplate),
    Duplicate(OrderTemplate),
}

/// The open template dialog, the template awaiting delete confirmation and the command in
/// flight.
#[derive(Default)]
pub(super) struct OrderTemplatesUi {
    editor: Option<OrderTemplateEditor>,
    delete_confirmation: Option<OrderTemplate>,
    next_command: u64,
}

impl OrderTemplatesUi {
    pub(super) fn modal_open(&self) -> bool {
        self.editor.is_some() || self.delete_confirmation.is_some()
    }
}

struct OrderTemplateEditor {
    /// The identity and revision the save writes: a fresh identity for a new or duplicated
    /// template, the next revision for an edited one.
    template_id: String,
    revision: u32,
    editing: bool,
    fields: [Entity<InputState>; EDITOR_FIELD_COUNT],
    target_count: usize,
    trailing: bool,
    problem: Option<EditorProblem>,
    /// The save in flight, which holds further saves until its reply.
    pending: Option<u64>,
    _input_events: Vec<gpui::Subscription>,
}

impl OrderTemplateEditor {
    fn input(&self, field: EditorField) -> &Entity<InputState> {
        &self.fields[field.index()]
    }

    fn marks(&self, field: EditorField) -> bool {
        self.problem
            .as_ref()
            .is_some_and(|problem| problem.marks(field))
    }
}

/// The values the editor opens with: a new template starts at one contract with an 8-tick
/// stop and a 16-tick target.
fn draft_values(template: Option<&OrderTemplate>) -> Vec<(EditorField, String)> {
    let mut values = vec![
        (EditorField::Quantity, "1".to_string()),
        (EditorField::Stop, "8".to_string()),
        (EditorField::TargetTicks(0), "16".to_string()),
        (EditorField::TargetQuantity(0), "1".to_string()),
        (EditorField::TrailingActivation, "8".to_string()),
        (EditorField::TrailingDistance, "4".to_string()),
    ];
    let Some(template) = template else {
        return values;
    };
    values.clear();
    values.push((EditorField::Name, template.name.clone()));
    values.push((EditorField::Quantity, template.quantity.to_string()));
    values.push((EditorField::Stop, template.stop_offset_ticks.to_string()));
    for (index, target) in template.targets.iter().enumerate() {
        values.push((
            EditorField::TargetTicks(index),
            target.offset_ticks.to_string(),
        ));
        values.push((
            EditorField::TargetQuantity(index),
            target.quantity.to_string(),
        ));
    }
    let trailing = template.trailing_stop.unwrap_or(TrailingStopRule {
        activation_ticks: 8,
        distance_ticks: 4,
    });
    values.push((
        EditorField::TrailingActivation,
        trailing.activation_ticks.to_string(),
    ));
    values.push((
        EditorField::TrailingDistance,
        trailing.distance_ticks.to_string(),
    ));
    values
}

/// A template command's reply.
enum TemplateReply {
    Saved(OrderTemplate),
    Removed(OrderTemplate),
}

/// A refused template command.
enum TemplateFailure {
    Save(OrderTemplateError),
    Remove(OrderTemplateError),
}

impl TerminalApp {
    /// Makes `key` the template every order control uses. Every surface saves its own copy of
    /// the chart chrome, so each one carries the choice.
    pub(super) fn select_order_template(
        &mut self,
        key: Option<NonZeroU64>,
        cx: &mut Context<Self>,
    ) {
        if self.chrome.preferences().order_template == key {
            return;
        }
        self.chrome.preferences_mut().order_template = key;
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, surface_cx| {
                    surface.chart_chrome.order_template = key;
                    surface_cx.notify();
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }

    pub(super) fn open_order_template_editor(
        &mut self,
        draft: OrderTemplateDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = self
            .services
            .trading_store()
            .read(cx)
            .current()
            .map_or(&[][..], |set| set.order_templates.as_slice());
        let fresh = fresh_order_template_id(self.lifecycle.trading_feedback().now(), saved);
        let (template_id, revision, editing, source) = match draft {
            OrderTemplateDraft::New => (fresh, 1, false, None),
            OrderTemplateDraft::Edit(template) => (
                template.template_id.clone(),
                template.revision.saturating_add(1),
                true,
                Some(template),
            ),
            OrderTemplateDraft::Duplicate(mut template) => {
                template.name = tr!(order_templates::COPY_NAME, name = template.name);
                (fresh, 1, false, Some(template))
            }
        };
        let fields = std::array::from_fn(|index| {
            cx.new(|input_cx| {
                let input = InputState::new(window, input_cx);
                if index == EditorField::Name.index() {
                    input.placeholder(tr!(order_templates::NAME_PLACEHOLDER))
                } else {
                    input
                }
            })
        });
        for (field, value) in draft_values(source.as_ref()) {
            fields[field.index()].update(cx, |input, input_cx| {
                input.set_value(value, window, input_cx);
            });
        }
        let input_events = fields
            .iter()
            .map(|input| {
                cx.subscribe_in(
                    input,
                    window,
                    |terminal, _, event: &InputEvent, window, cx| match event {
                        InputEvent::PressEnter { .. } => terminal.save_order_template(window, cx),
                        InputEvent::Change => terminal.clear_order_template_problem(cx),
                        InputEvent::Blur | InputEvent::Focus => {}
                    },
                )
            })
            .collect();
        fields[EditorField::Name.index()].update(cx, |input, input_cx| {
            input.focus(window, input_cx);
        });
        self.pages.order_templates.delete_confirmation = None;
        self.pages.order_templates.editor = Some(OrderTemplateEditor {
            template_id,
            revision,
            editing,
            fields,
            target_count: source.as_ref().map_or(1, |template| {
                template.targets.len().clamp(1, MAXIMUM_BRACKET_TARGETS)
            }),
            trailing: source
                .as_ref()
                .is_some_and(|template| template.trailing_stop.is_some()),
            problem: None,
            pending: None,
            _input_events: input_events,
        });
        cx.notify();
    }

    fn clear_order_template_problem(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.pages.order_templates.editor.as_mut()
            && editor.problem.take().is_some()
        {
            cx.notify();
        }
    }

    fn add_order_template_target(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.pages.order_templates.editor.as_mut()
            && editor.target_count < MAXIMUM_BRACKET_TARGETS
        {
            editor.target_count += 1;
            editor.problem = None;
            cx.notify();
        }
    }

    /// Removes one target row; the further targets move up into its place.
    fn remove_order_template_target(
        &mut self,
        target: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.pages.order_templates.editor.as_mut() else {
            return;
        };
        if editor.target_count <= 1 || target >= editor.target_count {
            return;
        }
        for row in target..editor.target_count {
            for field in [EditorField::TargetTicks, EditorField::TargetQuantity] {
                let value = if row + 1 < editor.target_count {
                    editor.input(field(row + 1)).read(cx).value().to_string()
                } else {
                    String::new()
                };
                editor.input(field(row)).update(cx, |input, input_cx| {
                    input.set_value(value, window, input_cx);
                });
            }
        }
        editor.target_count -= 1;
        editor.problem = None;
        cx.notify();
    }

    fn set_order_template_trailing(&mut self, trailing: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.pages.order_templates.editor.as_mut() {
            editor.trailing = trailing;
            editor.problem = None;
            cx.notify();
        }
    }

    /// Checks the editor's template and sends it to the trading owner, or shows why it cannot
    /// be saved.
    pub(super) fn save_order_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &mut self.pages.order_templates;
        let Some(editor) = ui.editor.as_mut() else {
            return;
        };
        if editor.pending.is_some() {
            return;
        }
        let draft = draft_template(
            editor.template_id.clone(),
            editor.revision,
            editor.target_count,
            editor.trailing,
            |field| editor.input(field).read(cx).value().to_string(),
        );
        let template = match draft {
            Ok(template) => template,
            Err(problem) => {
                editor.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        ui.next_command += 1;
        let token = ui.next_command;
        editor.pending = Some(token);
        editor.problem = None;
        self.send_order_template_command(
            token,
            move |service| {
                service
                    .save_order_template(template)
                    .map(TemplateReply::Saved)
                    .map_err(TemplateFailure::Save)
            },
            window,
            cx,
        );
    }

    pub(super) fn request_order_template_delete(
        &mut self,
        template: OrderTemplate,
        cx: &mut Context<Self>,
    ) {
        self.pages.order_templates.editor = None;
        self.pages.order_templates.delete_confirmation = Some(template);
        cx.notify();
    }

    fn delete_order_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &mut self.pages.order_templates;
        let Some(template) = ui.delete_confirmation.take() else {
            return;
        };
        ui.next_command += 1;
        let token = ui.next_command;
        self.send_order_template_command(
            token,
            move |service| match service.remove_order_template(template.template_id.clone()) {
                Ok(()) => Ok(TemplateReply::Removed(template)),
                Err(error) => Err(TemplateFailure::Remove(error)),
            },
            window,
            cx,
        );
        self.focus_workspace(window, cx);
    }

    pub(super) fn cancel_order_template_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pages.order_templates.editor = None;
        self.pages.order_templates.delete_confirmation = None;
        self.focus_workspace(window, cx);
        cx.notify();
    }

    /// Runs a template command off the UI thread; the reply comes back to this window.
    fn send_order_template_command(
        &mut self,
        token: u64,
        command: impl FnOnce(
            &aeris_trading_runtime::TradingService,
        ) -> Result<TemplateReply, TemplateFailure>
        + Send
        + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.lifecycle.trading().clone();
        cx.spawn_in(window, async move |terminal, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { command(&service) })
                .await;
            if let Err(error) = terminal.update_in(cx, |terminal, window, terminal_cx| {
                terminal.finish_order_template_command(token, result, window, terminal_cx);
            }) && terminal.upgrade().is_some()
            {
                diagnostic!("An order template reply could not reach its window: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_order_template_command(
        &mut self,
        token: u64,
        result: Result<TemplateReply, TemplateFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let feedback = self.lifecycle.trading_feedback().clone();
        let ui = &mut self.pages.order_templates;
        let editor = ui
            .editor
            .as_mut()
            .filter(|editor| editor.pending == Some(token));
        match result {
            Ok(TemplateReply::Saved(template)) => {
                if editor.is_some() {
                    ui.editor = None;
                    self.focus_workspace(window, cx);
                }
                feedback.record(Ok(tr!(order_templates::SAVED, name = template.name)));
                self.select_order_template(order_template_key(&template.template_id), cx);
            }
            Ok(TemplateReply::Removed(template)) => {
                feedback.record(Ok(tr!(order_templates::DELETED, name = template.name)));
                if self.chrome.preferences().order_template
                    == order_template_key(&template.template_id)
                {
                    self.select_order_template(None, cx);
                }
            }
            Err(TemplateFailure::Save(error)) => match editor {
                Some(editor) => {
                    editor.pending = None;
                    editor.problem = Some(EditorProblem::Refused(error));
                }
                None => feedback.record(Err(order_template_error_text(&error))),
            },
            Err(TemplateFailure::Remove(error)) => {
                let detail = order_template_error_text(&error);
                feedback.record(Err(tr!(order_templates::DELETE_FAILED, error = detail)));
            }
        }
        cx.notify();
    }

    /// The open template dialog, if any.
    pub(super) fn order_templates_dialog_layer(
        &self,
        terminal: &Entity<Self>,
    ) -> Option<AnyElement> {
        let theme = self.theme;
        let ui = &self.pages.order_templates;
        if let Some(template) = ui.delete_confirmation.as_ref() {
            let cancel = terminal.clone();
            let confirm = terminal.clone();
            return Some(
                ConfirmationDialog::new(
                    "order_template_delete_dialog",
                    tr!(order_templates::DELETE_TITLE),
                    ConfirmationTone::Destructive,
                    &theme,
                    move |window, cx| {
                        cancel.update(cx, |terminal, cx| {
                            terminal.cancel_order_template_dialog(window, cx);
                        });
                    },
                    move |window, cx| {
                        confirm.update(cx, |terminal, cx| {
                            terminal.delete_order_template(window, cx);
                        });
                    },
                )
                .confirm_label(tr!(order_templates::DELETE_CONFIRM))
                .message(tr!(
                    order_templates::DELETE_MESSAGE,
                    name = template.name.as_str()
                ))
                .into_any_element(),
            );
        }
        let editor = ui.editor.as_ref()?;
        Some(order_template_editor_dialog(terminal, editor, &theme))
    }
}

fn field_label(label: impl Into<SharedString>, theme: &AerisTheme) -> Div {
    div()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(label.into())
}

/// A labelled editor input that grows to share its row.
fn editor_input(
    editor: &OrderTemplateEditor,
    field: EditorField,
    label: impl Into<SharedString>,
    theme: &AerisTheme,
) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(field_label(label, theme))
        .child(
            Input::new(editor.input(field))
                .platform(theme)
                .invalid(editor.marks(field)),
        )
}

/// The heading, one row per target and the button that adds one.
fn editor_targets(
    terminal: &Entity<TerminalApp>,
    editor: &OrderTemplateEditor,
    theme: &AerisTheme,
) -> Div {
    let add = terminal.clone();
    let rows = (0..editor.target_count).map(|target| {
        let remove = terminal.clone();
        let number = target + 1;
        div()
            .flex()
            .items_end()
            .gap_2()
            .child(editor_input(
                editor,
                EditorField::TargetTicks(target),
                tr!(order_templates::TARGET_TICKS_LABEL, number = number),
                theme,
            ))
            .child(editor_input(
                editor,
                EditorField::TargetQuantity(target),
                tr!(order_templates::TARGET_QUANTITY_LABEL),
                theme,
            ))
            .child(
                Button::new(("order_template_remove_target", target), theme)
                    .variant(ButtonVariant::Ghost)
                    .icon(header_icon(HugeIcon::Close))
                    .aria_label(tr!(order_templates::REMOVE_TARGET_ARIA, number = number))
                    .disabled(editor.target_count <= 1)
                    .on_click(move |_, window, cx| {
                        remove.update(cx, |terminal, terminal_cx| {
                            terminal.remove_order_template_target(target, window, terminal_cx);
                        });
                    }),
            )
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Emphasis))
                .text_color(gpui_color(theme.colors.text_default))
                .child(tr!(order_templates::TARGETS_HEADING)),
        )
        .children(rows)
        .child(
            div().flex().child(
                Button::new("order_template_add_target", theme)
                    .variant(ButtonVariant::Secondary)
                    .button_size(ButtonSize::Sm)
                    .icon(header_icon(HugeIcon::Add))
                    .label(tr!(order_templates::ADD_TARGET))
                    .disabled(editor.target_count >= MAXIMUM_BRACKET_TARGETS)
                    .on_click(move |_, _, cx| {
                        add.update(cx, TerminalApp::add_order_template_target);
                    }),
            ),
        )
}

/// The trailing stop switch and, while it is on, its activation and distance.
fn editor_trailing(
    terminal: &Entity<TerminalApp>,
    editor: &OrderTemplateEditor,
    theme: &AerisTheme,
) -> Div {
    let trailing = terminal.clone();
    let fields = editor.trailing.then(|| {
        div()
            .flex()
            .gap_2()
            .child(editor_input(
                editor,
                EditorField::TrailingActivation,
                tr!(order_templates::TRAILING_ACTIVATION_LABEL),
                theme,
            ))
            .child(editor_input(
                editor,
                EditorField::TrailingDistance,
                tr!(order_templates::TRAILING_DISTANCE_LABEL),
                theme,
            ))
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            SwitchRow::new(
                "order_template_trailing",
                tr!(order_templates::TRAILING_LABEL),
                editor.trailing,
                theme,
            )
            .description(tr!(order_templates::TRAILING_DESCRIPTION))
            .on_change(move |on, _, _, cx| {
                trailing.update(cx, |terminal, terminal_cx| {
                    terminal.set_order_template_trailing(on, terminal_cx);
                });
            }),
        )
        .children(fields)
}

fn order_template_editor_dialog(
    terminal: &Entity<TerminalApp>,
    editor: &OrderTemplateEditor,
    theme: &AerisTheme,
) -> AnyElement {
    let dismiss = terminal.clone();
    let cancel = terminal.clone();
    let save = terminal.clone();
    let title = if editor.editing {
        tr!(order_templates::EDIT_TITLE)
    } else {
        tr!(order_templates::NEW_TITLE)
    };
    let problem = editor.problem.as_ref().map(|problem| {
        div()
            .debug_selector(|| "order_template_problem".into())
            .text_xs()
            .text_color(gpui_color(theme.colors.text_danger))
            .child(problem.message())
    });
    Dialog::new(
        "order_template_dialog",
        DialogSize::Md,
        theme,
        move |window, cx| {
            dismiss.update(cx, |terminal, cx| {
                terminal.cancel_order_template_dialog(window, cx);
            });
        },
    )
    .title(title)
    .child(editor_input(
        editor,
        EditorField::Name,
        tr!(order_templates::NAME_LABEL),
        theme,
    ))
    .child(
        div()
            .flex()
            .gap_2()
            .child(editor_input(
                editor,
                EditorField::Quantity,
                tr!(order_templates::QUANTITY_LABEL),
                theme,
            ))
            .child(editor_input(
                editor,
                EditorField::Stop,
                tr!(order_templates::STOP_LABEL),
                theme,
            )),
    )
    .child(editor_targets(terminal, editor, theme))
    .child(editor_trailing(terminal, editor, theme))
    .children(problem)
    .action(
        Button::new("order_template_cancel", theme)
            .variant(ButtonVariant::Secondary)
            .label(tr!(common::CANCEL))
            .on_click(move |_, window, cx| {
                cancel.update(cx, |terminal, cx| {
                    terminal.cancel_order_template_dialog(window, cx);
                });
            }),
    )
    .action(
        Button::new("order_template_save", theme)
            .variant(ButtonVariant::Primary)
            .label(tr!(order_templates::SAVE))
            .loading(editor.pending.is_some())
            .disabled(editor.pending.is_some())
            .on_click(move |_, window, cx| {
                save.update(cx, |terminal, cx| terminal.save_order_template(window, cx));
            }),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn key(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("non-zero key")
    }

    fn template(value: u64, targets: &[(u32, u32)], trailing: bool) -> OrderTemplate {
        OrderTemplate {
            template_id: order_template_id(key(value)),
            revision: 1,
            name: format!("Template {value}"),
            quantity: targets.iter().map(|&(_, quantity)| quantity).sum(),
            stop_offset_ticks: 8,
            targets: targets
                .iter()
                .map(|&(offset_ticks, quantity)| OrderTemplateTarget {
                    offset_ticks,
                    quantity,
                })
                .collect(),
            trailing_stop: trailing.then_some(TrailingStopRule {
                activation_ticks: 6,
                distance_ticks: 4,
            }),
        }
    }

    #[test]
    fn a_choice_names_a_saved_template_or_refuses_one_that_is_gone() {
        let saved = template(0x2a, &[(16, 1)], false);
        let set = [saved.clone()];
        assert_eq!(
            OrderTemplateChoice::resolve(Some(&set), None),
            OrderTemplateChoice::None
        );
        let chosen = OrderTemplateChoice::resolve(Some(&set), Some(key(0x2a)));
        assert_eq!(chosen, OrderTemplateChoice::Template(saved));
        assert_eq!(chosen.template_id(), Ok(Some("user-2a".to_string())));
        for missing in [
            OrderTemplateChoice::resolve(Some(&set), Some(key(0x2b))),
            OrderTemplateChoice::resolve(None, Some(key(0x2a))),
        ] {
            assert_eq!(missing, OrderTemplateChoice::Missing);
            assert_eq!(
                missing.template_id(),
                Err(tr!(order_templates::SELECTED_MISSING))
            );
        }
    }

    #[test]
    fn the_ticket_uses_the_template_bracket_before_its_own() {
        let own = TicketBracket {
            stop_ticks: 4,
            target_ticks: 12,
        };
        assert_eq!(
            OrderTemplateChoice::None.ticket_protection(Some(own)),
            Ok(TicketProtection::Bracket(own))
        );
        assert_eq!(
            OrderTemplateChoice::None.ticket_protection(None),
            Ok(TicketProtection::None)
        );
        let chosen = OrderTemplateChoice::Template(template(0x2a, &[(8, 1), (16, 1)], false));
        assert_eq!(
            chosen.ticket_protection(Some(own)),
            Ok(TicketProtection::Template("user-2a".to_string()))
        );
        assert!(
            OrderTemplateChoice::Missing
                .ticket_protection(Some(own))
                .is_err(),
            "a vanished template never falls back to an unprotected or other bracket"
        );
    }

    #[test]
    fn a_replay_trades_only_a_single_target_template_without_trailing() {
        let own = TicketBracket {
            stop_ticks: 4,
            target_ticks: 12,
        };
        assert_eq!(
            OrderTemplateChoice::None.replay_bracket(Some(own)),
            Ok(Some(own))
        );
        assert_eq!(
            OrderTemplateChoice::Template(template(1, &[(16, 2)], false)).replay_bracket(None),
            Ok(Some(TicketBracket {
                stop_ticks: 8,
                target_ticks: 16
            }))
        );
        for unsupported in [
            template(1, &[(8, 1), (16, 1)], false),
            template(1, &[(16, 1)], true),
        ] {
            assert_eq!(
                OrderTemplateChoice::Template(unsupported).replay_bracket(Some(own)),
                Err(tr!(order_templates::REPLAY_UNSUPPORTED))
            );
        }
        assert_eq!(
            OrderTemplateChoice::Missing.replay_bracket(None),
            Err(tr!(order_templates::SELECTED_MISSING))
        );
    }

    #[test]
    fn fresh_identities_step_past_saved_ones() {
        let saved = vec![
            template(0x10, &[(16, 1)], false),
            template(0x11, &[(16, 1)], false),
        ];
        assert_eq!(fresh_order_template_id(0x10, &saved), "user-12");
        assert_eq!(fresh_order_template_id(0x20, &saved), "user-20");
        assert_eq!(fresh_order_template_id(-5, &[]), "user-1");
    }

    fn texts(values: &[(EditorField, &str)]) -> impl Fn(EditorField) -> String {
        let values = values
            .iter()
            .map(|&(field, text)| (field.index(), text.to_string()))
            .collect::<HashMap<_, _>>();
        move |field| values.get(&field.index()).cloned().unwrap_or_default()
    }

    #[test]
    fn the_editor_builds_the_template_its_fields_describe() {
        let fields = texts(&[
            (EditorField::Name, "  Scalp  "),
            (EditorField::Quantity, "3"),
            (EditorField::Stop, "8"),
            (EditorField::TargetTicks(0), "4"),
            (EditorField::TargetQuantity(0), "1"),
            (EditorField::TargetTicks(1), "12"),
            (EditorField::TargetQuantity(1), "2"),
            // A hidden row and switched-off trailing fields are not read.
            (EditorField::TargetTicks(2), "x"),
            (EditorField::TrailingActivation, "x"),
        ]);
        let built =
            draft_template("user-2a".to_string(), 3, 2, false, &fields).expect("a valid template");
        assert_eq!(
            built,
            OrderTemplate {
                template_id: "user-2a".to_string(),
                revision: 3,
                name: "Scalp".to_string(),
                quantity: 3,
                stop_offset_ticks: 8,
                targets: vec![
                    OrderTemplateTarget {
                        offset_ticks: 4,
                        quantity: 1
                    },
                    OrderTemplateTarget {
                        offset_ticks: 12,
                        quantity: 2
                    },
                ],
                trailing_stop: None,
            }
        );
        assert_eq!(
            draft_template("user-2a".to_string(), 3, 2, true, &fields),
            Err(EditorProblem::NotANumber(EditorField::TrailingActivation))
        );
    }

    #[test]
    fn the_editor_names_the_refused_field() {
        let base = [
            (EditorField::Name, "Scalp"),
            (EditorField::Quantity, "2"),
            (EditorField::Stop, "8"),
            (EditorField::TargetTicks(0), "4"),
            (EditorField::TargetQuantity(0), "1"),
        ];
        let refused = |change: (EditorField, &'static str)| {
            let mut values = base.to_vec();
            values.push(change);
            draft_template("user-2a".to_string(), 1, 1, false, texts(&values)).expect_err("refused")
        };
        let not_a_number = refused((EditorField::Stop, "-8"));
        assert_eq!(not_a_number, EditorProblem::NotANumber(EditorField::Stop));
        assert!(not_a_number.marks(EditorField::Stop));
        assert!(!not_a_number.marks(EditorField::Quantity));
        let total = refused((EditorField::Quantity, "2"));
        assert_eq!(
            total,
            EditorProblem::Refused(OrderTemplateError::Invalid(
                OrderTemplateInvalid::TargetTotal
            ))
        );
        assert!(total.marks(EditorField::Quantity));
        assert!(total.marks(EditorField::TargetQuantity(0)));
        assert!(!total.marks(EditorField::Name));
        let name = refused((EditorField::Name, "   "));
        assert!(name.marks(EditorField::Name));
        assert_eq!(
            name.message(),
            tr!(
                order_templates::INVALID_NAME,
                maximum = MAXIMUM_ORDER_TEMPLATE_NAME_CHARS
            )
        );
    }

    #[test]
    fn every_refusal_reads_as_its_own_text() {
        let texts = [
            OrderTemplateError::TooManyTemplates,
            OrderTemplateError::UnknownTemplate,
            OrderTemplateError::RevisionMustAdvance { current: 3 },
            OrderTemplateError::Owner("disk full".to_string()),
        ]
        .iter()
        .map(order_template_error_text)
        .collect::<Vec<_>>();
        assert_eq!(
            texts[0],
            "At most 64 order templates can be saved. Delete one first."
        );
        assert_eq!(texts[3], "The template could not be saved: disk full");
        let distinct = texts.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), texts.len());
    }

    #[test]
    fn an_edited_template_opens_with_its_own_values() {
        let saved = template(0x2a, &[(4, 1), (12, 2)], true);
        let values = draft_values(Some(&saved));
        let built = draft_template(
            saved.template_id.clone(),
            saved.revision,
            saved.targets.len(),
            true,
            |field| {
                values
                    .iter()
                    .find(|(candidate, _)| *candidate == field)
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default()
            },
        );
        assert_eq!(built, Ok(saved));
        let fields = [
            EditorField::Name,
            EditorField::Quantity,
            EditorField::Stop,
            EditorField::TrailingActivation,
            EditorField::TrailingDistance,
        ]
        .into_iter()
        .chain((0..MAXIMUM_BRACKET_TARGETS).flat_map(|target| {
            [
                EditorField::TargetTicks(target),
                EditorField::TargetQuantity(target),
            ]
        }))
        .map(EditorField::index)
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(fields.len(), EDITOR_FIELD_COUNT);
        assert_eq!(fields.last(), Some(&(EDITOR_FIELD_COUNT - 1)));
    }
}
