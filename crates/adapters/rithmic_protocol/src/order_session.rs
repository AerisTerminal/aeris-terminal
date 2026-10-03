//! Authenticated order- and PnL-plant connections.
//!
//! Both plants follow the same conformance rules: an account's update stream is
//! subscribed and acknowledged before its snapshot is requested, nothing is
//! polled, and historical execution requests run one at a time.

use crate::{
    CancelAllOrdersRequest, CancelOrderRequest, DecodedControlMessage, DecodedOrderMessage,
    DecodedPnlMessage, ExecutionReplayRequest, FillHistoryRequest, ModifyOrderRequest,
    NewOrderRequest, OrderPlantRequest, OutboundRequest, PnlPlantRequest,
    PnlPositionUpdatesRequest, ProtocolError, RithmicAccountKey, RithmicLoginMetadata,
    RithmicProtocolCodec, RithmicRequestCompletion, RithmicRequestKind, RithmicRequestOutcome,
    RithmicSessionError, SubscriptionAction, TradeRoutesRequest,
    session::{AuthenticatedConnection, map_protocol_error},
};
use std::{
    collections::VecDeque,
    fmt,
    time::{Duration, Instant},
};

/// Accounts whose update streams one plant connection may track.
pub const MAXIMUM_TRACKED_RITHMIC_ACCOUNTS: usize = 64;
/// Messages retained while a stream-start helper waits for its subscription
/// acknowledgement; they are returned by the next reads in arrival order.
pub const MAXIMUM_STREAM_START_MESSAGES: usize = 256;

/// One sanitized inbound message from an authenticated order-plant session.
/// Plant payloads are boxed so frequent control frames stay small.
#[derive(Clone, Debug, PartialEq)]
pub enum RithmicOrderPlantMessage {
    Control(DecodedControlMessage),
    Order(Box<DecodedOrderMessage>),
}

impl RithmicOrderPlantMessage {
    fn completion(&self) -> Option<&RithmicRequestCompletion> {
        match self {
            Self::Order(message) => match message.as_ref() {
                DecodedOrderMessage::RequestComplete(completion) => Some(completion),
                _ => None,
            },
            Self::Control(_) => None,
        }
    }
}

/// One sanitized inbound message from an authenticated PnL-plant session.
/// Plant payloads are boxed so frequent control frames stay small.
#[derive(Clone, Debug, PartialEq)]
pub enum RithmicPnlPlantMessage {
    Control(DecodedControlMessage),
    Pnl(Box<DecodedPnlMessage>),
}

impl RithmicPnlPlantMessage {
    fn completion(&self) -> Option<(RithmicRequestKind, &RithmicRequestOutcome)> {
        match self {
            Self::Pnl(message) => match message.as_ref() {
                DecodedPnlMessage::RequestComplete { request, outcome } => {
                    Some((*request, outcome))
                }
                _ => None,
            },
            Self::Control(_) => None,
        }
    }
}

/// Per-account subscribe-then-snapshot state for one plant connection.
///
/// Subscription replies carry no account identity, so at most one subscription
/// change is outstanding and its reply is attributed to that pending change.
#[derive(Debug, Default)]
struct AccountStreams {
    pending: Option<(String, SubscriptionAction)>,
    subscribed: Vec<String>,
    snapshot_in_flight: bool,
}

impl AccountStreams {
    fn is_subscribed(&self, account_id: &str) -> bool {
        self.subscribed.iter().any(|known| known == account_id)
    }

    fn check_change(
        &self,
        account_id: &str,
        action: SubscriptionAction,
    ) -> Result<(), RithmicSessionError> {
        if self.pending.is_some() {
            return Err(RithmicSessionError::RequestInFlight);
        }
        let subscribed = self.is_subscribed(account_id);
        match action {
            SubscriptionAction::Subscribe
                if subscribed || self.subscribed.len() >= MAXIMUM_TRACKED_RITHMIC_ACCOUNTS =>
            {
                Err(RithmicSessionError::RequestNotPermitted)
            }
            SubscriptionAction::Unsubscribe if !subscribed => {
                Err(RithmicSessionError::RequestNotPermitted)
            }
            SubscriptionAction::Subscribe | SubscriptionAction::Unsubscribe => Ok(()),
        }
    }

    fn change_sent(&mut self, account_id: &str, action: SubscriptionAction) {
        self.pending = Some((account_id.to_string(), action));
    }

    fn change_replied(&mut self, accepted: bool) {
        let Some((account_id, action)) = self.pending.take() else {
            return;
        };
        if !accepted {
            return;
        }
        match action {
            SubscriptionAction::Subscribe => self.subscribed.push(account_id),
            SubscriptionAction::Unsubscribe => {
                self.subscribed.retain(|known| *known != account_id);
            }
        }
    }

    fn check_snapshot(&self, account_id: &str) -> Result<(), RithmicSessionError> {
        if self.snapshot_in_flight {
            return Err(RithmicSessionError::RequestInFlight);
        }
        if !self.is_subscribed(account_id) {
            return Err(RithmicSessionError::RequestNotPermitted);
        }
        Ok(())
    }
}

/// Bounded queue of messages read while a helper waited for one reply.
#[derive(Debug)]
struct Deferred<M> {
    messages: VecDeque<M>,
}

impl<M> Default for Deferred<M> {
    fn default() -> Self {
        Self {
            messages: VecDeque::new(),
        }
    }
}

impl<M> Deferred<M> {
    fn push(&mut self, message: M) -> Result<(), RithmicSessionError> {
        if self.messages.len() >= MAXIMUM_STREAM_START_MESSAGES {
            return Err(RithmicSessionError::Protocol);
        }
        self.messages.push_back(message);
        Ok(())
    }

    fn pop(&mut self) -> Option<M> {
        self.messages.pop_front()
    }
}

/// Authenticated order-plant connection.
pub struct RithmicOrderConnection {
    connection: AuthenticatedConnection,
    order_updates: AccountStreams,
    account_list_in_flight: bool,
    trade_routes_in_flight: bool,
    history_in_flight: Option<RithmicRequestKind>,
    deferred: Deferred<RithmicOrderPlantMessage>,
}

impl fmt::Debug for RithmicOrderConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicOrderConnection")
            .field("heartbeat_interval", &self.connection.heartbeat_interval())
            .field("limits", &self.connection.limits())
            .finish_non_exhaustive()
    }
}

impl RithmicOrderConnection {
    pub(crate) fn new(connection: AuthenticatedConnection) -> Self {
        Self {
            connection,
            order_updates: AccountStreams::default(),
            account_list_in_flight: false,
            trade_routes_in_flight: false,
            history_in_flight: None,
            deferred: Deferred::default(),
        }
    }

    #[must_use]
    pub const fn heartbeat_interval(&self) -> Duration {
        self.connection.heartbeat_interval()
    }

    /// Returns the provider identity and UTC start time for this order login.
    #[must_use]
    pub const fn login_metadata(&self) -> &RithmicLoginMetadata {
        self.connection.login_metadata()
    }

    /// Sends a provider heartbeat request.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn send_heartbeat(&mut self) -> Result<(), RithmicSessionError> {
        self.connection.send(OutboundRequest::Heartbeat)
    }

    /// Requests the firm identity and session limits of the logged-in user.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn request_login_info(&mut self) -> Result<(), RithmicSessionError> {
        self.send(OrderPlantRequest::LoginInfo)
    }

    /// Lists the user's accounts; rows arrive before one completion.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestInFlight`] while a listing is
    /// outstanding, or a redacted protocol or transport failure.
    pub fn request_accounts(
        &mut self,
        request: crate::AccountListRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        if self.account_list_in_flight {
            return Err(RithmicSessionError::RequestInFlight);
        }
        self.send(OrderPlantRequest::AccountList(request))?;
        self.account_list_in_flight = true;
        Ok(())
    }

    /// Lists trade routes, optionally subscribing to pushed route updates.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestInFlight`] while a listing is
    /// outstanding, or a redacted protocol or transport failure.
    pub fn request_trade_routes(
        &mut self,
        request: TradeRoutesRequest,
    ) -> Result<(), RithmicSessionError> {
        if self.trade_routes_in_flight {
            return Err(RithmicSessionError::RequestInFlight);
        }
        self.send(OrderPlantRequest::TradeRoutes(request))?;
        self.trade_routes_in_flight = true;
        Ok(())
    }

    /// Subscribes to one account's order notifications. The acknowledgement is
    /// observed by [`Self::read_next`] or [`Self::read_next_until`].
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestInFlight`] while another
    /// subscription is unacknowledged, [`RithmicSessionError::RequestNotPermitted`]
    /// for a duplicate or untracked account, or a protocol or transport failure.
    pub fn subscribe_order_updates(
        &mut self,
        account: RithmicAccountKey<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.order_updates
            .check_change(account.account_id, SubscriptionAction::Subscribe)?;
        self.send(OrderPlantRequest::SubscribeOrderUpdates(account))?;
        self.order_updates
            .change_sent(account.account_id, SubscriptionAction::Subscribe);
        Ok(())
    }

    /// Requests one account's working-order snapshot. Permitted only after the
    /// account's order-update subscription was acknowledged, so no change can
    /// fall between the snapshot and the live stream.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestNotPermitted`] before the
    /// subscription is acknowledged, [`RithmicSessionError::RequestInFlight`]
    /// while another snapshot is outstanding, or a protocol or transport failure.
    pub fn request_order_snapshot(
        &mut self,
        account: RithmicAccountKey<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.order_updates.check_snapshot(account.account_id)?;
        self.send(OrderPlantRequest::ShowOrders(account))?;
        self.order_updates.snapshot_in_flight = true;
        Ok(())
    }

    /// Subscribes to one account's order updates, waits until `deadline` for the
    /// acknowledgement, then requests the working-order snapshot.
    ///
    /// Messages read while waiting are returned by subsequent reads in arrival
    /// order; at most [`MAXIMUM_STREAM_START_MESSAGES`] are retained.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestRejected`] when the provider refuses
    /// the subscription, [`RithmicSessionError::Protocol`] when the retention
    /// bound is exceeded, or a deadline, protocol, or transport failure.
    pub fn start_order_stream(
        &mut self,
        account: RithmicAccountKey<'_>,
        deadline: Instant,
    ) -> Result<(), RithmicSessionError> {
        self.subscribe_order_updates(account)?;
        loop {
            let message = self.read_from_wire(deadline)?;
            if let Some(completion) = message.completion()
                && completion.request == RithmicRequestKind::OrderUpdates
            {
                if !completion.outcome.is_accepted() {
                    return Err(RithmicSessionError::RequestRejected);
                }
                break;
            }
            match &message {
                RithmicOrderPlantMessage::Control(DecodedControlMessage::Reject) => {
                    self.order_updates.change_replied(false);
                    self.deferred.push(message)?;
                    return Err(RithmicSessionError::RequestRejected);
                }
                RithmicOrderPlantMessage::Control(DecodedControlMessage::ForcedLogout) => {
                    self.deferred.push(message)?;
                    return Err(RithmicSessionError::UnexpectedMessage);
                }
                _ => self.deferred.push(message)?,
            }
        }
        self.request_order_snapshot(account)
    }

    /// Submits one order.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation, protocol, or transport failure.
    pub fn submit_order(
        &mut self,
        request: NewOrderRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OrderPlantRequest::NewOrder(request))
    }

    /// Modifies one working order.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation, protocol, or transport failure.
    pub fn modify_order(
        &mut self,
        request: ModifyOrderRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OrderPlantRequest::ModifyOrder(request))
    }

    /// Cancels one working order.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation, protocol, or transport failure.
    pub fn cancel_order(
        &mut self,
        request: CancelOrderRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OrderPlantRequest::CancelOrder(request))
    }

    /// Cancels every working order of one account.
    ///
    /// # Errors
    ///
    /// Returns a redacted validation, protocol, or transport failure.
    pub fn cancel_all_orders(
        &mut self,
        request: CancelAllOrdersRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.send(OrderPlantRequest::CancelAllOrders(request))
    }

    /// Starts one execution replay. Historical requests run one at a time.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestInFlight`] while any historical
    /// request is outstanding, or a validation, protocol, or transport failure.
    pub fn replay_executions(
        &mut self,
        request: ExecutionReplayRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.start_history(
            RithmicRequestKind::ReplayExecutions,
            OrderPlantRequest::ReplayExecutions(request),
        )
    }

    /// Starts one fill-history request of at most 30 days. Historical requests
    /// run one at a time.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestInFlight`] while any historical
    /// request is outstanding, or a validation, protocol, or transport failure.
    pub fn request_fill_history(
        &mut self,
        request: FillHistoryRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.start_history(
            RithmicRequestKind::FillHistory,
            OrderPlantRequest::ShowFillHistory(request),
        )
    }

    /// Reads and decodes one bounded provider message.
    ///
    /// # Errors
    ///
    /// Returns a redacted deadline, transport, or protocol failure.
    pub fn read_next(&mut self) -> Result<RithmicOrderPlantMessage, RithmicSessionError> {
        self.read_next_until(Instant::now() + self.connection.limits().response_timeout)
    }

    /// Reads and decodes one bounded provider message before `deadline`.
    ///
    /// # Errors
    ///
    /// Returns a redacted deadline, transport, or protocol failure.
    pub fn read_next_until(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicOrderPlantMessage, RithmicSessionError> {
        if let Some(message) = self.deferred.pop() {
            return Ok(message);
        }
        self.read_from_wire(deadline)
    }

    /// Logs out and closes within the configured close deadline.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn close(self) -> Result<(), RithmicSessionError> {
        self.connection.close()
    }

    fn send(&mut self, request: OrderPlantRequest<'_>) -> Result<(), RithmicSessionError> {
        self.connection.send(OutboundRequest::Order(request))
    }

    fn start_history(
        &mut self,
        kind: RithmicRequestKind,
        request: OrderPlantRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        if self.history_in_flight.is_some() {
            return Err(RithmicSessionError::RequestInFlight);
        }
        self.send(request)?;
        self.history_in_flight = Some(kind);
        Ok(())
    }

    fn read_from_wire(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicOrderPlantMessage, RithmicSessionError> {
        loop {
            let frame = self.connection.read_frame_until(deadline)?;
            if let Some(message) = decode_order_frame(&frame)? {
                self.observe(&message);
                return Ok(message);
            }
        }
    }

    fn observe(&mut self, message: &RithmicOrderPlantMessage) {
        let Some(completion) = message.completion() else {
            return;
        };
        match completion.request {
            RithmicRequestKind::OrderUpdates => self
                .order_updates
                .change_replied(completion.outcome.is_accepted()),
            RithmicRequestKind::ShowOrders => self.order_updates.snapshot_in_flight = false,
            RithmicRequestKind::AccountList => self.account_list_in_flight = false,
            RithmicRequestKind::TradeRoutes => self.trade_routes_in_flight = false,
            kind @ (RithmicRequestKind::ReplayExecutions | RithmicRequestKind::FillHistory) => {
                if self.history_in_flight == Some(kind) {
                    self.history_in_flight = None;
                }
            }
            RithmicRequestKind::LoginInfo
            | RithmicRequestKind::NewOrder
            | RithmicRequestKind::ModifyOrder
            | RithmicRequestKind::CancelOrder
            | RithmicRequestKind::CancelAllOrders
            | RithmicRequestKind::PnlUpdates
            | RithmicRequestKind::PnlSnapshot => {}
        }
    }
}

/// Authenticated PnL-plant connection.
pub struct RithmicPnlConnection {
    connection: AuthenticatedConnection,
    position_updates: AccountStreams,
    deferred: Deferred<RithmicPnlPlantMessage>,
}

impl fmt::Debug for RithmicPnlConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicPnlConnection")
            .field("heartbeat_interval", &self.connection.heartbeat_interval())
            .field("limits", &self.connection.limits())
            .finish_non_exhaustive()
    }
}

impl RithmicPnlConnection {
    pub(crate) fn new(connection: AuthenticatedConnection) -> Self {
        Self {
            connection,
            position_updates: AccountStreams::default(),
            deferred: Deferred::default(),
        }
    }

    #[must_use]
    pub const fn heartbeat_interval(&self) -> Duration {
        self.connection.heartbeat_interval()
    }

    /// Returns the provider identity and UTC start time for this P&L login.
    #[must_use]
    pub const fn login_metadata(&self) -> &RithmicLoginMetadata {
        self.connection.login_metadata()
    }

    /// Sends a provider heartbeat request.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn send_heartbeat(&mut self) -> Result<(), RithmicSessionError> {
        self.connection.send(OutboundRequest::Heartbeat)
    }

    /// Installs or removes one account's pushed P&L updates. The acknowledgement
    /// is observed by [`Self::read_next`] or [`Self::read_next_until`].
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestInFlight`] while another change is
    /// unacknowledged, [`RithmicSessionError::RequestNotPermitted`] for a
    /// duplicate or unknown account, or a protocol or transport failure.
    pub fn update_position_updates(
        &mut self,
        request: PnlPositionUpdatesRequest<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.position_updates
            .check_change(request.account.account_id, request.action)?;
        self.send(PnlPlantRequest::PositionUpdates(request))?;
        self.position_updates
            .change_sent(request.account.account_id, request.action);
        Ok(())
    }

    /// Requests one account's position and account P&L snapshot. Permitted only
    /// after the account's update subscription was acknowledged.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestNotPermitted`] before the
    /// subscription is acknowledged, [`RithmicSessionError::RequestInFlight`]
    /// while another snapshot is outstanding, or a protocol or transport failure.
    pub fn request_position_snapshot(
        &mut self,
        account: RithmicAccountKey<'_>,
    ) -> Result<(), RithmicSessionError> {
        self.position_updates.check_snapshot(account.account_id)?;
        self.send(PnlPlantRequest::PositionSnapshot(account))?;
        self.position_updates.snapshot_in_flight = true;
        Ok(())
    }

    /// Subscribes to one account's P&L updates, waits until `deadline` for the
    /// acknowledgement, then requests the P&L snapshot.
    ///
    /// Messages read while waiting are returned by subsequent reads in arrival
    /// order; at most [`MAXIMUM_STREAM_START_MESSAGES`] are retained.
    ///
    /// # Errors
    ///
    /// Returns [`RithmicSessionError::RequestRejected`] when the provider refuses
    /// the subscription, [`RithmicSessionError::Protocol`] when the retention
    /// bound is exceeded, or a deadline, protocol, or transport failure.
    pub fn start_pnl_stream(
        &mut self,
        account: RithmicAccountKey<'_>,
        deadline: Instant,
    ) -> Result<(), RithmicSessionError> {
        self.update_position_updates(PnlPositionUpdatesRequest {
            account,
            action: SubscriptionAction::Subscribe,
        })?;
        loop {
            let message = self.read_from_wire(deadline)?;
            if let Some((RithmicRequestKind::PnlUpdates, outcome)) = message.completion() {
                if !outcome.is_accepted() {
                    return Err(RithmicSessionError::RequestRejected);
                }
                break;
            }
            match &message {
                RithmicPnlPlantMessage::Control(DecodedControlMessage::Reject) => {
                    self.position_updates.change_replied(false);
                    self.deferred.push(message)?;
                    return Err(RithmicSessionError::RequestRejected);
                }
                RithmicPnlPlantMessage::Control(DecodedControlMessage::ForcedLogout) => {
                    self.deferred.push(message)?;
                    return Err(RithmicSessionError::UnexpectedMessage);
                }
                _ => self.deferred.push(message)?,
            }
        }
        self.request_position_snapshot(account)
    }

    /// Reads and decodes one bounded provider message.
    ///
    /// # Errors
    ///
    /// Returns a redacted deadline, transport, or protocol failure.
    pub fn read_next(&mut self) -> Result<RithmicPnlPlantMessage, RithmicSessionError> {
        self.read_next_until(Instant::now() + self.connection.limits().response_timeout)
    }

    /// Reads and decodes one bounded provider message before `deadline`.
    ///
    /// # Errors
    ///
    /// Returns a redacted deadline, transport, or protocol failure.
    pub fn read_next_until(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicPnlPlantMessage, RithmicSessionError> {
        if let Some(message) = self.deferred.pop() {
            return Ok(message);
        }
        self.read_from_wire(deadline)
    }

    /// Logs out and closes within the configured close deadline.
    ///
    /// # Errors
    ///
    /// Returns a redacted protocol or transport failure.
    pub fn close(self) -> Result<(), RithmicSessionError> {
        self.connection.close()
    }

    fn send(&mut self, request: PnlPlantRequest<'_>) -> Result<(), RithmicSessionError> {
        self.connection.send(OutboundRequest::Pnl(request))
    }

    fn read_from_wire(
        &mut self,
        deadline: Instant,
    ) -> Result<RithmicPnlPlantMessage, RithmicSessionError> {
        let frame = self.connection.read_frame_until(deadline)?;
        let message = decode_pnl_frame(&frame)?;
        match message.completion() {
            Some((RithmicRequestKind::PnlUpdates, outcome)) => {
                self.position_updates.change_replied(outcome.is_accepted());
            }
            Some((RithmicRequestKind::PnlSnapshot, _)) => {
                self.position_updates.snapshot_in_flight = false;
            }
            _ => {}
        }
        Ok(message)
    }
}

fn decode_order_frame(
    frame: &[u8],
) -> Result<Option<RithmicOrderPlantMessage>, RithmicSessionError> {
    let codec = RithmicProtocolCodec;
    match codec.decode_control(frame) {
        Ok(message) => return Ok(Some(RithmicOrderPlantMessage::Control(message))),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    codec
        .decode_order(frame)
        .map(|message| message.map(|message| RithmicOrderPlantMessage::Order(Box::new(message))))
        .map_err(map_protocol_error)
}

fn decode_pnl_frame(frame: &[u8]) -> Result<RithmicPnlPlantMessage, RithmicSessionError> {
    let codec = RithmicProtocolCodec;
    match codec.decode_control(frame) {
        Ok(message) => return Ok(RithmicPnlPlantMessage::Control(message)),
        Err(ProtocolError::UnsupportedTemplate(_)) => {}
        Err(error) => return Err(map_protocol_error(error)),
    }
    codec
        .decode_pnl(frame)
        .map(|message| RithmicPnlPlantMessage::Pnl(Box::new(message)))
        .map_err(map_protocol_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_require_an_acknowledged_subscription() {
        let mut streams = AccountStreams::default();
        assert_eq!(
            streams.check_snapshot("fixture-account"),
            Err(RithmicSessionError::RequestNotPermitted)
        );
        streams
            .check_change("fixture-account", SubscriptionAction::Subscribe)
            .expect("first subscription is permitted");
        streams.change_sent("fixture-account", SubscriptionAction::Subscribe);
        assert_eq!(
            streams.check_snapshot("fixture-account"),
            Err(RithmicSessionError::RequestNotPermitted),
            "a sent but unacknowledged subscription does not permit a snapshot"
        );
        assert_eq!(
            streams.check_change("fixture-other", SubscriptionAction::Subscribe),
            Err(RithmicSessionError::RequestInFlight)
        );
        streams.change_replied(true);
        streams
            .check_snapshot("fixture-account")
            .expect("acknowledged subscription permits the snapshot");
        assert_eq!(
            streams.check_change("fixture-account", SubscriptionAction::Subscribe),
            Err(RithmicSessionError::RequestNotPermitted)
        );
        assert_eq!(
            streams.check_snapshot("fixture-other"),
            Err(RithmicSessionError::RequestNotPermitted)
        );
    }

    #[test]
    fn rejected_subscriptions_and_unsubscribes_update_tracking() {
        let mut streams = AccountStreams::default();
        streams.change_sent("fixture-account", SubscriptionAction::Subscribe);
        streams.change_replied(false);
        assert!(!streams.is_subscribed("fixture-account"));
        assert_eq!(
            streams.check_change("fixture-account", SubscriptionAction::Unsubscribe),
            Err(RithmicSessionError::RequestNotPermitted)
        );
        streams.change_sent("fixture-account", SubscriptionAction::Subscribe);
        streams.change_replied(true);
        streams
            .check_change("fixture-account", SubscriptionAction::Unsubscribe)
            .expect("subscribed account may unsubscribe");
        streams.change_sent("fixture-account", SubscriptionAction::Unsubscribe);
        streams.change_replied(true);
        assert!(!streams.is_subscribed("fixture-account"));
    }

    #[test]
    fn tracked_accounts_and_deferred_messages_are_bounded() {
        let mut streams = AccountStreams::default();
        for index in 0..MAXIMUM_TRACKED_RITHMIC_ACCOUNTS {
            let account = format!("fixture-account-{index}");
            streams
                .check_change(&account, SubscriptionAction::Subscribe)
                .expect("within the account bound");
            streams.change_sent(&account, SubscriptionAction::Subscribe);
            streams.change_replied(true);
        }
        assert_eq!(
            streams.check_change("fixture-account-overflow", SubscriptionAction::Subscribe),
            Err(RithmicSessionError::RequestNotPermitted)
        );

        let mut deferred = Deferred::default();
        for index in 0..MAXIMUM_STREAM_START_MESSAGES {
            deferred.push(index).expect("within the retention bound");
        }
        assert_eq!(
            deferred.push(MAXIMUM_STREAM_START_MESSAGES),
            Err(RithmicSessionError::Protocol)
        );
        assert_eq!(
            deferred.pop(),
            Some(0),
            "deferred messages keep arrival order"
        );
    }
}
