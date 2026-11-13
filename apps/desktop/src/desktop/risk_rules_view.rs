//! Risk rules: the Accounts panel section that edits a practice account's risk profile (the
//! daily limits plus the prop-firm daily profit target and maximum drawdown) and shows the
//! owner's live meter. The trading owner validates, stores and enforces every rule; this
//! section edits a draft and sends only the profile the user saved.

use super::accounts_panel::ACCOUNTS_FIELD_WIDTH;
use super::big_trades_dialog::{control_strip, described_setting_row, settings_group};
use super::order_ticket::format_money;
use super::trade_copier_view::{
    account_name, decimal_text, is_practice, next_revision, parse_positive_decimal,
    practice_account,
};
use super::*;
use aeris_localization::{risk_rules, tr};
use aeris_trading::{FixedPoint, TradingAccount, TradingAccountId};
use aeris_trading_runtime::{MaximumDrawdownRule, RiskProfile, TrailingDrawdownMode, WorkingSet};

/// The identity of a profile first saved from this section; a saved profile keeps its own.
const DEFAULT_PROFILE_ID: &str = "risk-rules";

/// The section's text fields, in the order the editor holds their inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuleField {
    DailyLossLimit,
    MaximumContracts,
    TrailingDrawdown,
    ConsistencyPercent,
    ProfitTarget,
    MaximumDrawdown,
    FloorStop,
}

impl RuleField {
    const ALL: [Self; 7] = [
        Self::DailyLossLimit,
        Self::MaximumContracts,
        Self::TrailingDrawdown,
        Self::ConsistencyPercent,
        Self::ProfitTarget,
        Self::MaximumDrawdown,
        Self::FloorStop,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::DailyLossLimit => tr!(risk_rules::PLACEHOLDER_AMOUNT),
            Self::MaximumContracts => tr!(risk_rules::PLACEHOLDER_CONTRACTS),
            _ => tr!(risk_rules::PLACEHOLDER_OFF),
        }
    }
}

/// An exact positive amount in the account currency: at most `scale` decimals, rescaled to
/// that scale, so no value is rounded.
fn parse_money(text: &str, scale: u8) -> Option<FixedPoint> {
    parse_positive_decimal(text)
        .filter(|value| value.scale() <= scale)
        .and_then(|value| value.exact_rescale(scale).ok())
}

fn parse_percent(text: &str) -> Option<u8> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u8>()
        .ok()
        .filter(|percent| (1..=100).contains(percent))
}

/// The editor's fields as typed.
#[derive(Clone, Debug, PartialEq)]
struct RiskRulesDraft {
    enabled: bool,
    trailing_mode: TrailingDrawdownMode,
    drawdown_mode: TrailingDrawdownMode,
    texts: [String; RuleField::ALL.len()],
}

impl RiskRulesDraft {
    fn text(&self, field: RuleField) -> &str {
        self.texts[field.index()].trim()
    }
}

/// The draft a saved profile reopens as, or an empty enabled one.
fn saved_draft(saved: Option<&RiskProfile>) -> RiskRulesDraft {
    let Some(profile) = saved else {
        return RiskRulesDraft {
            enabled: true,
            trailing_mode: TrailingDrawdownMode::Intraday,
            drawdown_mode: TrailingDrawdownMode::Intraday,
            texts: Default::default(),
        };
    };
    let optional = |value: Option<FixedPoint>| value.map(decimal_text).unwrap_or_default();
    let drawdown = profile.maximum_drawdown;
    RiskRulesDraft {
        enabled: profile.enabled,
        trailing_mode: profile.trailing_mode,
        drawdown_mode: drawdown.map_or(TrailingDrawdownMode::Intraday, |rule| rule.mode),
        texts: [
            decimal_text(profile.daily_loss_limit),
            decimal_text(profile.max_contracts),
            optional(profile.trailing_drawdown),
            profile
                .consistency_max_single_trade_percent
                .map(|percent| percent.to_string())
                .unwrap_or_default(),
            optional(profile.daily_profit_target),
            optional(drawdown.map(|rule| rule.amount)),
            optional(drawdown.and_then(|rule| rule.floor_stops_at)),
        ],
    }
}

/// Where a newly saved profile's first risk session starts: now, with the account's current
/// realized P&L as its baseline, so it measures only what follows the save.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionStart {
    unix_nanos: i64,
    realized_pnl: FixedPoint,
}

/// Why the editor could not save its rules.
#[derive(Clone, Debug, PartialEq)]
enum RiskRulesProblem {
    NoAccount,
    Field(RuleField),
    FloorStopWithoutDrawdown,
    RevisionExhausted,
    Refused(String),
}

impl RiskRulesProblem {
    fn marks(&self, field: RuleField) -> bool {
        match self {
            Self::Field(marked) => *marked == field,
            Self::FloorStopWithoutDrawdown => field == RuleField::FloorStop,
            _ => false,
        }
    }

    fn message(&self) -> String {
        let text = match self {
            Self::NoAccount => tr!(risk_rules::PROBLEM_NO_ACCOUNT),
            Self::Field(RuleField::DailyLossLimit) => tr!(risk_rules::PROBLEM_DAILY_LOSS),
            Self::Field(RuleField::MaximumContracts) => tr!(risk_rules::PROBLEM_MAXIMUM_CONTRACTS),
            Self::Field(RuleField::TrailingDrawdown) => tr!(risk_rules::PROBLEM_TRAILING),
            Self::Field(RuleField::ConsistencyPercent) => tr!(risk_rules::PROBLEM_CONSISTENCY),
            Self::Field(RuleField::ProfitTarget) => tr!(risk_rules::PROBLEM_PROFIT_TARGET),
            Self::Field(RuleField::MaximumDrawdown) => tr!(risk_rules::PROBLEM_MAXIMUM_DRAWDOWN),
            Self::Field(RuleField::FloorStop) => tr!(risk_rules::PROBLEM_FLOOR_STOP),
            Self::FloorStopWithoutDrawdown => {
                tr!(risk_rules::PROBLEM_FLOOR_STOP_WITHOUT_DRAWDOWN)
            }
            Self::RevisionExhausted => tr!(risk_rules::PROBLEM_REVISION_EXHAUSTED),
            Self::Refused(error) => return tr!(risk_rules::SAVE_FAILED, error = error),
        };
        text.to_string()
    }
}

/// The profile the editor's fields describe, checked by the owner's own validation so the
/// editor refuses what the owner would; the owner stays the authority on save. A revision
/// keeps the saved profile's identity, session start and the rules this section does not
/// edit (the news restriction and the economic-event rule).
fn draft_profile(
    account: Option<&TradingAccount>,
    saved: Option<&RiskProfile>,
    version: u32,
    new_session: SessionStart,
    draft: &RiskRulesDraft,
) -> Result<RiskProfile, RiskRulesProblem> {
    let account = account
        .filter(|account| is_practice(account))
        .ok_or(RiskRulesProblem::NoAccount)?;
    let scale = account.currency_scale;
    let money = |field: RuleField| {
        parse_money(draft.text(field), scale).ok_or(RiskRulesProblem::Field(field))
    };
    let optional_money = |field: RuleField| {
        if draft.text(field).is_empty() {
            Ok(None)
        } else {
            money(field).map(Some)
        }
    };
    let daily_loss_limit = money(RuleField::DailyLossLimit)?;
    let max_contracts = parse_positive_decimal(draft.text(RuleField::MaximumContracts))
        .ok_or(RiskRulesProblem::Field(RuleField::MaximumContracts))?;
    let trailing_drawdown = optional_money(RuleField::TrailingDrawdown)?;
    let consistency = draft.text(RuleField::ConsistencyPercent);
    let consistency_max_single_trade_percent = if consistency.is_empty() {
        None
    } else {
        Some(
            parse_percent(consistency)
                .ok_or(RiskRulesProblem::Field(RuleField::ConsistencyPercent))?,
        )
    };
    let daily_profit_target = optional_money(RuleField::ProfitTarget)?;
    let floor_stops_at = optional_money(RuleField::FloorStop)?;
    let maximum_drawdown = match (optional_money(RuleField::MaximumDrawdown)?, floor_stops_at) {
        (Some(amount), floor_stops_at) => Some(MaximumDrawdownRule {
            amount,
            mode: draft.drawdown_mode,
            floor_stops_at,
        }),
        (None, Some(_)) => return Err(RiskRulesProblem::FloorStopWithoutDrawdown),
        (None, None) => None,
    };
    let session = match saved {
        Some(profile) => SessionStart {
            unix_nanos: profile.session_start_unix_nanos,
            realized_pnl: profile
                .session_start_realized_pnl
                .exact_rescale(scale)
                .map_err(|error| RiskRulesProblem::Refused(error.to_string()))?,
        },
        None => new_session,
    };
    let profile = RiskProfile {
        account_id: account.id.clone(),
        profile_id: saved.map_or_else(
            || DEFAULT_PROFILE_ID.to_string(),
            |profile| profile.profile_id.clone(),
        ),
        version,
        session_start_unix_nanos: session.unix_nanos,
        session_start_realized_pnl: session.realized_pnl,
        daily_loss_limit,
        trailing_drawdown,
        trailing_mode: draft.trailing_mode,
        max_contracts,
        consistency_max_single_trade_percent,
        restricted_until_unix_nanos: saved.and_then(|profile| profile.restricted_until_unix_nanos),
        economic_event_rule: saved.and_then(|profile| profile.economic_event_rule),
        daily_profit_target,
        maximum_drawdown,
        enabled: draft.enabled,
    };
    profile.validate().map_err(RiskRulesProblem::Refused)?;
    Ok(profile)
}

fn saved_profile<'a>(
    profiles: &'a [RiskProfile],
    account_id: &TradingAccountId,
) -> Option<&'a RiskProfile> {
    profiles
        .iter()
        .find(|profile| &profile.account_id == account_id)
}

/// The account the section opens on: the account this pane trades on when it is a practice
/// account, else the first practice account with saved rules, else the first practice account.
fn default_rules_account(
    accounts: &[TradingAccount],
    profiles: &[RiskProfile],
    selected: Option<&TradingAccountId>,
) -> Option<TradingAccountId> {
    selected
        .filter(|id| practice_account(accounts, id).is_some())
        .cloned()
        .or_else(|| {
            profiles
                .iter()
                .map(|profile| &profile.account_id)
                .find(|id| practice_account(accounts, id).is_some())
                .cloned()
        })
        .or_else(|| {
            accounts
                .iter()
                .find(|account| is_practice(account))
                .map(|account| account.id.clone())
        })
}

/// The first session of a new profile, from the working set's account P&L.
fn new_session_start(
    set: Option<&WorkingSet>,
    account: Option<&TradingAccount>,
    now_unix_nanos: i64,
) -> Option<SessionStart> {
    let account = account?;
    let realized_pnl = set
        .and_then(|set| {
            set.account_pnl
                .iter()
                .find(|pnl| pnl.account_id == account.id)
        })
        .map_or_else(
            || FixedPoint::try_new(0, account.currency_scale).ok(),
            |pnl| pnl.realized.exact_rescale(account.currency_scale).ok(),
        )?;
    Some(SessionStart {
        unix_nanos: now_unix_nanos,
        realized_pnl,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RiskRulesMenu {
    Account,
    TrailingMode,
    DrawdownMode,
}

struct RuleInput {
    input: Entity<InputState>,
    _input_events: gpui::Subscription,
}

/// The open risk-rules section's draft, the command in flight and its last outcome.
pub(super) struct RiskRulesEditor {
    account: Option<TradingAccountId>,
    enabled: bool,
    trailing_mode: TrailingDrawdownMode,
    drawdown_mode: TrailingDrawdownMode,
    inputs: Vec<RuleInput>,
    menu: Option<RiskRulesMenu>,
    problem: Option<RiskRulesProblem>,
    /// The save in flight, which holds further saves until its reply.
    pending: Option<u64>,
    next_command: u64,
    last_saved_version: Option<u32>,
}

impl RiskRulesEditor {
    fn draft(&self, cx: &App) -> RiskRulesDraft {
        RiskRulesDraft {
            enabled: self.enabled,
            trailing_mode: self.trailing_mode,
            drawdown_mode: self.drawdown_mode,
            texts: std::array::from_fn(|index| {
                self.inputs[index].input.read(cx).value().to_string()
            }),
        }
    }

    fn input(&self, field: RuleField) -> &Entity<InputState> {
        &self.inputs[field.index()].input
    }
}

#[cfg(test)]
impl RiskRulesEditor {
    pub(super) fn account(&self) -> Option<&TradingAccountId> {
        self.account.as_ref()
    }

    pub(super) fn daily_loss_input(&self) -> &Entity<InputState> {
        self.input(RuleField::DailyLossLimit)
    }

    pub(super) fn maximum_contracts_input(&self) -> &Entity<InputState> {
        self.input(RuleField::MaximumContracts)
    }

    pub(super) fn profit_target_input(&self) -> &Entity<InputState> {
        self.input(RuleField::ProfitTarget)
    }

    pub(super) fn maximum_drawdown_input(&self) -> &Entity<InputState> {
        self.input(RuleField::MaximumDrawdown)
    }

    pub(super) const fn is_saving(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) const fn has_problem(&self) -> bool {
        self.problem.is_some()
    }
}

fn rule_input(
    field: RuleField,
    value: String,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> RuleInput {
    let input =
        cx.new(|input_cx| InputState::new(window, input_cx).placeholder(field.placeholder()));
    input.update(cx, |input, input_cx| {
        input.set_value(value, window, input_cx);
    });
    let input_events = cx.subscribe_in(
        &input,
        window,
        |surface, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => surface.save_risk_rules(window, cx),
            InputEvent::Change => surface.risk_rules_edited(cx),
            InputEvent::Blur | InputEvent::Focus => {}
        },
    );
    RuleInput {
        input,
        _input_events: input_events,
    }
}

impl WorkspaceSurface {
    fn risk_rules_working_set<'a>(&self, cx: &'a App) -> Option<&'a WorkingSet> {
        self.lifecycle.trading_store()?.read(cx).current()
    }

    /// Opens the section on its default account unless it is already open.
    pub(super) fn open_risk_rules_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.trading_pnl.risk_rules.is_some() {
            return;
        }
        let account = self.risk_rules_working_set(cx).and_then(|set| {
            default_rules_account(
                &set.accounts,
                &set.risk_profiles,
                self.trading_pnl.order_entry.selected_account_id.as_ref(),
            )
        });
        self.load_risk_rules(account, window, cx);
    }

    /// Replaces the draft with `account`'s saved rules, or an empty draft when none are saved.
    fn load_risk_rules(
        &mut self,
        account: Option<TradingAccountId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = account.as_ref().and_then(|account| {
            self.risk_rules_working_set(cx)
                .and_then(|set| saved_profile(&set.risk_profiles, account))
                .cloned()
        });
        let draft = saved_draft(saved.as_ref());
        let inputs = RuleField::ALL
            .into_iter()
            .map(|field| rule_input(field, draft.texts[field.index()].clone(), window, cx))
            .collect();
        let next_command = self
            .trading_pnl
            .risk_rules
            .as_ref()
            .map_or(0, |editor| editor.next_command);
        self.trading_pnl.risk_rules = Some(RiskRulesEditor {
            account,
            enabled: draft.enabled,
            trailing_mode: draft.trailing_mode,
            drawdown_mode: draft.drawdown_mode,
            inputs,
            menu: None,
            problem: None,
            pending: None,
            next_command,
            last_saved_version: None,
        });
        cx.notify();
    }

    fn risk_rules_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.risk_rules.as_mut() {
            editor.problem = None;
            cx.notify();
        }
    }

    fn toggle_risk_rules_menu(&mut self, menu: RiskRulesMenu, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.risk_rules.as_mut() {
            editor.menu = (editor.menu != Some(menu)).then_some(menu);
            cx.notify();
        }
    }

    fn close_risk_rules_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.risk_rules.as_mut()
            && editor.menu.take().is_some()
        {
            cx.notify();
        }
    }

    fn choose_risk_rules_account(
        &mut self,
        account: TradingAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unchanged = self
            .trading_pnl
            .risk_rules
            .as_ref()
            .is_some_and(|editor| editor.account.as_ref() == Some(&account));
        if !unchanged {
            self.load_risk_rules(Some(account), window, cx);
        }
    }

    fn set_risk_rules_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.risk_rules.as_mut() {
            editor.enabled = enabled;
            editor.problem = None;
            cx.notify();
        }
    }

    fn set_risk_rules_trailing_mode(
        &mut self,
        mode: TrailingDrawdownMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.trading_pnl.risk_rules.as_mut() {
            editor.trailing_mode = mode;
            editor.problem = None;
            cx.notify();
        }
    }

    fn set_risk_rules_drawdown_mode(
        &mut self,
        mode: TrailingDrawdownMode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.trading_pnl.risk_rules.as_mut() {
            editor.drawdown_mode = mode;
            editor.problem = None;
            cx.notify();
        }
    }

    /// Checks the draft and sends it to the trading owner, or shows why it cannot be saved.
    pub(super) fn save_risk_rules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.trading_pnl.risk_rules.as_ref() else {
            return;
        };
        if editor.pending.is_some() {
            return;
        }
        let working_set = self.risk_rules_working_set(cx);
        let accounts = working_set.map_or(&[][..], |set| set.accounts.as_slice());
        let account = editor
            .account
            .as_ref()
            .and_then(|account| practice_account(accounts, account));
        let saved = editor.account.as_ref().and_then(|account| {
            working_set.and_then(|set| saved_profile(&set.risk_profiles, account))
        });
        let now = self.lifecycle.clock().wall_unix_nanos();
        let draft = new_session_start(working_set, account, now)
            .ok_or(RiskRulesProblem::NoAccount)
            .and_then(|session| {
                next_revision(
                    saved.map(|profile| profile.version),
                    editor.last_saved_version,
                )
                .ok_or(RiskRulesProblem::RevisionExhausted)
                .and_then(|version| {
                    draft_profile(account, saved, version, session, &editor.draft(cx))
                })
            });
        let Some(editor) = self.trading_pnl.risk_rules.as_mut() else {
            return;
        };
        editor.menu = None;
        let profile = match draft {
            Ok(profile) => profile,
            Err(problem) => {
                editor.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        editor.next_command += 1;
        let token = editor.next_command;
        editor.pending = Some(token);
        editor.problem = None;
        let service = self.lifecycle.trading().clone();
        cx.spawn_in(window, async move |surface, cx| {
            let version = profile.version;
            let result = cx
                .background_executor()
                .spawn(async move {
                    service
                        .register_risk_profile(profile)
                        .map(|()| version)
                        .map_err(RiskRulesProblem::Refused)
                })
                .await;
            if let Err(error) = surface.update_in(cx, |surface, _, surface_cx| {
                surface.finish_risk_rules_save(token, result, surface_cx);
            }) && surface.upgrade().is_some()
            {
                diagnostic!("A risk rules reply could not reach its pane: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_risk_rules_save(
        &mut self,
        token: u64,
        result: Result<u32, RiskRulesProblem>,
        cx: &mut Context<Self>,
    ) {
        let feedback = self.lifecycle.trading_feedback().clone();
        let editor = self
            .trading_pnl
            .risk_rules
            .as_mut()
            .filter(|editor| editor.pending == Some(token));
        match (editor, result) {
            (Some(editor), Ok(version)) => {
                editor.pending = None;
                editor.last_saved_version = Some(version);
                feedback.record(Ok(tr!(risk_rules::SAVED).to_string()));
            }
            (Some(editor), Err(problem)) => {
                editor.pending = None;
                editor.problem = Some(problem);
            }
            (None, Ok(_)) => feedback.record(Ok(tr!(risk_rules::SAVED).to_string())),
            // The editor that sent the save has closed, so its outcome goes to the pane's
            // feedback line.
            (None, Err(problem)) => feedback.record(Err(problem.message())),
        }
        cx.notify();
    }
}

/// The section's groups: the account and whether its rules are enforced, the daily limits,
/// the maximum drawdown with save, and the owner's live meter.
pub(super) fn risk_rules_groups(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    working_set: Option<&WorkingSet>,
    theme: &AerisTheme,
    cx: &App,
) -> Vec<AnyElement> {
    let accounts = working_set.map_or(&[][..], |set| set.accounts.as_slice());
    let profiles = working_set.map_or(&[][..], |set| set.risk_profiles.as_slice());
    let editor = app_state.trading_pnl.risk_rules.as_ref();
    let mut rows = vec![note(tr!(risk_rules::INTRO), theme.colors.text_secondary)];
    if !accounts.iter().any(is_practice) {
        rows.push(note(
            tr!(risk_rules::NO_PRACTICE_ACCOUNTS),
            theme.colors.text_secondary,
        ));
    }
    let Some(editor) = editor else {
        return vec![settings_group(tr!(risk_rules::HEADING), rows, theme)];
    };
    rows.push(described_setting_row(
        tr!(risk_rules::ACCOUNT_LABEL),
        [SharedString::from(tr!(risk_rules::ACCOUNT_DESCRIPTION))],
        account_dropdown(app, editor, accounts, theme),
        theme,
    ));
    let Some(account) = editor
        .account
        .as_ref()
        .and_then(|account| practice_account(accounts, account))
    else {
        return vec![settings_group(tr!(risk_rules::HEADING), rows, theme)];
    };
    let saved = saved_profile(profiles, &account.id);
    let status = match saved {
        None => tr!(risk_rules::STATUS_NOT_SAVED),
        Some(profile) if profile.enabled => tr!(risk_rules::STATUS_ENABLED),
        Some(_) => tr!(risk_rules::STATUS_DISABLED),
    };
    let toggle = app.clone();
    rows.push(described_setting_row(
        tr!(risk_rules::ENABLED_LABEL),
        [SharedString::from(status)],
        Switch::new(
            "risk_rules_enabled",
            tr!(risk_rules::ENABLED_LABEL),
            editor.enabled,
            theme,
        )
        .on_change(move |on, _, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.set_risk_rules_enabled(on, surface_cx);
            });
        })
        .into_any_element(),
        theme,
    ));
    let mut drawdown = drawdown_rows(app, editor, theme);
    drawdown.extend(save_rows(app, editor, account, saved, theme, cx));
    vec![
        settings_group(tr!(risk_rules::HEADING), rows, theme),
        settings_group(
            tr!(risk_rules::DAILY_HEADING),
            daily_rows(app, editor, theme),
            theme,
        ),
        settings_group(tr!(risk_rules::DRAWDOWN_HEADING), drawdown, theme),
        settings_group(
            tr!(risk_rules::METER_HEADING),
            meter_rows(app, working_set, account, theme),
            theme,
        ),
    ]
}

fn field_row(
    editor: &RiskRulesEditor,
    field: RuleField,
    label: &'static str,
    description: &'static str,
    theme: &AerisTheme,
) -> AnyElement {
    described_setting_row(
        label,
        [SharedString::from(description)],
        rule_field(editor, field, theme),
        theme,
    )
}

fn daily_rows(
    app: &Entity<WorkspaceSurface>,
    editor: &RiskRulesEditor,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let field = |field, label, description| field_row(editor, field, label, description, theme);
    vec![
        field(
            RuleField::DailyLossLimit,
            tr!(risk_rules::DAILY_LOSS_LABEL),
            tr!(risk_rules::DAILY_LOSS_DESCRIPTION),
        ),
        field(
            RuleField::MaximumContracts,
            tr!(risk_rules::MAXIMUM_CONTRACTS_LABEL),
            tr!(risk_rules::MAXIMUM_CONTRACTS_DESCRIPTION),
        ),
        field(
            RuleField::TrailingDrawdown,
            tr!(risk_rules::TRAILING_LABEL),
            tr!(risk_rules::TRAILING_DESCRIPTION),
        ),
        described_setting_row(
            tr!(risk_rules::TRAILING_MODE_LABEL),
            [SharedString::from(tr!(
                risk_rules::TRAILING_MODE_DESCRIPTION
            ))],
            mode_dropdown(
                app,
                editor,
                RiskRulesMenu::TrailingMode,
                editor.trailing_mode,
                WorkspaceSurface::set_risk_rules_trailing_mode,
                theme,
            ),
            theme,
        ),
        field(
            RuleField::ConsistencyPercent,
            tr!(risk_rules::CONSISTENCY_LABEL),
            tr!(risk_rules::CONSISTENCY_DESCRIPTION),
        ),
        field(
            RuleField::ProfitTarget,
            tr!(risk_rules::PROFIT_TARGET_LABEL),
            tr!(risk_rules::PROFIT_TARGET_DESCRIPTION),
        ),
    ]
}

fn drawdown_rows(
    app: &Entity<WorkspaceSurface>,
    editor: &RiskRulesEditor,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let field = |field, label, description| field_row(editor, field, label, description, theme);
    vec![
        note(tr!(risk_rules::DRAWDOWN_INTRO), theme.colors.text_secondary),
        field(
            RuleField::MaximumDrawdown,
            tr!(risk_rules::MAXIMUM_DRAWDOWN_LABEL),
            tr!(risk_rules::MAXIMUM_DRAWDOWN_DESCRIPTION),
        ),
        described_setting_row(
            tr!(risk_rules::DRAWDOWN_MODE_LABEL),
            [SharedString::from(tr!(
                risk_rules::DRAWDOWN_MODE_DESCRIPTION
            ))],
            mode_dropdown(
                app,
                editor,
                RiskRulesMenu::DrawdownMode,
                editor.drawdown_mode,
                WorkspaceSurface::set_risk_rules_drawdown_mode,
                theme,
            ),
            theme,
        ),
        field(
            RuleField::FloorStop,
            tr!(risk_rules::FLOOR_STOP_LABEL),
            tr!(risk_rules::FLOOR_STOP_DESCRIPTION),
        ),
    ]
}

fn note(text: impl Into<SharedString>, color: ThemeColor) -> AnyElement {
    div()
        .py_1()
        .text_sm()
        .text_color(gpui_color(color))
        .child(text.into())
        .into_any_element()
}

fn rule_field(editor: &RiskRulesEditor, field: RuleField, theme: &AerisTheme) -> AnyElement {
    let marked = editor
        .problem
        .as_ref()
        .is_some_and(|problem| problem.marks(field));
    div()
        .w(design_rems(SETTINGS_CONTROL_WIDTH))
        .h(px(32.0))
        .flex()
        .child(
            Input::new(editor.input(field))
                .platform(theme)
                .invalid(marked)
                .flex_1(),
        )
        .into_any_element()
}

impl RiskRulesMenu {
    const fn id(self) -> &'static str {
        match self {
            Self::Account => "risk_rules_account",
            Self::TrailingMode => "risk_rules_trailing_mode",
            Self::DrawdownMode => "risk_rules_drawdown_mode",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Account => tr!(risk_rules::ACCOUNT_LABEL),
            Self::TrailingMode => tr!(risk_rules::TRAILING_MODE_LABEL),
            Self::DrawdownMode => tr!(risk_rules::DRAWDOWN_MODE_LABEL),
        }
    }

    /// Account names are wider than a setting value; a mode matches the inputs beside it.
    fn width(self) -> gpui::Length {
        match self {
            Self::Account => px(ACCOUNTS_FIELD_WIDTH).into(),
            Self::TrailingMode | Self::DrawdownMode => design_rems(SETTINGS_CONTROL_WIDTH).into(),
        }
    }
}

/// The dropdown `menu` showing `current`, disabled with no `choices`; picking one closes it
/// before `choose` runs.
fn choice_menu<T: Clone + 'static>(
    app: &Entity<WorkspaceSurface>,
    editor: &RiskRulesEditor,
    menu: RiskRulesMenu,
    current: String,
    choices: Vec<(T, String, bool)>,
    choose: fn(&mut WorkspaceSurface, T, &mut Window, &mut Context<WorkspaceSurface>),
    theme: &AerisTheme,
) -> AnyElement {
    let id = menu.id();
    let open = editor.menu == Some(menu);
    let toggle = app.clone();
    let trigger = Button::new(id, theme)
        .variant(ButtonVariant::Secondary)
        .trigger()
        .full_width()
        .open(open)
        .disabled(choices.is_empty())
        .aria_label(menu.label())
        .leading(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_default))
                .child(current),
        )
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_risk_rules_menu(menu, surface_cx);
            });
        });
    let panel = open.then(|| {
        MenuPanel::new(
            SharedString::from(format!("{id}_menu")),
            MenuPlacement::Anchored {
                side: MenuSide::Below,
                align: MenuAlign::Stretch,
            },
            theme,
        )
        .animate_from(PopupAnimationOrigin::TOP_LEFT)
        .children(choices.into_iter().enumerate().map(
            |(index, (value, label, checked))| {
                let pick = app.clone();
                MenuRow::compact((id, index), label, theme)
                    .checked(checked)
                    .on_click(move |_, window, cx| {
                        pick.update(cx, |surface, surface_cx| {
                            surface.close_risk_rules_menu(surface_cx);
                            choose(surface, value.clone(), window, surface_cx);
                        });
                    })
            },
        ))
    });
    let dismiss = app.clone();
    div()
        .w(menu.width())
        .flex_none()
        .child(
            MenuAnchor::new(SharedString::from(format!("{id}_anchor")), trigger)
                .full_width()
                .menu(panel)
                .on_dismiss(move |_, cx| {
                    dismiss.update(cx, WorkspaceSurface::close_risk_rules_menu);
                }),
        )
        .into_any_element()
}

fn account_dropdown(
    app: &Entity<WorkspaceSurface>,
    editor: &RiskRulesEditor,
    accounts: &[TradingAccount],
    theme: &AerisTheme,
) -> AnyElement {
    let choices = accounts
        .iter()
        .filter(|account| is_practice(account))
        .map(|account| {
            (
                account.id.clone(),
                account.display_name.clone(),
                editor.account.as_ref() == Some(&account.id),
            )
        })
        .collect::<Vec<_>>();
    let current = editor.account.as_ref().map_or_else(
        || tr!(risk_rules::CHOOSE_ACCOUNT).to_string(),
        |account| account_name(accounts, account),
    );
    choice_menu(
        app,
        editor,
        RiskRulesMenu::Account,
        current,
        choices,
        WorkspaceSurface::choose_risk_rules_account,
        theme,
    )
}

fn mode_label(mode: TrailingDrawdownMode) -> &'static str {
    match mode {
        TrailingDrawdownMode::Intraday => tr!(risk_rules::MODE_INTRADAY),
        TrailingDrawdownMode::EndOfDay => tr!(risk_rules::MODE_END_OF_DAY),
    }
}

fn mode_dropdown(
    app: &Entity<WorkspaceSurface>,
    editor: &RiskRulesEditor,
    menu: RiskRulesMenu,
    current: TrailingDrawdownMode,
    choose: fn(
        &mut WorkspaceSurface,
        TrailingDrawdownMode,
        &mut Window,
        &mut Context<WorkspaceSurface>,
    ),
    theme: &AerisTheme,
) -> AnyElement {
    let choices = [
        TrailingDrawdownMode::Intraday,
        TrailingDrawdownMode::EndOfDay,
    ]
    .into_iter()
    .map(|mode| (mode, mode_label(mode).to_string(), mode == current))
    .collect();
    choice_menu(
        app,
        editor,
        menu,
        mode_label(current).to_string(),
        choices,
        choose,
        theme,
    )
}

fn save_rows(
    app: &Entity<WorkspaceSurface>,
    editor: &RiskRulesEditor,
    account: &TradingAccount,
    saved: Option<&RiskProfile>,
    theme: &AerisTheme,
    cx: &App,
) -> Vec<AnyElement> {
    let colors = theme.colors;
    let draft = editor.draft(cx);
    let unsaved = match saved {
        Some(profile) => {
            let session = SessionStart {
                unix_nanos: profile.session_start_unix_nanos,
                realized_pnl: profile.session_start_realized_pnl,
            };
            draft_profile(Some(account), saved, profile.version, session, &draft)
                .map_or(true, |draft| &draft != profile)
        }
        None => draft != saved_draft(None),
    };
    let mut rows = Vec::new();
    rows.extend(editor.problem.as_ref().map(|problem| {
        div()
            .debug_selector(|| "risk_rules_problem".into())
            .py_1()
            .text_xs()
            .text_color(gpui_color(colors.text_danger))
            .child(problem.message())
            .into_any_element()
    }));
    let save = app.clone();
    rows.push(
        div()
            .pt_2()
            .flex()
            .items_center()
            .justify_end()
            .gap_3()
            .children(unsaved.then(|| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(tr!(risk_rules::UNSAVED_CHANGES))
            }))
            .child(
                Button::new("risk_rules_save", theme)
                    .variant(ButtonVariant::Primary)
                    .label(tr!(risk_rules::SAVE))
                    .loading(editor.pending.is_some())
                    .disabled(editor.pending.is_some())
                    .on_click(move |_, window, cx| {
                        save.update(cx, |surface, surface_cx| {
                            surface.save_risk_rules(window, surface_cx);
                        });
                    }),
            )
            .into_any_element(),
    );
    rows
}

/// The owner's live meter rows for one account: how far it is from each saved rule.
fn meter_lines(set: &WorkingSet, account: &TradingAccount) -> Option<Vec<(&'static str, String)>> {
    let meter = set
        .risk_meters
        .iter()
        .find(|meter| meter.account_id == account.id)?;
    let money = |value: FixedPoint| format_money(&account.currency, value);
    let mut lines = vec![
        (
            tr!(risk_rules::METER_SESSION_PNL),
            money(meter.current_realized_pnl),
        ),
        (
            tr!(risk_rules::METER_DAILY_LOSS_REMAINING),
            money(meter.daily_loss_remaining),
        ),
    ];
    let optional = [
        (
            tr!(risk_rules::METER_TRAILING_REMAINING),
            meter.trailing_drawdown_remaining,
        ),
        (
            tr!(risk_rules::METER_PROFIT_TARGET_REMAINING),
            meter.profit_target_remaining,
        ),
        (
            tr!(risk_rules::METER_DRAWDOWN_PEAK),
            meter.maximum_drawdown_peak,
        ),
        (
            tr!(risk_rules::METER_DRAWDOWN_FLOOR),
            meter.maximum_drawdown_floor,
        ),
        (
            tr!(risk_rules::METER_DRAWDOWN_REMAINING),
            meter.maximum_drawdown_remaining,
        ),
    ];
    lines.extend(
        optional
            .into_iter()
            .filter_map(|(label, value)| value.map(|value| (label, money(value)))),
    );
    Some(lines)
}

fn meter_value(value: String, theme: &AerisTheme) -> AnyElement {
    numeric_text(
        div()
            .text_sm()
            .text_color(gpui_color(theme.colors.text_default))
            .child(value),
    )
    .into_any_element()
}

/// The meter rows and the account's lock. Any lock, including a maximum drawdown lock that
/// no session boundary releases, is cleared here through the owner's unlock command; the
/// owner locks the account again while a rule is still breached.
fn meter_rows(
    app: &Entity<WorkspaceSurface>,
    working_set: Option<&WorkingSet>,
    account: &TradingAccount,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let Some((set, lines)) =
        working_set.and_then(|set| meter_lines(set, account).map(|lines| (set, lines)))
    else {
        return vec![note(
            tr!(risk_rules::METER_EMPTY),
            theme.colors.text_secondary,
        )];
    };
    let mut rows = lines
        .into_iter()
        .map(|(label, value)| {
            described_setting_row(label, [], control_strip([meter_value(value, theme)]), theme)
        })
        .collect::<Vec<_>>();
    let lock = set
        .risk_locks
        .iter()
        .find(|lock| lock.account_id == account.id);
    let lock_controls = match lock {
        // Lock reasons are the owner's own text, shown as it wrote them.
        Some(lock) => {
            let unlock = app.clone();
            let account_key = account.id.as_str().to_string();
            vec![
                meter_value(lock.reason.clone(), theme),
                Button::new("risk_rules_unlock", theme)
                    .variant(ButtonVariant::Secondary)
                    .label(tr!(risk_rules::UNLOCK))
                    .on_click(move |_, _, cx| {
                        unlock.update(cx, |surface, surface_cx| {
                            aeris_desktop::trading::unlock_simulated_account(
                                surface.lifecycle.trading(),
                                surface.lifecycle.trading_feedback(),
                                Some(account_key.clone()),
                                surface_cx,
                            );
                        });
                    })
                    .into_any_element(),
            ]
        }
        None => vec![meter_value(
            tr!(risk_rules::METER_UNLOCKED).to_string(),
            theme,
        )],
    };
    rows.push(described_setting_row(
        tr!(risk_rules::METER_LOCK),
        [],
        control_strip(lock_controls),
        theme,
    ));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading::AccountEnvironment;

    fn money(units: i64) -> FixedPoint {
        FixedPoint::try_new(units, 2).expect("money")
    }

    fn account(id: &str, environment: AccountEnvironment) -> TradingAccount {
        TradingAccount {
            id: TradingAccountId::try_new(id).expect("account"),
            display_name: format!("Account {id}"),
            environment,
            venue_id: if environment == AccountEnvironment::Simulated {
                "aeris-sim".to_string()
            } else {
                aeris_trading::venue::BROKER_VENUE_ID.to_string()
            },
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: None,
        }
    }

    fn session() -> SessionStart {
        SessionStart {
            unix_nanos: 1_700_000_000_000_000_000,
            realized_pnl: money(12_550),
        }
    }

    fn draft(texts: [&str; 7]) -> RiskRulesDraft {
        RiskRulesDraft {
            enabled: true,
            trailing_mode: TrailingDrawdownMode::Intraday,
            drawdown_mode: TrailingDrawdownMode::EndOfDay,
            texts: texts.map(str::to_string),
        }
    }

    #[test]
    fn amounts_are_exact_at_the_account_currency_scale() {
        assert_eq!(parse_money("500", 2), Some(money(50_000)));
        assert_eq!(parse_money(" 1250.5 ", 2), Some(money(125_050)));
        assert_eq!(parse_money("0.01", 2), Some(money(1)));
        for refused in ["", "0", "0.00", "-5", "1.005", "1,000", "abc", "1e3"] {
            assert_eq!(parse_money(refused, 2), None, "{refused:?} is refused");
        }
        assert_eq!(parse_percent("40"), Some(40));
        for refused in ["0", "101", "4.5", "-1", "", "x"] {
            assert_eq!(parse_percent(refused), None, "{refused:?} is refused");
        }
    }

    #[test]
    fn a_new_profile_maps_every_field_and_starts_its_session_at_the_save() {
        let practice = account("sim-a", AccountEnvironment::Simulated);
        let profile = draft_profile(
            Some(&practice),
            None,
            1,
            session(),
            &draft(["1000", "5", "500", "40", "750.50", "2500", "50000"]),
        )
        .expect("a valid profile");
        assert_eq!(
            profile,
            RiskProfile {
                account_id: practice.id.clone(),
                profile_id: DEFAULT_PROFILE_ID.to_string(),
                version: 1,
                session_start_unix_nanos: session().unix_nanos,
                session_start_realized_pnl: money(12_550),
                daily_loss_limit: money(100_000),
                trailing_drawdown: Some(money(50_000)),
                trailing_mode: TrailingDrawdownMode::Intraday,
                max_contracts: FixedPoint::try_new(5, 0).expect("contracts"),
                consistency_max_single_trade_percent: Some(40),
                restricted_until_unix_nanos: None,
                economic_event_rule: None,
                daily_profit_target: Some(money(75_050)),
                maximum_drawdown: Some(MaximumDrawdownRule {
                    amount: money(250_000),
                    mode: TrailingDrawdownMode::EndOfDay,
                    floor_stops_at: Some(money(5_000_000)),
                }),
                enabled: true,
            }
        );
        let minimal = draft_profile(
            Some(&practice),
            None,
            1,
            session(),
            &draft(["1000", "5", "", "", "", "", ""]),
        )
        .expect("only the required rules");
        assert_eq!(minimal.trailing_drawdown, None);
        assert_eq!(minimal.consistency_max_single_trade_percent, None);
        assert_eq!(minimal.daily_profit_target, None);
        assert_eq!(minimal.maximum_drawdown, None);
    }

    #[test]
    fn a_revision_keeps_the_saved_identity_session_and_unedited_rules_and_reopens_unchanged() {
        let practice = account("sim-a", AccountEnvironment::Simulated);
        let mut saved = draft_profile(
            Some(&practice),
            None,
            3,
            session(),
            &draft(["1000", "5", "", "", "750", "2500", ""]),
        )
        .expect("saved profile");
        saved.profile_id = "prop-firm".to_string();
        saved.restricted_until_unix_nanos = Some(1_800_000_000_000_000_000);
        let reopened = saved_draft(Some(&saved));
        assert_eq!(reopened.text(RuleField::DailyLossLimit), "1000.00");
        assert_eq!(reopened.text(RuleField::ProfitTarget), "750.00");
        assert_eq!(reopened.drawdown_mode, TrailingDrawdownMode::EndOfDay);
        let later = SessionStart {
            unix_nanos: 1_900_000_000_000_000_000,
            realized_pnl: money(0),
        };
        assert_eq!(
            draft_profile(
                Some(&practice),
                Some(&saved),
                saved.version,
                later,
                &reopened
            ),
            Ok(saved.clone()),
            "an untouched draft describes the saved profile exactly"
        );
        let mut edited = reopened;
        edited.texts[RuleField::MaximumDrawdown.index()] = String::new();
        let revision =
            draft_profile(Some(&practice), Some(&saved), 4, later, &edited).expect("revision");
        assert_eq!(revision.profile_id, "prop-firm");
        assert_eq!(revision.version, 4);
        assert_eq!(revision.session_start_unix_nanos, session().unix_nanos);
        assert_eq!(revision.session_start_realized_pnl, money(12_550));
        assert_eq!(
            revision.restricted_until_unix_nanos,
            saved.restricted_until_unix_nanos
        );
        assert_eq!(revision.maximum_drawdown, None);
    }

    #[test]
    fn the_editor_names_what_the_owner_would_refuse() {
        let practice = account("sim-a", AccountEnvironment::Simulated);
        let broker = account("broker-demo", AccountEnvironment::Demo);
        let valid = ["1000", "5", "", "", "", "", ""];
        assert_eq!(
            draft_profile(None, None, 1, session(), &draft(valid)),
            Err(RiskRulesProblem::NoAccount)
        );
        assert_eq!(
            draft_profile(Some(&broker), None, 1, session(), &draft(valid)),
            Err(RiskRulesProblem::NoAccount),
            "the section edits practice accounts only"
        );
        let cases = [
            (["", "5", "", "", "", "", ""], RuleField::DailyLossLimit),
            (
                ["1000", "0", "", "", "", "", ""],
                RuleField::MaximumContracts,
            ),
            (
                ["1000", "5", "1.234", "", "", "", ""],
                RuleField::TrailingDrawdown,
            ),
            (
                ["1000", "5", "", "150", "", "", ""],
                RuleField::ConsistencyPercent,
            ),
            (["1000", "5", "", "", "-1", "", ""], RuleField::ProfitTarget),
            (
                ["1000", "5", "", "", "", "zero", ""],
                RuleField::MaximumDrawdown,
            ),
            (["1000", "5", "", "", "", "2500", "0"], RuleField::FloorStop),
        ];
        for (texts, field) in cases {
            let problem = draft_profile(Some(&practice), None, 1, session(), &draft(texts))
                .expect_err("an invalid field");
            assert_eq!(problem, RiskRulesProblem::Field(field));
            assert!(problem.marks(field));
            assert!(
                !problem.marks(RuleField::DailyLossLimit) || field == RuleField::DailyLossLimit
            );
        }
        let floor_only = draft_profile(
            Some(&practice),
            None,
            1,
            session(),
            &draft(["1000", "5", "", "", "", "", "50000"]),
        )
        .expect_err("a floor stop needs a maximum drawdown");
        assert_eq!(floor_only, RiskRulesProblem::FloorStopWithoutDrawdown);
        assert!(floor_only.marks(RuleField::FloorStop));
        assert!(matches!(
            draft_profile(Some(&practice), None, 0, session(), &draft(valid)),
            Err(RiskRulesProblem::Refused(_))
        ));
    }

    #[test]
    fn every_problem_reads_as_its_own_text() {
        let mut problems = RuleField::ALL
            .into_iter()
            .map(RiskRulesProblem::Field)
            .collect::<Vec<_>>();
        problems.extend([
            RiskRulesProblem::NoAccount,
            RiskRulesProblem::FloorStopWithoutDrawdown,
            RiskRulesProblem::RevisionExhausted,
            RiskRulesProblem::Refused("disk full".to_string()),
        ]);
        let texts = problems
            .iter()
            .map(RiskRulesProblem::message)
            .collect::<Vec<_>>();
        assert_eq!(
            texts.last().map(String::as_str),
            Some("The risk rules could not be saved: disk full")
        );
        let distinct = texts.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), texts.len());
    }

    #[test]
    fn the_section_opens_on_the_traded_practice_account_or_a_saved_one() {
        let accounts = [
            account("sim-a", AccountEnvironment::Simulated),
            account("sim-b", AccountEnvironment::Simulated),
            account("broker-demo", AccountEnvironment::Demo),
        ];
        let id = |value: &str| TradingAccountId::try_new(value).expect("account");
        let mut saved = draft_profile(
            Some(&accounts[1]),
            None,
            1,
            session(),
            &draft(["1000", "5", "", "", "", "", ""]),
        )
        .expect("profile");
        assert_eq!(
            default_rules_account(&accounts, std::slice::from_ref(&saved), Some(&id("sim-a"))),
            Some(id("sim-a"))
        );
        assert_eq!(
            default_rules_account(
                &accounts,
                std::slice::from_ref(&saved),
                Some(&id("broker-demo"))
            ),
            Some(id("sim-b")),
            "a broker account is never offered"
        );
        saved.account_id = id("broker-demo");
        assert_eq!(
            default_rules_account(&accounts, &[saved], None),
            Some(id("sim-a"))
        );
        assert_eq!(default_rules_account(&accounts[2..], &[], None), None);
    }
}
