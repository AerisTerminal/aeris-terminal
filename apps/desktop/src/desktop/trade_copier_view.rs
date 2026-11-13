//! Trade copier: the Accounts panel section that chooses a source practice account, the
//! practice accounts its orders are copied to with an exact quantity multiplier each, and the
//! owner's recent copy results. The trading owner holds, validates and runs every copier; this
//! section edits a draft and sends only the configuration the user saved.

use super::accounts_panel::{ACCOUNTS_FIELD_WIDTH, pill};
use super::big_trades_dialog::{control_strip, described_setting_row, settings_group};
use super::order_ticket::format_quantity;
use super::*;
use aeris_localization::{tr, trade_copier, trading};
use aeris_trading::{AccountEnvironment, FixedPoint, OrderSide, TradingAccount, TradingAccountId};
use aeris_trading_runtime::{
    MAXIMUM_COPIER_TARGETS, TradeCopierConfig, TradeCopierTarget, TradeCopyDispatch, WorkingSet,
};

/// The section lists at most this many of its source's newest copy results.
const RECENT_COPY_ROWS: usize = 20;

/// Only practice accounts can copy or receive copies; the owner refuses broker accounts.
fn is_practice(account: &TradingAccount) -> bool {
    account.environment == AccountEnvironment::Simulated
}

fn practice_account<'a>(
    accounts: &'a [TradingAccount],
    account_id: &TradingAccountId,
) -> Option<&'a TradingAccount> {
    accounts
        .iter()
        .find(|account| &account.id == account_id)
        .filter(|account| is_practice(account))
}

fn account_name(accounts: &[TradingAccount], account_id: &TradingAccountId) -> String {
    accounts
        .iter()
        .find(|account| &account.id == account_id)
        .map_or_else(
            || account_id.as_str().to_string(),
            |account| account.display_name.clone(),
        )
}

fn saved_copier<'a>(
    copiers: &'a [TradeCopierConfig],
    source: &TradingAccountId,
) -> Option<&'a TradeCopierConfig> {
    copiers
        .iter()
        .find(|config| &config.source_account_id == source)
}

/// An exact positive multiplier such as `2`, `0.5` or `1.25`; its scale is the number of
/// decimals typed, so no value is rounded.
fn parse_multiplier(text: &str) -> Option<FixedPoint> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let scale = u8::try_from(fraction.len()).ok()?;
    let units = format!("{whole}{fraction}").parse::<i64>().ok()?;
    if units <= 0 {
        return None;
    }
    FixedPoint::try_new(units, scale).ok()
}

/// A multiplier as typed back into its field, keeping its scale: `1.50` reopens as `1.50`,
/// so an unchanged saved copier never reads as edited.
fn multiplier_text(value: FixedPoint) -> String {
    let scale = usize::from(value.scale());
    let digits = format!(
        "{:0>width$}",
        value.units().unsigned_abs(),
        width = scale + 1
    );
    let (whole, fraction) = digits.split_at(digits.len() - scale);
    if fraction.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{fraction}")
    }
}

/// The revision a save writes: past both the published copier and this editor's own last
/// save, which the working set may not show yet.
fn next_copier_revision(saved: Option<u32>, last_saved: Option<u32>) -> Option<u32> {
    saved.max(last_saved).unwrap_or(0).checked_add(1)
}

/// The account the section opens on: the account this pane trades on when it is a practice
/// account, else the first source with a saved copier, else the first practice account.
fn default_copier_source(
    accounts: &[TradingAccount],
    copiers: &[TradeCopierConfig],
    selected: Option<&TradingAccountId>,
) -> Option<TradingAccountId> {
    selected
        .filter(|id| practice_account(accounts, id).is_some())
        .cloned()
        .or_else(|| {
            copiers
                .iter()
                .map(|config| &config.source_account_id)
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

/// One target row as the editor shows it.
#[derive(Clone, Debug, PartialEq)]
struct TargetDraft {
    account_id: TradingAccountId,
    enabled: bool,
    multiplier: String,
}

/// Why the editor could not save its copier.
#[derive(Clone, Debug, PartialEq)]
enum CopierProblem {
    NoSource,
    NoTargets,
    Multiplier(TradingAccountId),
    TargetUnavailable(TradingAccountId),
    RevisionExhausted,
    Refused(String),
}

impl CopierProblem {
    fn marks(&self, account_id: &TradingAccountId) -> bool {
        matches!(self, Self::Multiplier(marked) | Self::TargetUnavailable(marked)
            if marked == account_id)
    }

    fn message(&self, accounts: &[TradingAccount]) -> String {
        match self {
            Self::NoSource => tr!(trade_copier::PROBLEM_NO_SOURCE).to_string(),
            Self::NoTargets => tr!(trade_copier::PROBLEM_NO_TARGETS).to_string(),
            Self::Multiplier(_) => tr!(trade_copier::PROBLEM_MULTIPLIER).to_string(),
            Self::TargetUnavailable(account_id) => tr!(
                trade_copier::PROBLEM_TARGET_UNAVAILABLE,
                account = account_name(accounts, account_id)
            ),
            Self::RevisionExhausted => tr!(trade_copier::PROBLEM_REVISION_EXHAUSTED).to_string(),
            Self::Refused(error) => tr!(trade_copier::SAVE_FAILED, error = error),
        }
    }
}

/// The copier the editor's fields describe, checked by the owner's own validation so the
/// editor refuses what the owner would; the owner stays the authority on save.
fn draft_config(
    source: Option<&TradingAccountId>,
    revision: u32,
    enabled: bool,
    targets: &[TargetDraft],
    accounts: &[TradingAccount],
) -> Result<TradeCopierConfig, CopierProblem> {
    let source = source
        .filter(|source| practice_account(accounts, source).is_some())
        .ok_or(CopierProblem::NoSource)?;
    if targets.is_empty() {
        return Err(CopierProblem::NoTargets);
    }
    let targets = targets
        .iter()
        .map(|target| {
            if practice_account(accounts, &target.account_id).is_none() {
                return Err(CopierProblem::TargetUnavailable(target.account_id.clone()));
            }
            let quantity_multiplier = parse_multiplier(&target.multiplier)
                .ok_or_else(|| CopierProblem::Multiplier(target.account_id.clone()))?;
            Ok(TradeCopierTarget {
                account_id: target.account_id.clone(),
                quantity_multiplier,
                enabled: target.enabled,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let config = TradeCopierConfig {
        source_account_id: source.clone(),
        revision,
        enabled,
        targets,
    };
    config.validate().map_err(CopierProblem::Refused)?;
    Ok(config)
}

/// What the owner's saved copier does now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopierStatus {
    NotSaved,
    Paused,
    Active(usize),
    AllTargetsPaused,
    /// An active target is no longer a practice account, so the owner refuses the source's
    /// orders until it is removed.
    Blocked,
}

fn copier_status(saved: Option<&TradeCopierConfig>, accounts: &[TradingAccount]) -> CopierStatus {
    let Some(config) = saved else {
        return CopierStatus::NotSaved;
    };
    if !config.enabled {
        return CopierStatus::Paused;
    }
    let active = config
        .targets
        .iter()
        .filter(|target| target.enabled)
        .collect::<Vec<_>>();
    if active
        .iter()
        .any(|target| practice_account(accounts, &target.account_id).is_none())
    {
        return CopierStatus::Blocked;
    }
    match active.len() {
        0 => CopierStatus::AllTargetsPaused,
        targets => CopierStatus::Active(targets),
    }
}

fn status_text(status: CopierStatus) -> String {
    match status {
        CopierStatus::NotSaved => tr!(trade_copier::STATUS_NOT_SAVED).to_string(),
        CopierStatus::Paused => tr!(trade_copier::STATUS_PAUSED).to_string(),
        CopierStatus::Active(targets) => tr!(trade_copier::STATUS_ACTIVE, targets = targets),
        CopierStatus::AllTargetsPaused => tr!(trade_copier::STATUS_ALL_TARGETS_PAUSED).to_string(),
        CopierStatus::Blocked => tr!(trade_copier::STATUS_BLOCKED).to_string(),
    }
}

/// The newest copy results of `source`, newest first.
fn recent_copies<'a>(
    dispatches: &'a [TradeCopyDispatch],
    source: &'a TradingAccountId,
) -> impl Iterator<Item = &'a TradeCopyDispatch> {
    dispatches
        .iter()
        .rev()
        .filter(move |dispatch| &dispatch.source_account_id == source)
        .take(RECENT_COPY_ROWS)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopierMenu {
    Source,
    AddTarget,
}

struct TargetRow {
    account_id: TradingAccountId,
    enabled: bool,
    multiplier: Entity<InputState>,
    _input_events: gpui::Subscription,
}

/// The open trade-copier section's draft, the command in flight and its last outcome.
pub(super) struct TradeCopierEditor {
    source: Option<TradingAccountId>,
    enabled: bool,
    targets: Vec<TargetRow>,
    menu: Option<CopierMenu>,
    problem: Option<CopierProblem>,
    /// The save in flight, which holds further saves until its reply.
    pending: Option<u64>,
    next_command: u64,
    last_saved_revision: Option<u32>,
}

impl TradeCopierEditor {
    fn drafts(&self, cx: &App) -> Vec<TargetDraft> {
        self.targets
            .iter()
            .map(|row| TargetDraft {
                account_id: row.account_id.clone(),
                enabled: row.enabled,
                multiplier: row.multiplier.read(cx).value().to_string(),
            })
            .collect()
    }

    fn holds(&self, account_id: &TradingAccountId) -> bool {
        self.targets.iter().any(|row| &row.account_id == account_id)
    }
}

#[cfg(test)]
impl TradeCopierEditor {
    pub(super) fn source(&self) -> Option<&TradingAccountId> {
        self.source.as_ref()
    }

    pub(super) fn target_multiplier(
        &self,
        account_id: &TradingAccountId,
    ) -> Option<&Entity<InputState>> {
        self.targets
            .iter()
            .find(|row| &row.account_id == account_id)
            .map(|row| &row.multiplier)
    }

    pub(super) const fn is_saving(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) const fn has_problem(&self) -> bool {
        self.problem.is_some()
    }
}

fn target_row(
    account_id: TradingAccountId,
    enabled: bool,
    multiplier: String,
    window: &mut Window,
    cx: &mut Context<WorkspaceSurface>,
) -> TargetRow {
    let input = cx.new(|input_cx| {
        InputState::new(window, input_cx).placeholder(tr!(trade_copier::MULTIPLIER_PLACEHOLDER))
    });
    input.update(cx, |input, input_cx| {
        input.set_value(multiplier, window, input_cx);
    });
    let input_events = cx.subscribe_in(
        &input,
        window,
        |surface, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => surface.save_trade_copier(window, cx),
            InputEvent::Change => surface.trade_copier_edited(cx),
            InputEvent::Blur | InputEvent::Focus => {}
        },
    );
    TargetRow {
        account_id,
        enabled,
        multiplier: input,
        _input_events: input_events,
    }
}

impl WorkspaceSurface {
    fn copier_working_set<'a>(&self, cx: &'a App) -> Option<&'a WorkingSet> {
        self.lifecycle.trading_store()?.read(cx).current()
    }

    /// Opens the section on its default source unless it is already open.
    pub(super) fn open_trade_copier_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.trading_pnl.trade_copier.is_some() {
            return;
        }
        let source = self.copier_working_set(cx).and_then(|set| {
            default_copier_source(
                &set.accounts,
                &set.trade_copiers,
                self.trading_pnl.order_entry.selected_account_id.as_ref(),
            )
        });
        self.load_trade_copier(source, window, cx);
    }

    /// Replaces the draft with `source`'s saved copier, or an empty one when none is saved.
    fn load_trade_copier(
        &mut self,
        source: Option<TradingAccountId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = source.as_ref().and_then(|source| {
            self.copier_working_set(cx)
                .and_then(|set| saved_copier(&set.trade_copiers, source))
                .cloned()
        });
        let targets = saved.as_ref().map_or_else(Vec::new, |config| {
            config
                .targets
                .iter()
                .map(|target| {
                    target_row(
                        target.account_id.clone(),
                        target.enabled,
                        multiplier_text(target.quantity_multiplier),
                        window,
                        cx,
                    )
                })
                .collect()
        });
        let next_command = self
            .trading_pnl
            .trade_copier
            .as_ref()
            .map_or(0, |editor| editor.next_command);
        self.trading_pnl.trade_copier = Some(TradeCopierEditor {
            source,
            enabled: saved.as_ref().is_none_or(|config| config.enabled),
            targets,
            menu: None,
            problem: None,
            pending: None,
            next_command,
            last_saved_revision: None,
        });
        cx.notify();
    }

    fn trade_copier_edited(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.trade_copier.as_mut() {
            editor.problem = None;
            cx.notify();
        }
    }

    fn toggle_trade_copier_menu(&mut self, menu: CopierMenu, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.trade_copier.as_mut() {
            editor.menu = (editor.menu != Some(menu)).then_some(menu);
            cx.notify();
        }
    }

    fn close_trade_copier_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.trade_copier.as_mut()
            && editor.menu.take().is_some()
        {
            cx.notify();
        }
    }

    fn choose_trade_copier_source(
        &mut self,
        source: TradingAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unchanged = self
            .trading_pnl
            .trade_copier
            .as_ref()
            .is_some_and(|editor| editor.source.as_ref() == Some(&source));
        if unchanged {
            self.close_trade_copier_menu(cx);
            return;
        }
        self.load_trade_copier(Some(source), window, cx);
    }

    /// Adds `account_id` as an active target with a multiplier of one.
    pub(super) fn add_trade_copier_target(
        &mut self,
        account_id: TradingAccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let addable = self
            .trading_pnl
            .trade_copier
            .as_ref()
            .is_some_and(|editor| {
                editor.targets.len() < MAXIMUM_COPIER_TARGETS
                    && editor.source.as_ref() != Some(&account_id)
                    && !editor.holds(&account_id)
            });
        if !addable {
            self.close_trade_copier_menu(cx);
            return;
        }
        let row = target_row(account_id, true, "1".to_string(), window, cx);
        if let Some(editor) = self.trading_pnl.trade_copier.as_mut() {
            editor.targets.push(row);
            editor.menu = None;
            editor.problem = None;
        }
        cx.notify();
    }

    fn remove_trade_copier_target(
        &mut self,
        account_id: &TradingAccountId,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.trading_pnl.trade_copier.as_mut() {
            editor.targets.retain(|row| &row.account_id != account_id);
            editor.problem = None;
            cx.notify();
        }
    }

    fn set_trade_copier_target_enabled(
        &mut self,
        account_id: &TradingAccountId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.trading_pnl.trade_copier.as_mut().and_then(|editor| {
            editor
                .targets
                .iter_mut()
                .find(|row| &row.account_id == account_id)
        }) {
            row.enabled = enabled;
            cx.notify();
        }
    }

    fn set_trade_copier_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.trading_pnl.trade_copier.as_mut() {
            editor.enabled = enabled;
            editor.problem = None;
            cx.notify();
        }
    }

    /// Checks the draft and sends it to the trading owner, or shows why it cannot be saved.
    pub(super) fn save_trade_copier(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.trading_pnl.trade_copier.as_ref() else {
            return;
        };
        if editor.pending.is_some() {
            return;
        }
        let working_set = self.copier_working_set(cx);
        let accounts = working_set.map_or(&[][..], |set| set.accounts.as_slice());
        let saved_revision = editor.source.as_ref().and_then(|source| {
            working_set
                .and_then(|set| saved_copier(&set.trade_copiers, source))
                .map(|config| config.revision)
        });
        let draft = next_copier_revision(saved_revision, editor.last_saved_revision)
            .ok_or(CopierProblem::RevisionExhausted)
            .and_then(|revision| {
                draft_config(
                    editor.source.as_ref(),
                    revision,
                    editor.enabled,
                    &editor.drafts(cx),
                    accounts,
                )
            });
        let Some(editor) = self.trading_pnl.trade_copier.as_mut() else {
            return;
        };
        editor.menu = None;
        let config = match draft {
            Ok(config) => config,
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
            let revision = config.revision;
            let result = cx
                .background_executor()
                .spawn(async move {
                    service
                        .register_trade_copier(config)
                        .map(|()| revision)
                        .map_err(CopierProblem::Refused)
                })
                .await;
            if let Err(error) = surface.update_in(cx, |surface, _, surface_cx| {
                surface.finish_trade_copier_save(token, result, surface_cx);
            }) && surface.upgrade().is_some()
            {
                diagnostic!("A trade copier reply could not reach its pane: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_trade_copier_save(
        &mut self,
        token: u64,
        result: Result<u32, CopierProblem>,
        cx: &mut Context<Self>,
    ) {
        let feedback = self.lifecycle.trading_feedback().clone();
        let editor = self
            .trading_pnl
            .trade_copier
            .as_mut()
            .filter(|editor| editor.pending == Some(token));
        match (editor, result) {
            (Some(editor), Ok(revision)) => {
                editor.pending = None;
                editor.last_saved_revision = Some(revision);
                feedback.record(Ok(tr!(trading::TRADE_COPIER_UPDATED).to_string()));
            }
            (Some(editor), Err(problem)) => {
                editor.pending = None;
                editor.problem = Some(problem);
            }
            (None, Ok(_)) => feedback.record(Ok(tr!(trading::TRADE_COPIER_UPDATED).to_string())),
            // The editor that sent the save has closed, so its outcome goes to the pane's
            // feedback line; a refusal's text names no account.
            (None, Err(problem)) => feedback.record(Err(problem.message(&[]))),
        }
        cx.notify();
    }
}

/// The section's groups: the source and its status, the targets and save, and the recent
/// copy results.
pub(super) fn trade_copier_groups(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    working_set: Option<&WorkingSet>,
    theme: &AerisTheme,
    cx: &App,
) -> Vec<AnyElement> {
    let accounts = working_set.map_or(&[][..], |set| set.accounts.as_slice());
    let copiers = working_set.map_or(&[][..], |set| set.trade_copiers.as_slice());
    let editor = app_state.trading_pnl.trade_copier.as_ref();
    let source = editor.and_then(|editor| editor.source.as_ref());
    let saved = source.and_then(|source| saved_copier(copiers, source));
    let mut rows = vec![note(tr!(trade_copier::INTRO), theme.colors.text_secondary)];
    if accounts
        .iter()
        .filter(|account| is_practice(account))
        .count()
        < 2
    {
        rows.push(note(
            tr!(trade_copier::NEEDS_TWO_ACCOUNTS),
            theme.colors.text_secondary,
        ));
    }
    let Some(editor) = editor else {
        return vec![settings_group(tr!(trade_copier::HEADING), rows, theme)];
    };
    rows.push(described_setting_row(
        tr!(trade_copier::SOURCE_LABEL),
        [SharedString::from(tr!(trade_copier::SOURCE_DESCRIPTION))],
        source_dropdown(app, editor, accounts, theme),
        theme,
    ));
    let Some(source) = source else {
        return vec![settings_group(tr!(trade_copier::HEADING), rows, theme)];
    };
    let toggle = app.clone();
    rows.push(described_setting_row(
        tr!(trade_copier::COPYING_LABEL),
        [SharedString::from(status_text(copier_status(
            saved, accounts,
        )))],
        Switch::new(
            "trade_copier_enabled",
            tr!(trade_copier::COPYING_LABEL),
            editor.enabled,
            theme,
        )
        .on_change(move |on, _, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.set_trade_copier_enabled(on, surface_cx);
            });
        })
        .into_any_element(),
        theme,
    ));
    vec![
        settings_group(tr!(trade_copier::HEADING), rows, theme),
        settings_group(
            tr!(trade_copier::TARGETS_HEADING),
            target_rows(app, editor, saved, accounts, theme, cx),
            theme,
        ),
        settings_group(
            tr!(trade_copier::RECENT_HEADING),
            recent_rows(working_set, source, theme),
            theme,
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

/// A dropdown of practice accounts; picking one closes it before `choose` runs.
fn account_menu(
    app: &Entity<WorkspaceSurface>,
    id: &'static str,
    trigger: Button,
    open: bool,
    choices: Vec<(TradingAccountId, String, bool)>,
    choose: fn(
        &mut WorkspaceSurface,
        TradingAccountId,
        &mut Window,
        &mut Context<WorkspaceSurface>,
    ),
    theme: &AerisTheme,
) -> AnyElement {
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
            |(index, (account_id, name, checked))| {
                let pick = app.clone();
                MenuRow::compact((id, index), name, theme)
                    .checked(checked)
                    .on_click(move |_, window, cx| {
                        pick.update(cx, |surface, surface_cx| {
                            surface.close_trade_copier_menu(surface_cx);
                            choose(surface, account_id.clone(), window, surface_cx);
                        });
                    })
            },
        ))
    });
    let dismiss = app.clone();
    div()
        .w(px(ACCOUNTS_FIELD_WIDTH))
        .flex_none()
        .child(
            MenuAnchor::new(SharedString::from(format!("{id}_anchor")), trigger)
                .full_width()
                .menu(panel)
                .on_dismiss(move |_, cx| {
                    dismiss.update(cx, WorkspaceSurface::close_trade_copier_menu);
                }),
        )
        .into_any_element()
}

fn source_dropdown(
    app: &Entity<WorkspaceSurface>,
    editor: &TradeCopierEditor,
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
                editor.source.as_ref() == Some(&account.id),
            )
        })
        .collect::<Vec<_>>();
    let current = editor.source.as_ref().map_or_else(
        || tr!(trade_copier::CHOOSE_SOURCE).to_string(),
        |source| account_name(accounts, source),
    );
    let open = editor.menu == Some(CopierMenu::Source);
    let toggle = app.clone();
    let trigger = Button::new("trade_copier_source", theme)
        .variant(ButtonVariant::Secondary)
        .trigger()
        .full_width()
        .open(open)
        .disabled(choices.is_empty())
        .aria_label(tr!(trade_copier::SOURCE_LABEL))
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
                surface.toggle_trade_copier_menu(CopierMenu::Source, surface_cx);
            });
        });
    account_menu(
        app,
        "trade_copier_source",
        trigger,
        open,
        choices,
        WorkspaceSurface::choose_trade_copier_source,
        theme,
    )
}

fn target_rows(
    app: &Entity<WorkspaceSurface>,
    editor: &TradeCopierEditor,
    saved: Option<&TradeCopierConfig>,
    accounts: &[TradingAccount],
    theme: &AerisTheme,
    cx: &App,
) -> Vec<AnyElement> {
    let colors = theme.colors;
    let mut rows = Vec::new();
    if editor.targets.is_empty() {
        rows.push(note(tr!(trade_copier::NO_TARGETS), colors.text_secondary));
    }
    for (index, row) in editor.targets.iter().enumerate() {
        rows.push(target_row_element(app, editor, index, row, accounts, theme));
    }
    rows.push(add_target_row(app, editor, accounts, theme));
    rows.extend(editor.problem.as_ref().map(|problem| {
        div()
            .debug_selector(|| "trade_copier_problem".into())
            .py_1()
            .text_xs()
            .text_color(gpui_color(colors.text_danger))
            .child(problem.message(accounts))
            .into_any_element()
    }));
    let unsaved = match saved {
        Some(config) => draft_config(
            editor.source.as_ref(),
            config.revision,
            editor.enabled,
            &editor.drafts(cx),
            accounts,
        )
        .map_or(true, |draft| &draft != config),
        None => !editor.targets.is_empty(),
    };
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
                    .child(tr!(trade_copier::UNSAVED_CHANGES))
            }))
            .child(
                Button::new("trade_copier_save", theme)
                    .variant(ButtonVariant::Primary)
                    .label(tr!(trade_copier::SAVE))
                    .loading(editor.pending.is_some())
                    .disabled(editor.pending.is_some())
                    .on_click(move |_, window, cx| {
                        save.update(cx, |surface, surface_cx| {
                            surface.save_trade_copier(window, surface_cx);
                        });
                    }),
            )
            .into_any_element(),
    );
    rows
}

fn target_row_element(
    app: &Entity<WorkspaceSurface>,
    editor: &TradeCopierEditor,
    index: usize,
    row: &TargetRow,
    accounts: &[TradingAccount],
    theme: &AerisTheme,
) -> AnyElement {
    let name = account_name(accounts, &row.account_id);
    let detail = if practice_account(accounts, &row.account_id).is_some() {
        tr!(trade_copier::TARGET_DETAIL)
    } else {
        tr!(trade_copier::TARGET_UNAVAILABLE)
    };
    let marked = editor
        .problem
        .as_ref()
        .is_some_and(|problem| problem.marks(&row.account_id));
    let toggle = app.clone();
    let toggle_account = row.account_id.clone();
    let remove = app.clone();
    let remove_account = row.account_id.clone();
    described_setting_row(
        name.clone(),
        [SharedString::from(detail)],
        control_strip([
            div()
                .w(design_rems(SETTINGS_CONTROL_WIDTH))
                .h(px(32.0))
                .flex()
                .child(
                    Input::new(&row.multiplier)
                        .platform(theme)
                        .invalid(marked)
                        .flex_1(),
                )
                .into_any_element(),
            Switch::new(
                ("trade_copier_target_enabled", index),
                tr!(trade_copier::TARGET_ENABLED_ARIA, account = name.as_str()),
                row.enabled,
                theme,
            )
            .on_change(move |on, _, _, cx| {
                toggle.update(cx, |surface, surface_cx| {
                    surface.set_trade_copier_target_enabled(&toggle_account, on, surface_cx);
                });
            })
            .into_any_element(),
            Button::new(("trade_copier_remove_target", index), theme)
                .variant(ButtonVariant::Ghost)
                .icon(header_icon(HugeIcon::Close))
                .aria_label(tr!(
                    trade_copier::REMOVE_TARGET_ARIA,
                    account = name.as_str()
                ))
                .danger_on_hover()
                .on_click(move |_, _, cx| {
                    remove.update(cx, |surface, surface_cx| {
                        surface.remove_trade_copier_target(&remove_account, surface_cx);
                    });
                })
                .into_any_element(),
        ]),
        theme,
    )
}

fn add_target_row(
    app: &Entity<WorkspaceSurface>,
    editor: &TradeCopierEditor,
    accounts: &[TradingAccount],
    theme: &AerisTheme,
) -> AnyElement {
    let full = editor.targets.len() >= MAXIMUM_COPIER_TARGETS;
    let choices = accounts
        .iter()
        .filter(|account| {
            is_practice(account)
                && editor.source.as_ref() != Some(&account.id)
                && !editor.holds(&account.id)
        })
        .map(|account| (account.id.clone(), account.display_name.clone(), false))
        .collect::<Vec<_>>();
    let description = if full {
        tr!(trade_copier::TARGETS_FULL, maximum = MAXIMUM_COPIER_TARGETS)
    } else {
        tr!(trade_copier::ADD_TARGET_DESCRIPTION).to_string()
    };
    let open = editor.menu == Some(CopierMenu::AddTarget);
    let toggle = app.clone();
    let trigger = Button::new("trade_copier_add_target", theme)
        .variant(ButtonVariant::Secondary)
        .trigger()
        .full_width()
        .open(open)
        .disabled(full || choices.is_empty())
        .icon(header_icon(HugeIcon::Add))
        .label(tr!(trade_copier::ADD_TARGET))
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_trade_copier_menu(CopierMenu::AddTarget, surface_cx);
            });
        });
    described_setting_row(
        tr!(trade_copier::ADD_TARGET_LABEL),
        [SharedString::from(description)],
        account_menu(
            app,
            "trade_copier_add_target",
            trigger,
            open,
            choices,
            WorkspaceSurface::add_trade_copier_target,
            theme,
        ),
        theme,
    )
}

fn recent_rows(
    working_set: Option<&WorkingSet>,
    source: &TradingAccountId,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let rows = working_set
        .map(|set| {
            recent_copies(&set.copy_dispatches, source)
                .map(|dispatch| dispatch_row(set, dispatch, theme))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return vec![note(
            tr!(trade_copier::RECENT_EMPTY),
            theme.colors.text_secondary,
        )];
    }
    rows
}

fn dispatch_label(set: &WorkingSet, dispatch: &TradeCopyDispatch) -> String {
    let account = account_name(&set.accounts, &dispatch.target_account_id);
    let symbol = set
        .instruments
        .iter()
        .find(|terms| terms.instrument_id == dispatch.instrument_id)
        .map_or_else(
            || dispatch.instrument_id.as_str().to_string(),
            |terms| terms.contract.provenance.display_symbol.clone(),
        );
    let side = match dispatch.side {
        OrderSide::Buy => tr!(trading::BUY_ACTION),
        OrderSide::Sell => tr!(trading::SELL_ACTION),
    };
    match dispatch.quantity {
        Some(quantity) => tr!(
            trade_copier::DISPATCH_LABEL,
            side = side,
            quantity = format_quantity(quantity),
            symbol = symbol,
            account = account
        ),
        None => tr!(
            trade_copier::DISPATCH_LABEL_NO_QUANTITY,
            side = side,
            symbol = symbol,
            account = account
        ),
    }
}

fn dispatch_row(set: &WorkingSet, dispatch: &TradeCopyDispatch, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    // Refusal details are the owner's own reasons, shown as it wrote them.
    let detail = dispatch
        .detail
        .clone()
        .unwrap_or_else(|| tr!(trade_copier::DISPATCH_COPIED_DETAIL).to_string());
    let status = if dispatch.accepted {
        pill(
            tr!(trade_copier::COPIED),
            colors.positive_subtle,
            colors.text_positive,
            None,
            theme,
        )
    } else {
        pill(
            tr!(trade_copier::REFUSED),
            colors.negative_subtle,
            colors.text_negative,
            None,
            theme,
        )
    };
    described_setting_row(
        dispatch_label(set, dispatch),
        [SharedString::from(detail)],
        status,
        theme,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn id(value: &str) -> TradingAccountId {
        TradingAccountId::try_new(value).expect("account")
    }

    fn accounts() -> Vec<TradingAccount> {
        vec![
            account("sim-a", AccountEnvironment::Simulated),
            account("sim-b", AccountEnvironment::Simulated),
            account("sim-c", AccountEnvironment::Simulated),
            account("broker-demo", AccountEnvironment::Demo),
        ]
    }

    fn draft(account_id: &str, enabled: bool, multiplier: &str) -> TargetDraft {
        TargetDraft {
            account_id: id(account_id),
            enabled,
            multiplier: multiplier.to_string(),
        }
    }

    fn fixed(units: i64, scale: u8) -> FixedPoint {
        FixedPoint::try_new(units, scale).expect("fixed point")
    }

    #[test]
    fn multipliers_are_exact_positive_decimals() {
        assert_eq!(parse_multiplier("1"), Some(fixed(1, 0)));
        assert_eq!(parse_multiplier(" 2 "), Some(fixed(2, 0)));
        assert_eq!(parse_multiplier("0.5"), Some(fixed(5, 1)));
        assert_eq!(parse_multiplier("1.50"), Some(fixed(150, 2)));
        assert_eq!(parse_multiplier(".25"), Some(fixed(25, 2)));
        assert_eq!(parse_multiplier("3."), Some(fixed(3, 0)));
        for refused in [
            "",
            ".",
            "0",
            "0.00",
            "-1",
            "+1",
            "1,5",
            "1.2.3",
            "two",
            "0.0000000000000000001",
            "99999999999999999999",
        ] {
            assert_eq!(parse_multiplier(refused), None, "{refused:?} is refused");
        }
        for saved in [
            fixed(1, 0),
            fixed(150, 2),
            fixed(5, 1),
            fixed(25, 3),
            fixed(12, 0),
        ] {
            assert_eq!(
                parse_multiplier(&multiplier_text(saved)),
                Some(saved),
                "a saved multiplier reopens as the same value"
            );
        }
        assert_eq!(multiplier_text(fixed(25, 3)), "0.025");
        assert_eq!(multiplier_text(fixed(150, 2)), "1.50");
    }

    #[test]
    fn the_editor_saves_the_copier_its_rows_describe() {
        let accounts = accounts();
        let config = draft_config(
            Some(&id("sim-a")),
            4,
            false,
            &[draft("sim-b", true, "2"), draft("sim-c", false, "0.5")],
            &accounts,
        )
        .expect("a valid copier");
        assert_eq!(
            config,
            TradeCopierConfig {
                source_account_id: id("sim-a"),
                revision: 4,
                enabled: false,
                targets: vec![
                    TradeCopierTarget {
                        account_id: id("sim-b"),
                        quantity_multiplier: fixed(2, 0),
                        enabled: true,
                    },
                    TradeCopierTarget {
                        account_id: id("sim-c"),
                        quantity_multiplier: fixed(5, 1),
                        enabled: false,
                    },
                ],
            }
        );
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn the_editor_names_what_the_owner_would_refuse() {
        let accounts = accounts();
        let targets = [draft("sim-b", true, "1")];
        assert_eq!(
            draft_config(None, 1, true, &targets, &accounts),
            Err(CopierProblem::NoSource)
        );
        assert_eq!(
            draft_config(Some(&id("broker-demo")), 1, true, &targets, &accounts),
            Err(CopierProblem::NoSource),
            "a broker account never copies"
        );
        assert_eq!(
            draft_config(Some(&id("sim-a")), 1, true, &[], &accounts),
            Err(CopierProblem::NoTargets)
        );
        let multiplier = draft_config(
            Some(&id("sim-a")),
            1,
            true,
            &[draft("sim-b", true, "1"), draft("sim-c", true, "0")],
            &accounts,
        )
        .expect_err("a zero multiplier");
        assert_eq!(multiplier, CopierProblem::Multiplier(id("sim-c")));
        assert!(multiplier.marks(&id("sim-c")));
        assert!(!multiplier.marks(&id("sim-b")));
        let broker = draft_config(
            Some(&id("sim-a")),
            1,
            true,
            &[draft("broker-demo", false, "1")],
            &accounts,
        )
        .expect_err("a broker target, even paused");
        assert_eq!(broker, CopierProblem::TargetUnavailable(id("broker-demo")));
        assert_eq!(
            broker.message(&accounts),
            "Account broker-demo is not a practice account. Remove it before saving."
        );
        assert!(matches!(
            draft_config(
                Some(&id("sim-a")),
                1,
                true,
                &[draft("sim-a", true, "1")],
                &accounts
            ),
            Err(CopierProblem::Refused(_))
        ));
    }

    #[test]
    fn every_problem_reads_as_its_own_text() {
        let accounts = accounts();
        let texts = [
            CopierProblem::NoSource,
            CopierProblem::NoTargets,
            CopierProblem::Multiplier(id("sim-b")),
            CopierProblem::TargetUnavailable(id("broker-demo")),
            CopierProblem::RevisionExhausted,
            CopierProblem::Refused("disk full".to_string()),
        ]
        .iter()
        .map(|problem| problem.message(&accounts))
        .collect::<Vec<_>>();
        assert_eq!(texts[5], "The copier could not be saved: disk full");
        let distinct = texts.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), texts.len());
    }

    #[test]
    fn a_save_advances_past_the_published_and_the_last_saved_revision() {
        assert_eq!(next_copier_revision(None, None), Some(1));
        assert_eq!(next_copier_revision(Some(3), None), Some(4));
        assert_eq!(next_copier_revision(Some(3), Some(5)), Some(6));
        assert_eq!(next_copier_revision(Some(7), Some(5)), Some(8));
        assert_eq!(next_copier_revision(Some(u32::MAX), None), None);
    }

    #[test]
    fn the_section_opens_on_the_traded_practice_account_or_a_saved_source() {
        let accounts = accounts();
        let copier = |source: &str| TradeCopierConfig {
            source_account_id: id(source),
            revision: 1,
            enabled: true,
            targets: vec![TradeCopierTarget {
                account_id: id("sim-a"),
                quantity_multiplier: fixed(1, 0),
                enabled: true,
            }],
        };
        let copiers = [copier("sim-c")];
        assert_eq!(
            default_copier_source(&accounts, &copiers, Some(&id("sim-b"))),
            Some(id("sim-b"))
        );
        assert_eq!(
            default_copier_source(&accounts, &copiers, Some(&id("broker-demo"))),
            Some(id("sim-c")),
            "a broker account is never offered as the source"
        );
        assert_eq!(
            default_copier_source(&accounts, &[], None),
            Some(id("sim-a"))
        );
        assert_eq!(
            default_copier_source(&accounts[3..], &[], Some(&id("broker-demo"))),
            None
        );
    }

    #[test]
    fn the_status_follows_the_saved_copier() {
        let accounts = accounts();
        let target = |account_id: &str, enabled| TradeCopierTarget {
            account_id: id(account_id),
            quantity_multiplier: fixed(1, 0),
            enabled,
        };
        let config = |enabled, targets| TradeCopierConfig {
            source_account_id: id("sim-a"),
            revision: 1,
            enabled,
            targets,
        };
        assert_eq!(copier_status(None, &accounts), CopierStatus::NotSaved);
        assert_eq!(
            copier_status(Some(&config(false, vec![target("sim-b", true)])), &accounts),
            CopierStatus::Paused
        );
        assert_eq!(
            copier_status(
                Some(&config(
                    true,
                    vec![target("sim-b", true), target("sim-c", false)]
                )),
                &accounts
            ),
            CopierStatus::Active(1)
        );
        assert_eq!(
            copier_status(Some(&config(true, vec![target("sim-b", false)])), &accounts),
            CopierStatus::AllTargetsPaused
        );
        assert_eq!(
            copier_status(
                Some(&config(true, vec![target("broker-demo", true)])),
                &accounts
            ),
            CopierStatus::Blocked
        );
        let texts = [
            CopierStatus::NotSaved,
            CopierStatus::Paused,
            CopierStatus::Active(2),
            CopierStatus::AllTargetsPaused,
            CopierStatus::Blocked,
        ]
        .map(status_text);
        assert_eq!(texts[2], "Copying is on. Active targets: 2");
        let distinct = texts.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), texts.len());
    }

    #[test]
    fn recent_copies_are_the_sources_newest_first_and_bounded() {
        let dispatch = |source: &str, order: usize| TradeCopyDispatch {
            source_account_id: id(source),
            source_client_order_id: format!("order-{order}"),
            instrument_id: aeris_instruments::InstrumentId::try_new("instrument:fixture:A")
                .expect("instrument"),
            side: OrderSide::Buy,
            quantity: Some(fixed(1, 0)),
            target_account_id: id("sim-b"),
            mirrored_client_order_id: format!("copy-{order}"),
            accepted: true,
            detail: None,
        };
        let dispatches = (0..RECENT_COPY_ROWS + 5)
            .flat_map(|order| [dispatch("sim-a", order), dispatch("sim-c", order)])
            .collect::<Vec<_>>();
        let source = id("sim-a");
        let recent = recent_copies(&dispatches, &source)
            .map(|dispatch| dispatch.source_client_order_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(recent.len(), RECENT_COPY_ROWS);
        assert_eq!(recent[0], format!("order-{}", RECENT_COPY_ROWS + 4));
        assert_eq!(recent.last(), Some(&"order-5".to_string()));
    }
}
