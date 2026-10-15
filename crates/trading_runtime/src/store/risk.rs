use super::*;

pub(super) struct StoredRiskProfile {
    account: String,
    profile_id: String,
    version: u32,
    daily_loss_units: i64,
    daily_loss_scale: u8,
    trailing_units: Option<i64>,
    trailing_scale: Option<u8>,
    trailing_mode: String,
    max_contracts_units: i64,
    max_contracts_scale: u8,
    consistency_percent: Option<u8>,
    restricted_until_unix_nanos: Option<i64>,
    enabled: i64,
    session_start_unix_nanos: i64,
    session_start_realized_units: i64,
    session_start_realized_scale: u8,
    event_action: Option<String>,
    event_minimum_importance: Option<i64>,
    event_lead_seconds: Option<u32>,
}

pub(super) fn decode_risk_profile(
    stored: StoredRiskProfile,
) -> Result<(TradingAccountId, RiskProfile), String> {
    let account_id =
        TradingAccountId::try_new(stored.account).map_err(|error| error.to_string())?;
    let daily_loss_limit = FixedPoint::try_new(stored.daily_loss_units, stored.daily_loss_scale)
        .map_err(|error| error.to_string())?;
    let trailing_drawdown = stored
        .trailing_units
        .zip(stored.trailing_scale)
        .map(|(units, scale)| FixedPoint::try_new(units, scale))
        .transpose()
        .map_err(|error| error.to_string())?;
    let max_contracts = FixedPoint::try_new(stored.max_contracts_units, stored.max_contracts_scale)
        .map_err(|error| error.to_string())?;
    let session_start_realized_pnl = FixedPoint::try_new(
        stored.session_start_realized_units,
        stored.session_start_realized_scale,
    )
    .map_err(|error| error.to_string())?;
    let economic_event_rule = stored
        .event_action
        .zip(stored.event_minimum_importance)
        .zip(stored.event_lead_seconds)
        .map(
            |((action, importance), lead_seconds)| -> Result<_, String> {
                Ok(EconomicEventRiskRule {
                    action: EconomicEventRiskAction::parse(&action)?,
                    minimum_importance: EconomicEventRiskImportance::from_i64(importance)?,
                    lead_seconds,
                })
            },
        )
        .transpose()?;
    let profile = RiskProfile {
        account_id: account_id.clone(),
        profile_id: stored.profile_id,
        version: stored.version,
        daily_loss_limit,
        trailing_drawdown,
        trailing_mode: TrailingDrawdownMode::parse(&stored.trailing_mode)?,
        max_contracts,
        consistency_max_single_trade_percent: stored.consistency_percent,
        restricted_until_unix_nanos: stored.restricted_until_unix_nanos,
        economic_event_rule,
        enabled: stored.enabled != 0,
        session_start_unix_nanos: stored.session_start_unix_nanos,
        session_start_realized_pnl,
    };
    profile.validate()?;
    Ok((account_id, profile))
}

impl TradingStore {
    pub(crate) fn put_risk_profile_and_state(
        &mut self,
        profile: &RiskProfile,
        state: &RiskRuleState,
    ) -> Result<(), String> {
        profile.validate()?;
        if state.account_id != profile.account_id
            || state.profile_id != profile.profile_id
            || state.profile_version != profile.version
        {
            return Err("risk profile state identity does not match profile".to_string());
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO risk_profiles(account_id, profile_id, version, daily_loss_units,
                    daily_loss_scale, trailing_units, trailing_scale, trailing_mode,
                    max_contracts_units, max_contracts_scale, consistency_percent,
                    restricted_until_unix_nanos, enabled, session_start_unix_nanos,
                    session_start_realized_units, session_start_realized_scale, event_action,
                    event_minimum_importance, event_lead_seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT(account_id) DO UPDATE SET profile_id=excluded.profile_id,
                    version=excluded.version, daily_loss_units=excluded.daily_loss_units,
                    daily_loss_scale=excluded.daily_loss_scale, trailing_units=excluded.trailing_units,
                    trailing_scale=excluded.trailing_scale, trailing_mode=excluded.trailing_mode,
                    max_contracts_units=excluded.max_contracts_units,
                    max_contracts_scale=excluded.max_contracts_scale,
                    consistency_percent=excluded.consistency_percent,
                    restricted_until_unix_nanos=excluded.restricted_until_unix_nanos,
                    enabled=excluded.enabled,
                    session_start_unix_nanos=excluded.session_start_unix_nanos,
                    session_start_realized_units=excluded.session_start_realized_units,
                    session_start_realized_scale=excluded.session_start_realized_scale,
                    event_action=excluded.event_action,
                    event_minimum_importance=excluded.event_minimum_importance,
                    event_lead_seconds=excluded.event_lead_seconds",
                params![
                    profile.account_id.as_str(),
                    profile.profile_id,
                    profile.version,
                    profile.daily_loss_limit.units(),
                    profile.daily_loss_limit.scale(),
                    profile.trailing_drawdown.map(FixedPoint::units),
                    profile.trailing_drawdown.map(FixedPoint::scale),
                    profile.trailing_mode.as_str(),
                    profile.max_contracts.units(),
                    profile.max_contracts.scale(),
                    profile.consistency_max_single_trade_percent,
                    profile.restricted_until_unix_nanos,
                    i64::from(profile.enabled),
                    profile.session_start_unix_nanos,
                    profile.session_start_realized_pnl.units(),
                    profile.session_start_realized_pnl.scale(),
                    profile.economic_event_rule.map(|rule| rule.action.as_str()),
                    profile
                        .economic_event_rule
                        .map(|rule| rule.minimum_importance.as_i64()),
                    profile.economic_event_rule.map(|rule| rule.lead_seconds),
                ],
            )
            .map_err(database_error)?;
        upsert_risk_rule_state(&transaction, state)?;
        transaction
            .execute(
                "UPDATE trade_cycles SET risk_realized_units=0,peak_quantity_units=0
             WHERE account_id=?1",
                [profile.account_id.as_str()],
            )
            .map_err(database_error)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }

    pub(crate) fn put_session_plan(&self, plan: &SessionPlan) -> Result<(), String> {
        plan.validate()?;
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO session_plans(account_id, plan_id, revision,
                    session_start_unix_nanos, session_end_unix_nanos, plan_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(account_id) DO UPDATE SET plan_id=excluded.plan_id,
                    revision=excluded.revision,
                    session_start_unix_nanos=excluded.session_start_unix_nanos,
                    session_end_unix_nanos=excluded.session_end_unix_nanos,
                    plan_json=excluded.plan_json",
                params![
                    plan.account_id.as_str(),
                    plan.plan_id,
                    plan.revision,
                    plan.session_start_unix_nanos,
                    plan.session_end_unix_nanos,
                    encode_session_plan(plan)?,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(crate) fn put_risk_lock(&self, lock: &RiskLock) -> Result<(), String> {
        lock.validate()?;
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO risk_locks(account_id, reason, locked_at_unix_nanos, profile_id,
                    profile_version) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(account_id) DO UPDATE SET reason=excluded.reason,
                    locked_at_unix_nanos=excluded.locked_at_unix_nanos,
                    profile_id=excluded.profile_id, profile_version=excluded.profile_version",
                params![
                    lock.account_id.as_str(),
                    lock.reason,
                    lock.locked_at_unix_nanos,
                    lock.profile_id,
                    lock.profile_version,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn economic_event_action_exists(
        &self,
        account_id: &TradingAccountId,
        event_id: &str,
        profile_version: u32,
    ) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM economic_event_risk_actions
                 WHERE account_id = ?1 AND event_id = ?2 AND profile_version = ?3)",
                params![account_id.as_str(), event_id, profile_version],
                |row| row.get::<_, bool>(0),
            )
            .map_err(database_error)
    }

    pub(crate) fn put_economic_event_action(
        &mut self,
        account_id: &TradingAccountId,
        event_id: &str,
        profile_version: u32,
        action: EconomicEventRiskAction,
        applied_unix_nanos: i64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO economic_event_risk_actions(account_id, event_id, profile_version,
                    action, applied_unix_nanos) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    account_id.as_str(),
                    event_id,
                    profile_version,
                    action.as_str(),
                    applied_unix_nanos,
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM economic_event_risk_actions WHERE rowid IN (
                    SELECT rowid FROM economic_event_risk_actions
                    ORDER BY applied_unix_nanos DESC LIMIT -1 OFFSET ?1
                 )",
                [usize_to_i64(self.retention.maximum_user_records_per_kind)?],
            )
            .map_err(database_error)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }

    pub(crate) fn delete_risk_lock(&self, account_id: &TradingAccountId) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM risk_locks WHERE account_id = ?1",
                [account_id.as_str()],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn put_risk_rule_state(&self, state: &RiskRuleState) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        upsert_risk_rule_state(&transaction, state)?;
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn put_discipline_state(&self, state: &DisciplineState) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        upsert_discipline_state(&transaction, state)?;
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(super) fn load_risk_profiles(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, RiskProfile>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, profile_id, version, daily_loss_units, daily_loss_scale,
                    trailing_units, trailing_scale, trailing_mode, max_contracts_units,
                    max_contracts_scale, consistency_percent, restricted_until_unix_nanos, enabled,
                    session_start_unix_nanos, session_start_realized_units,
                    session_start_realized_scale, event_action, event_minimum_importance,
                    event_lead_seconds
                 FROM risk_profiles ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(StoredRiskProfile {
                    account: row.get(0)?,
                    profile_id: row.get(1)?,
                    version: row.get(2)?,
                    daily_loss_units: row.get(3)?,
                    daily_loss_scale: row.get(4)?,
                    trailing_units: row.get(5)?,
                    trailing_scale: row.get(6)?,
                    trailing_mode: row.get(7)?,
                    max_contracts_units: row.get(8)?,
                    max_contracts_scale: row.get(9)?,
                    consistency_percent: row.get(10)?,
                    restricted_until_unix_nanos: row.get(11)?,
                    enabled: row.get(12)?,
                    session_start_unix_nanos: row.get(13)?,
                    session_start_realized_units: row.get(14)?,
                    session_start_realized_scale: row.get(15)?,
                    event_action: row.get(16)?,
                    event_minimum_importance: row.get(17)?,
                    event_lead_seconds: row.get(18)?,
                })
            })
            .map_err(database_error)?;
        for row in rows {
            let (account_id, profile) = decode_risk_profile(row.map_err(database_error)?)?;
            result.insert(account_id, profile);
        }
        Ok(result)
    }

    pub(super) fn load_risk_locks(&self) -> Result<BTreeMap<TradingAccountId, RiskLock>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, reason, locked_at_unix_nanos, profile_id, profile_version
                 FROM risk_locks ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, reason, locked_at_unix_nanos, profile_id, profile_version) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            let lock = RiskLock {
                account_id: account_id.clone(),
                reason,
                locked_at_unix_nanos,
                profile_id,
                profile_version,
            };
            lock.validate()?;
            result.insert(account_id, lock);
        }
        Ok(result)
    }

    pub(super) fn load_risk_rule_states(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, RiskRuleState>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, profile_id, profile_version, peak_session_units,
                    peak_session_scale, total_winning_units, total_winning_scale,
                    largest_winner_units, largest_winner_scale
                 FROM risk_rule_states ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, u8>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, u8>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                account_id,
                profile_id,
                profile_version,
                peak_units,
                peak_scale,
                total_units,
                total_scale,
                largest_units,
                largest_scale,
            ) = row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account_id).map_err(|error| error.to_string())?;
            result.insert(
                account_id.clone(),
                RiskRuleState {
                    account_id,
                    profile_id,
                    profile_version,
                    peak_session_pnl: fixed(peak_units, peak_scale)?,
                    total_winning_pnl: fixed(total_units, total_scale)?,
                    largest_winning_trade_pnl: fixed(largest_units, largest_scale)?,
                },
            );
        }
        Ok(result)
    }

    pub(super) fn load_session_plans(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, SessionPlan>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, plan_id, revision, session_start_unix_nanos,
                    session_end_unix_nanos, plan_json FROM session_plans ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account_id, plan_id, revision, start, end, encoded) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account_id).map_err(|error| error.to_string())?;
            let plan =
                decode_session_plan(account_id.clone(), plan_id, revision, start, end, &encoded)?;
            plan.validate()?;
            result.insert(account_id, plan);
        }
        Ok(result)
    }

    pub(super) fn load_discipline_states(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, DisciplineState>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, rapid_loss_count, loss_window_started_unix_nanos,
                    last_loss_unix_nanos, post_loss_quantity_units, post_loss_quantity_scale,
                    last_stop_fill_unix_nanos, cooldown_until_unix_nanos
                 FROM discipline_states ORDER BY account_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u8>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<u8>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, count, window, last_loss, cap_units, cap_scale, last_stop, cooldown) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            result.insert(
                account_id.clone(),
                DisciplineState {
                    account_id,
                    rapid_loss_count: count,
                    loss_window_started_unix_nanos: window,
                    last_loss_unix_nanos: last_loss,
                    post_loss_quantity_cap: optional_fixed(cap_units, cap_scale)?,
                    last_stop_fill_unix_nanos: last_stop,
                    cooldown_until_unix_nanos: cooldown,
                },
            );
        }
        Ok(result)
    }
}

pub(super) fn upsert_risk_rule_state(
    connection: &Connection,
    state: &RiskRuleState,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO risk_rule_states(account_id, profile_id, profile_version,
                peak_session_units, peak_session_scale, total_winning_units,
                total_winning_scale, largest_winner_units, largest_winner_scale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(account_id) DO UPDATE SET profile_id=excluded.profile_id,
                profile_version=excluded.profile_version,
                peak_session_units=excluded.peak_session_units,
                peak_session_scale=excluded.peak_session_scale,
                total_winning_units=excluded.total_winning_units,
                total_winning_scale=excluded.total_winning_scale,
                largest_winner_units=excluded.largest_winner_units,
                largest_winner_scale=excluded.largest_winner_scale",
            params![
                state.account_id.as_str(),
                state.profile_id,
                state.profile_version,
                state.peak_session_pnl.units(),
                state.peak_session_pnl.scale(),
                state.total_winning_pnl.units(),
                state.total_winning_pnl.scale(),
                state.largest_winning_trade_pnl.units(),
                state.largest_winning_trade_pnl.scale(),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn upsert_discipline_state(
    connection: &Connection,
    state: &DisciplineState,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO discipline_states(account_id, rapid_loss_count,
                loss_window_started_unix_nanos, last_loss_unix_nanos,
                post_loss_quantity_units, post_loss_quantity_scale,
                last_stop_fill_unix_nanos, cooldown_until_unix_nanos)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id) DO UPDATE SET rapid_loss_count=excluded.rapid_loss_count,
                loss_window_started_unix_nanos=excluded.loss_window_started_unix_nanos,
                last_loss_unix_nanos=excluded.last_loss_unix_nanos,
                post_loss_quantity_units=excluded.post_loss_quantity_units,
                post_loss_quantity_scale=excluded.post_loss_quantity_scale,
                last_stop_fill_unix_nanos=excluded.last_stop_fill_unix_nanos,
                cooldown_until_unix_nanos=excluded.cooldown_until_unix_nanos",
            params![
                state.account_id.as_str(),
                state.rapid_loss_count,
                state.loss_window_started_unix_nanos,
                state.last_loss_unix_nanos,
                state.post_loss_quantity_cap.map(FixedPoint::units),
                state.post_loss_quantity_cap.map(FixedPoint::scale),
                state.last_stop_fill_unix_nanos,
                state.cooldown_until_unix_nanos,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct StoredSessionPlanPayload {
    bias: String,
    maximum_loss_units: i64,
    maximum_loss_scale: u8,
    session_start_realized_units: i64,
    session_start_realized_scale: u8,
    allowed_setups: Vec<String>,
    active_setup: Option<String>,
    checklist: Vec<StoredChecklistItem>,
    levels: Vec<StoredPlanLevel>,
}

#[derive(Deserialize, Serialize)]
struct StoredChecklistItem {
    item_id: String,
    label: String,
    completed: bool,
}

#[derive(Deserialize, Serialize)]
struct StoredPlanLevel {
    instrument_id: String,
    label: String,
    price_units: i64,
    price_scale: u8,
}

pub(super) fn encode_session_plan(plan: &SessionPlan) -> Result<String, String> {
    let payload = StoredSessionPlanPayload {
        bias: plan.bias.as_str().to_string(),
        maximum_loss_units: plan.maximum_loss.units(),
        maximum_loss_scale: plan.maximum_loss.scale(),
        session_start_realized_units: plan.session_start_realized_pnl.units(),
        session_start_realized_scale: plan.session_start_realized_pnl.scale(),
        allowed_setups: plan.allowed_setups.clone(),
        active_setup: plan.active_setup.clone(),
        checklist: plan
            .checklist
            .iter()
            .map(|item| StoredChecklistItem {
                item_id: item.item_id.clone(),
                label: item.label.clone(),
                completed: item.completed,
            })
            .collect(),
        levels: plan
            .levels
            .iter()
            .map(|level| StoredPlanLevel {
                instrument_id: level.instrument_id.as_str().to_string(),
                label: level.label.clone(),
                price_units: level.price.units(),
                price_scale: level.price.scale(),
            })
            .collect(),
    };
    serde_json::to_string(&payload)
        .map_err(|error| format!("session plan could not be encoded: {error}"))
}

pub(super) fn decode_session_plan(
    account_id: TradingAccountId,
    plan_id: String,
    revision: u32,
    session_start_unix_nanos: i64,
    session_end_unix_nanos: i64,
    raw: &str,
) -> Result<SessionPlan, String> {
    let payload: StoredSessionPlanPayload = serde_json::from_str(raw)
        .map_err(|error| format!("stored session plan is invalid: {error}"))?;
    Ok(SessionPlan {
        account_id,
        plan_id,
        revision,
        session_start_unix_nanos,
        session_end_unix_nanos,
        bias: SessionBias::parse(&payload.bias)?,
        maximum_loss: fixed(payload.maximum_loss_units, payload.maximum_loss_scale)?,
        session_start_realized_pnl: fixed(
            payload.session_start_realized_units,
            payload.session_start_realized_scale,
        )?,
        allowed_setups: payload.allowed_setups,
        active_setup: payload.active_setup,
        checklist: payload
            .checklist
            .into_iter()
            .map(|item| SessionChecklistItem {
                item_id: item.item_id,
                label: item.label,
                completed: item.completed,
            })
            .collect(),
        levels: payload
            .levels
            .into_iter()
            .map(|level| {
                Ok(SessionPlanLevel {
                    instrument_id: InstrumentId::try_new(level.instrument_id)
                        .map_err(|error| error.to_string())?,
                    label: level.label,
                    price: fixed(level.price_units, level.price_scale)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    })
}
