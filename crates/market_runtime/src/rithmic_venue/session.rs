//! One relay session: login on the order and P&L plants, account and route setup,
//! attachment as a venue session generation, then the request and report loop.

use super::{application, reconcile::Reconcile, requests::Pending, saved_login, translate};
use crate::market_service::{BrokerVenueLink, clock::Clock};
use aeris_observability::diagnostic;
use aeris_rithmic_protocol_adapter::{
    AccountListRequest, DecodedControlMessage, DecodedOrderMessage, DecodedPnlMessage,
    MAXIMUM_BRACKET_LEVELS, MAXIMUM_TRACKED_RITHMIC_ACCOUNTS, PnlPositionUpdatesRequest,
    RetryDisposition, RithmicAccount, RithmicLoginInfo, RithmicOrderConnection,
    RithmicOrderPlantMessage, RithmicPnlConnection, RithmicPnlPlantMessage, RithmicRequestKind,
    RithmicSession, RithmicSessionError, RithmicSessionLimits, RithmicTradeRoute, RithmicUserType,
    SubscriptionAction, TradeRoutesRequest,
};
use aeris_trading::{
    AccountEnvironment,
    venue::{ObservedAccount, VenueCapabilities, VenueEvent, VenueRequest, VenueUpdate},
    venue_outbox::VenueRequestReceiver,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
    time::{Duration, Instant},
};

/// Longest wait for one setup reply.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest single read while waiting for a setup reply, so shutdown stays prompt.
const STEP_READ: Duration = Duration::from_millis(250);
/// The first read of each plant per turn waits this long for a message.
const READ_SLICE: Duration = Duration::from_millis(25);
/// Later reads in the same turn only drain messages already arriving.
const DRAIN_SLICE: Duration = Duration::from_millis(2);
const MESSAGES_PER_TURN: usize = 64;
const REQUESTS_PER_TURN: usize = 16;
pub(super) const MAXIMUM_ROUTES: usize = 256;
const MINIMUM_HEARTBEAT: Duration = Duration::from_secs(1);
/// A plant that sends nothing, not even heartbeat replies, for this many heartbeat
/// intervals is treated as lost.
const SILENT_INTERVALS: u32 = 3;
/// Rithmic holds up to four target and four stop levels per bracket and echoes the
/// client order id back as the order's user tag. It documents no user tag length, so
/// the bound stays well inside the adapter's 256-byte field limit.
const CAPABILITIES: VenueCapabilities = VenueCapabilities {
    bracket_levels: MAXIMUM_BRACKET_LEVELS,
    client_order_id_limit: 64,
};
const LIVE_REFUSED: &str = "the saved Rithmic login trades live accounts, and Aeris routes \
                            orders only to Rithmic Test and paper-trading accounts";

/// Why a session ended, which decides what the supervisor does next.
pub(super) enum SessionEnd {
    Shutdown,
    /// The saved login cannot be routed; wait for the user to change it.
    Idle(String),
    LoginChanged,
    /// The connection failed; reconnect after the backoff.
    Lost(String),
}

pub(super) fn lost(step: &'static str) -> impl FnOnce(RithmicSessionError) -> SessionEnd {
    move |error| SessionEnd::Lost(format!("{step} failed: {error}"))
}

pub(super) struct Context<'a> {
    pub(super) clock: &'a dyn Clock,
    shutdown: &'a AtomicBool,
    login_changed: &'a dyn Fn() -> bool,
}

impl Context<'_> {
    fn check(&self) -> Result<(), SessionEnd> {
        if self.shutdown.load(Ordering::Acquire) {
            Err(SessionEnd::Shutdown)
        } else if (self.login_changed)() {
            Err(SessionEnd::LoginChanged)
        } else {
            Ok(())
        }
    }

    pub(super) fn wall_nanos(&self) -> Result<i64, SessionEnd> {
        self.clock
            .wall_nanos()
            .ok_or_else(|| SessionEnd::Lost("the system clock is invalid".into()))
    }
}

/// The two plant connections of one session and their liveness.
pub(super) struct Plants {
    pub(super) orders: RithmicOrderConnection,
    pub(super) pnl: RithmicPnlConnection,
    interval: Duration,
    next_heartbeat: Instant,
    order_heard: Instant,
    pnl_heard: Instant,
}

enum Read<T> {
    Message(T),
    Control,
    Idle,
}

impl Plants {
    fn new(orders: RithmicOrderConnection, pnl: RithmicPnlConnection, now: Instant) -> Self {
        let interval = orders
            .heartbeat_interval()
            .min(pnl.heartbeat_interval())
            .max(MINIMUM_HEARTBEAT);
        Self {
            orders,
            pnl,
            interval,
            next_heartbeat: now + interval,
            order_heard: now,
            pnl_heard: now,
        }
    }

    fn keep_alive(&mut self, now: Instant) -> Result<(), SessionEnd> {
        let silence = self.interval.saturating_mul(SILENT_INTERVALS);
        if now.saturating_duration_since(self.order_heard) > silence {
            return Err(SessionEnd::Lost("the order plant went silent".into()));
        }
        if now.saturating_duration_since(self.pnl_heard) > silence {
            return Err(SessionEnd::Lost("the P&L plant went silent".into()));
        }
        if now >= self.next_heartbeat {
            self.orders
                .send_heartbeat()
                .map_err(lost("the order plant heartbeat"))?;
            self.pnl
                .send_heartbeat()
                .map_err(lost("the P&L plant heartbeat"))?;
            self.next_heartbeat = now + self.interval;
        }
        Ok(())
    }

    fn read_order(
        &mut self,
        wait: Duration,
        clock: &dyn Clock,
    ) -> Result<Read<DecodedOrderMessage>, SessionEnd> {
        let read = self.orders.read_next_until(clock.monotonic() + wait);
        if read.is_ok() {
            self.order_heard = clock.monotonic();
        }
        match read {
            Ok(RithmicOrderPlantMessage::Order(message)) => Ok(Read::Message(*message)),
            Ok(RithmicOrderPlantMessage::Control(control)) => control_message(&control, "order"),
            Err(RithmicSessionError::Deadline) => Ok(Read::Idle),
            Err(error) => Err(lost("reading the order plant")(error)),
        }
    }

    fn read_pnl(
        &mut self,
        wait: Duration,
        clock: &dyn Clock,
    ) -> Result<Read<DecodedPnlMessage>, SessionEnd> {
        let read = self.pnl.read_next_until(clock.monotonic() + wait);
        if read.is_ok() {
            self.pnl_heard = clock.monotonic();
        }
        match read {
            Ok(RithmicPnlPlantMessage::Pnl(message)) => Ok(Read::Message(*message)),
            Ok(RithmicPnlPlantMessage::Control(control)) => control_message(&control, "P&L"),
            Err(RithmicSessionError::Deadline) => Ok(Read::Idle),
            Err(error) => Err(lost("reading the P&L plant")(error)),
        }
    }

    /// Reads order-plant messages until `accept` finishes the step. Messages it passes
    /// over arrive before the session is attached and are settled by the reconcile that
    /// follows the attachment.
    fn await_order<T>(
        &mut self,
        context: &Context<'_>,
        step: &str,
        mut accept: impl FnMut(DecodedOrderMessage) -> Option<Result<T, String>>,
    ) -> Result<T, SessionEnd> {
        let deadline = context.clock.monotonic() + STEP_TIMEOUT;
        loop {
            context.check()?;
            let now = context.clock.monotonic();
            if now >= deadline {
                return Err(SessionEnd::Lost(format!("{step} timed out")));
            }
            self.keep_alive(now)?;
            if let Read::Message(message) =
                self.read_order(STEP_READ.min(deadline - now), context.clock)?
                && let Some(result) = accept(message)
            {
                return result.map_err(SessionEnd::Lost);
            }
        }
    }

    fn await_pnl<T>(
        &mut self,
        context: &Context<'_>,
        step: &str,
        mut accept: impl FnMut(DecodedPnlMessage) -> Option<T>,
    ) -> Result<T, SessionEnd> {
        let deadline = context.clock.monotonic() + STEP_TIMEOUT;
        loop {
            context.check()?;
            let now = context.clock.monotonic();
            if now >= deadline {
                return Err(SessionEnd::Lost(format!("{step} timed out")));
            }
            self.keep_alive(now)?;
            if let Read::Message(message) =
                self.read_pnl(STEP_READ.min(deadline - now), context.clock)?
                && let Some(value) = accept(message)
            {
                return Ok(value);
            }
        }
    }

    fn close(self) {
        if let Err(error) = self.orders.close() {
            diagnostic!("Aeris broker venue order plant did not log out cleanly: {error}");
        }
        if let Err(error) = self.pnl.close() {
            diagnostic!("Aeris broker venue P&L plant did not log out cleanly: {error}");
        }
    }
}

fn control_message<T>(control: &DecodedControlMessage, plant: &str) -> Result<Read<T>, SessionEnd> {
    match control {
        DecodedControlMessage::ForcedLogout => Err(SessionEnd::Lost(format!(
            "Rithmic logged the {plant} plant session out"
        ))),
        DecodedControlMessage::Reject => {
            diagnostic!("Aeris broker venue: the Rithmic {plant} plant rejected a request");
            Ok(Read::Control)
        }
        _ => Ok(Read::Control),
    }
}

/// Runs one session to its end and logs both plants out.
pub(super) fn run(
    link: &mut BrokerVenueLink,
    shutdown: &Arc<AtomicBool>,
    clock: &dyn Clock,
    login_changed: &dyn Fn() -> bool,
) -> SessionEnd {
    let context = Context {
        clock,
        shutdown,
        login_changed,
    };
    let (orders, pnl, environment) = match open(shutdown) {
        Ok(opened) => opened,
        Err(end) => return end,
    };
    let mut plants = Plants::new(orders, pnl, clock.monotonic());
    let end = match prepare(&mut plants, &context) {
        Ok(setup) => match (link.attach)(CAPABILITIES) {
            Ok((generation, requests)) => Session {
                plants: &mut plants,
                context: &context,
                events: &*link.events,
                generation,
                requests,
                environment,
                user_type: setup.user_type,
                accounts: setup.accounts,
                routes: setup.routes,
                pending: Pending::default(),
                waiting: VecDeque::new(),
                reconcile: None,
            }
            .serve(),
            Err(error) => SessionEnd::Lost(format!("the trading owner did not attach: {error}")),
        },
        Err(end) => end,
    };
    plants.close();
    end
}

/// Logs in to both plants with the saved login, refusing a live one.
fn open(
    shutdown: &Arc<AtomicBool>,
) -> Result<
    (
        RithmicOrderConnection,
        RithmicPnlConnection,
        AccountEnvironment,
    ),
    SessionEnd,
> {
    let saved = saved_login()
        .map_err(SessionEnd::Idle)?
        .ok_or_else(|| SessionEnd::Idle("no Rithmic login is saved".into()))?;
    let credentials = saved
        .credentials()
        .map_err(|_| SessionEnd::Idle("the saved Rithmic login is unreadable".into()))?;
    // A live login never opens an order plant; the gateway's own report is checked again
    // after the login.
    if translate::environment(credentials.gateway, credentials.system_name)
        == AccountEnvironment::Live
    {
        return Err(SessionEnd::Idle(LIVE_REFUSED.into()));
    }
    let login_failed = |plant: &str, error: RithmicSessionError| {
        if shutdown.load(Ordering::Acquire) || error == RithmicSessionError::Cancelled {
            return SessionEnd::Shutdown;
        }
        let reason = format!("the {plant} plant login failed: {error}");
        match error.retry_disposition() {
            RetryDisposition::Transient => SessionEnd::Lost(reason),
            RetryDisposition::Terminal => SessionEnd::Idle(reason),
        }
    };
    let orders = RithmicSession::discover_and_login_orders(
        credentials,
        application(),
        RithmicSessionLimits::default(),
        Some(Arc::clone(shutdown)),
    )
    .map_err(|error| login_failed("order", error))?;
    let metadata = orders.login_metadata();
    let environment = translate::environment(metadata.gateway, &metadata.system_name);
    if environment == AccountEnvironment::Live {
        if let Err(error) = orders.close() {
            diagnostic!("Aeris broker venue order plant did not log out cleanly: {error}");
        }
        return Err(SessionEnd::Idle(LIVE_REFUSED.into()));
    }
    match RithmicSession::discover_and_login_pnl(
        credentials,
        application(),
        RithmicSessionLimits::default(),
        Some(Arc::clone(shutdown)),
    ) {
        Ok(pnl) => Ok((orders, pnl, environment)),
        Err(error) => {
            if let Err(close) = orders.close() {
                diagnostic!("Aeris broker venue order plant did not log out cleanly: {close}");
            }
            Err(login_failed("P&L", error))
        }
    }
}

struct Setup {
    user_type: RithmicUserType,
    accounts: Vec<RithmicAccount>,
    routes: Vec<RithmicTradeRoute>,
}

/// Reads the user's role, accounts and trade routes, and subscribes every tradable account
/// to its order and P&L updates.
fn prepare(plants: &mut Plants, context: &Context<'_>) -> Result<Setup, SessionEnd> {
    plants
        .orders
        .request_login_info()
        .map_err(lost("the login info request"))?;
    let info = plants.await_order(context, "the login info request", |message| match message {
        DecodedOrderMessage::LoginInfo(info) => Some(Ok(info)),
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::LoginInfo =>
        {
            Some(Err(format!(
                "Rithmic refused the login info request: {:?}",
                completion.outcome
            )))
        }
        _ => None,
    })?;
    let user_type = info
        .user_type
        .ok_or_else(|| SessionEnd::Lost("Rithmic did not report the user's role".into()))?;
    let accounts = list_accounts(plants, context, &info)?;
    let routes = list_routes(plants, context)?;
    let mut tracked = Vec::with_capacity(accounts.len());
    for account in accounts {
        if subscribe(plants, context, &account)? {
            tracked.push(account);
        }
    }
    Ok(Setup {
        user_type,
        accounts: tracked,
        routes,
    })
}

fn list_accounts(
    plants: &mut Plants,
    context: &Context<'_>,
    info: &RithmicLoginInfo,
) -> Result<Vec<RithmicAccount>, SessionEnd> {
    plants
        .orders
        .request_accounts(AccountListRequest {
            fcm_id: info.fcm_id.as_deref(),
            ib_id: info.ib_id.as_deref(),
        })
        .map_err(lost("the account list request"))?;
    let mut accounts = Vec::new();
    let (mut untracked, mut unpriced) = (0_usize, 0_usize);
    plants.await_order(context, "the account list", |message| match message {
        DecodedOrderMessage::Account(account) => {
            if account
                .currency
                .as_deref()
                .and_then(translate::currency_scale)
                .is_none()
            {
                unpriced += 1;
            } else if accounts.len() < MAXIMUM_TRACKED_RITHMIC_ACCOUNTS {
                accounts.push(account);
            } else {
                untracked += 1;
            }
            None
        }
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::AccountList =>
        {
            Some(finished(&completion.outcome, "the account list"))
        }
        _ => None,
    })?;
    if unpriced > 0 {
        diagnostic!(
            "Aeris broker venue skipped {unpriced} Rithmic accounts without a supported currency"
        );
    }
    if untracked > 0 {
        diagnostic!(
            "Aeris broker venue connects at most {MAXIMUM_TRACKED_RITHMIC_ACCOUNTS} Rithmic \
             accounts; {untracked} more were not connected"
        );
    }
    Ok(accounts)
}

fn list_routes(
    plants: &mut Plants,
    context: &Context<'_>,
) -> Result<Vec<RithmicTradeRoute>, SessionEnd> {
    plants
        .orders
        .request_trade_routes(TradeRoutesRequest {
            subscribe_for_updates: true,
        })
        .map_err(lost("the trade route request"))?;
    let mut routes = Vec::new();
    plants.await_order(context, "the trade route list", |message| match message {
        DecodedOrderMessage::TradeRoute(_) if routes.len() >= MAXIMUM_ROUTES => Some(Err(
            "Rithmic listed more trade routes than Aeris keeps".into(),
        )),
        DecodedOrderMessage::TradeRoute(route) => {
            routes.push(route);
            None
        }
        DecodedOrderMessage::RequestComplete(completion)
            if completion.request == RithmicRequestKind::TradeRoutes =>
        {
            Some(finished(&completion.outcome, "the trade route list"))
        }
        _ => None,
    })?;
    Ok(routes)
}

fn finished(
    outcome: &aeris_rithmic_protocol_adapter::RithmicRequestOutcome,
    step: &str,
) -> Result<(), String> {
    match outcome {
        aeris_rithmic_protocol_adapter::RithmicRequestOutcome::Rejected { .. } => {
            Err(format!("Rithmic refused {step}: {outcome:?}"))
        }
        _ => Ok(()),
    }
}

/// Subscribes one account to its order and P&L updates before anything is requested for
/// it; an account Rithmic refuses either stream for is not connected.
fn subscribe(
    plants: &mut Plants,
    context: &Context<'_>,
    account: &RithmicAccount,
) -> Result<bool, SessionEnd> {
    plants
        .orders
        .subscribe_order_updates(account.key())
        .map_err(lost("the order updates subscription"))?;
    let accepted =
        plants.await_order(
            context,
            "the order updates subscription",
            |message| match message {
                DecodedOrderMessage::RequestComplete(completion)
                    if completion.request == RithmicRequestKind::OrderUpdates =>
                {
                    Some(Ok(completion.outcome.is_accepted()))
                }
                _ => None,
            },
        )?;
    if !accepted {
        diagnostic!("Aeris broker venue: Rithmic refused order updates for one account");
        return Ok(false);
    }
    plants
        .pnl
        .update_position_updates(PnlPositionUpdatesRequest {
            account: account.key(),
            action: SubscriptionAction::Subscribe,
        })
        .map_err(lost("the P&L updates subscription"))?;
    let accepted =
        plants.await_pnl(
            context,
            "the P&L updates subscription",
            |message| match message {
                DecodedPnlMessage::RequestComplete {
                    request: RithmicRequestKind::PnlUpdates,
                    outcome,
                } => Some(outcome.is_accepted()),
                _ => None,
            },
        )?;
    if !accepted {
        diagnostic!("Aeris broker venue: Rithmic refused P&L updates for one account");
    }
    Ok(accepted)
}

/// An attached session: the plants, the owner's request receiver for this generation and
/// the bookkeeping that correlates requests with Rithmic's replies.
pub(super) struct Session<'a> {
    pub(super) plants: &'a mut Plants,
    pub(super) context: &'a Context<'a>,
    events: &'a (dyn Fn(VenueEvent) -> Result<(), String> + Send),
    generation: u64,
    requests: VenueRequestReceiver,
    environment: AccountEnvironment,
    pub(super) user_type: RithmicUserType,
    pub(super) accounts: Vec<RithmicAccount>,
    pub(super) routes: Vec<RithmicTradeRoute>,
    pub(super) pending: Pending,
    /// Requests whose exclusive Rithmic lane was busy, retried in arrival order.
    pub(super) waiting: VecDeque<VenueRequest>,
    pub(super) reconcile: Option<Reconcile>,
}

impl Session<'_> {
    fn serve(mut self) -> SessionEnd {
        if let Err(end) = self.observe_accounts() {
            return end;
        }
        loop {
            if let Err(end) = self.turn() {
                return end;
            }
        }
    }

    fn observe_accounts(&self) -> Result<(), SessionEnd> {
        for account in &self.accounts {
            let Some((currency, currency_scale)) = account
                .currency
                .as_deref()
                .and_then(|currency| Some((currency, translate::currency_scale(currency)?)))
            else {
                continue;
            };
            let display_name = account
                .account_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(account.account_id.as_str());
            self.emit(
                account.account_id.as_str(),
                VenueUpdate::AccountObserved(ObservedAccount {
                    environment: self.environment,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    currency_scale,
                }),
            )?;
        }
        Ok(())
    }

    /// One bounded turn: keep-alive, owner requests, then whatever each plant sent.
    fn turn(&mut self) -> Result<(), SessionEnd> {
        self.context.check()?;
        let now = self.context.clock.monotonic();
        self.plants.keep_alive(now)?;
        if self
            .reconcile
            .as_ref()
            .is_some_and(|reconcile| reconcile.expired(now))
        {
            return Err(SessionEnd::Lost(
                "an account reconcile did not complete".into(),
            ));
        }
        self.retry_waiting()?;
        for _ in 0..REQUESTS_PER_TURN {
            match self.requests.try_recv() {
                Ok(request) => self.submit(request)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(SessionEnd::Lost(
                        "the trading owner retired this venue session".into(),
                    ));
                }
            }
        }
        let mut wait = READ_SLICE;
        for _ in 0..MESSAGES_PER_TURN {
            match self.plants.read_order(wait, self.context.clock)? {
                Read::Message(message) => self.on_order(message)?,
                Read::Control => {}
                Read::Idle => break,
            }
            wait = DRAIN_SLICE;
        }
        let mut wait = READ_SLICE;
        for _ in 0..MESSAGES_PER_TURN {
            match self.plants.read_pnl(wait, self.context.clock)? {
                Read::Message(message) => self.on_pnl(message)?,
                Read::Control => {}
                Read::Idle => break,
            }
            wait = DRAIN_SLICE;
        }
        Ok(())
    }

    pub(super) fn emit(&self, broker_account: &str, update: VenueUpdate) -> Result<(), SessionEnd> {
        (self.events)(VenueEvent {
            session_generation: self.generation,
            broker_account: broker_account.to_string(),
            update,
            observed_unix_nanos: self.context.wall_nanos()?,
        })
        .map_err(|error| SessionEnd::Lost(format!("the trading owner stopped accepting: {error}")))
    }

    pub(super) fn knows(&self, account_id: &str) -> bool {
        self.accounts
            .iter()
            .any(|account| account.account_id.as_str() == account_id)
    }
}
