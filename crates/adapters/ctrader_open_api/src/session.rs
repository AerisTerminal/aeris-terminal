//! Synchronous request correlation and account-scoped authentication on one TLS session.
use crate::{
    ProtoMessage,
    accounts::CtraderAccount,
    codec::{self, CodecError},
    generated::{
        ProtoErrorRes, ProtoOaAccountAuthReq, ProtoOaApplicationAuthReq, ProtoOaErrorRes,
        ProtoOaGetAccountListByAccessTokenReq,
    },
    host::CtraderHost,
    market::{LightSymbol, MarketRequest, decode_symbol_list},
    transport::{Bucket, Transport, TransportError},
};
use prost::Message;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

const MAX_PENDING: usize = 256;
const MAX_EVENTS: usize = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct AppCredentials {
    pub client_id: String,
    pub client_secret: String,
}
impl Drop for AppCredentials {
    fn drop(&mut self) {
        self.client_id.zeroize();
        self.client_secret.zeroize();
    }
}
impl fmt::Debug for AppCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppCredentials([redacted])")
    }
}
pub struct AccessToken(pub String);
impl Drop for AccessToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken([redacted])")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum SessionFault {
    NeedsReconnect,
    TokenInvalidated,
    ClientAuthFailure,
    Reconnect,
    AccountDisconnect(i64),
    ConnectionLimit {
        message: &'static str,
        wait: Duration,
    },
    RateLimited {
        bucket: Bucket,
        wait: Duration,
    },
    Maintenance {
        wait: Duration,
    },
    Timeout,
    Overflow,
    Protocol,
    Cancelled,
}
impl fmt::Debug for SessionFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsReconnect => f.write_str("NeedsReconnect"),
            Self::TokenInvalidated => f.write_str("TokenInvalidated"),
            Self::ClientAuthFailure => f.write_str("ClientAuthFailure"),
            Self::Reconnect => f.write_str("Reconnect"),
            Self::AccountDisconnect(_) => f.write_str("AccountDisconnect([redacted])"),
            Self::ConnectionLimit { message, wait } => f
                .debug_struct("ConnectionLimit")
                .field("message", message)
                .field("wait", wait)
                .finish(),
            Self::RateLimited { bucket, wait } => f
                .debug_struct("RateLimited")
                .field("bucket", bucket)
                .field("wait", wait)
                .finish(),
            Self::Maintenance { wait } => {
                f.debug_struct("Maintenance").field("wait", wait).finish()
            }
            Self::Timeout => f.write_str("Timeout"),
            Self::Overflow => f.write_str("Overflow"),
            Self::Protocol => f.write_str("Protocol"),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}
impl fmt::Display for SessionFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cTrader session: {self:?}")
    }
}
impl std::error::Error for SessionFault {}
impl From<CodecError> for SessionFault {
    fn from(_: CodecError) -> Self {
        Self::Protocol
    }
}
impl From<TransportError> for SessionFault {
    fn from(value: TransportError) -> Self {
        match value {
            TransportError::Cancelled => Self::Cancelled,
            TransportError::Overflow => Self::Overflow,
            TransportError::RequestTooLarge => Self::Protocol,
            TransportError::ReadTimeout => Self::Timeout,
            _ => Self::Reconnect,
        }
    }
}
#[must_use]
pub fn map_error(
    code: &str,
    retry_after: Option<u64>,
    maintenance_end: Option<i64>,
    bucket: Bucket,
) -> SessionFault {
    match code {
        "CH_CLIENT_AUTH_FAILURE" => SessionFault::ClientAuthFailure,
        "CONNECTIONS_LIMIT_EXCEEDED" => SessionFault::ConnectionLimit {
            message: "cTrader connection limit reached",
            wait: Duration::from_secs(300),
        },
        "BLOCKED_PAYLOAD_TYPE" | "REQUEST_FREQUENCY_EXCEEDED" => SessionFault::RateLimited {
            bucket,
            wait: Duration::from_secs(retry_after.unwrap_or(1).max(1)),
        },
        "SERVER_IS_UNDER_MAINTENANCE" => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            SessionFault::Maintenance {
                wait: Duration::from_secs(
                    maintenance_end
                        .and_then(|end| u64::try_from(end).ok())
                        .unwrap_or(now + 1)
                        .saturating_sub(now)
                        .max(1),
                ),
            }
        }
        "CH_ACCESS_TOKEN_INVALID" | "OA_AUTH_TOKEN_EXPIRED" => SessionFault::NeedsReconnect,
        _ => SessionFault::Protocol,
    }
}
/// Hosted 401 and 404 cannot be repaired by retrying the same capability.
#[must_use]
pub fn hosted_fault(status: u16) -> SessionFault {
    if status == 401 || status == 404 {
        SessionFault::NeedsReconnect
    } else {
        SessionFault::Reconnect
    }
}

/// Jitter is between 0 and 1 second, with a 1–60 second total bound.
#[derive(Debug, Default)]
pub struct ReconnectBackoff {
    failures: u32,
}
impl ReconnectBackoff {
    pub fn next_delay(&mut self) -> Duration {
        let ceiling = (1_u64 << self.failures.min(6)).min(60);
        self.failures = self.failures.saturating_add(1);
        let mut random = [0_u8; 8];
        if getrandom::fill(&mut random).is_err() {
            return Duration::from_secs(ceiling);
        }
        let nanos = u64::from_le_bytes(random) % 1_000_000_000;
        Duration::from_secs(ceiling)
            .saturating_add(Duration::from_nanos(nanos))
            .min(Duration::from_secs(60))
    }
    pub fn healthy(&mut self) {
        self.failures = 0;
    }
}

struct Pending {
    expected: u32,
    deadline: Instant,
    bucket: Bucket,
    response: Option<Result<ProtoMessage, SessionFault>>,
}

pub struct CtraderSession {
    transport: Transport,
    host: CtraderHost,
    token: AccessToken,
    accounts: Vec<CtraderAccount>,
    authorized: HashSet<u64>,
    account_disconnect_retries: HashSet<u64>,
    pending: HashMap<String, Pending>,
    events: VecDeque<ProtoMessage>,
    next_id: u64,
    refreshed: bool,
    pub generation: u64,
}
impl CtraderSession {
    /// # Errors
    /// Returns a transport or authentication fault, without exposing credentials.
    pub fn open(
        host: CtraderHost,
        credentials: &AppCredentials,
        token: AccessToken,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, SessionFault> {
        let transport = Transport::connect(host, stop)?;
        Self::open_with_transport(host, credentials, token, transport)
    }
    /// Separate transport constructor keeps loopback tests on the real TLS and framing path.
    ///
    /// # Errors
    /// Returns a transport or authentication fault.
    pub fn open_with_transport(
        host: CtraderHost,
        credentials: &AppCredentials,
        token: AccessToken,
        transport: Transport,
    ) -> Result<Self, SessionFault> {
        let mut session = Self {
            transport,
            host,
            token,
            accounts: Vec::new(),
            authorized: HashSet::new(),
            account_disconnect_retries: HashSet::new(),
            pending: HashMap::new(),
            events: VecDeque::new(),
            next_id: 0,
            refreshed: false,
            generation: 1,
        };
        let auth = ProtoOaApplicationAuthReq {
            payload_type: None,
            client_id: credentials.client_id.clone(),
            client_secret: credentials.client_secret.clone(),
        };
        session.request(
            2100,
            auth.encode_to_vec(),
            2101,
            Bucket::General,
            REQUEST_TIMEOUT,
        )?;
        let list = ProtoOaGetAccountListByAccessTokenReq {
            payload_type: None,
            access_token: session.token.0.clone(),
        };
        let response = session.request(
            2149,
            list.encode_to_vec(),
            2150,
            Bucket::General,
            REQUEST_TIMEOUT,
        )?;
        session.accounts = CtraderAccount::from_list(&response)?;
        Ok(session)
    }
    /// Fetch fresh application credentials once after an application-auth failure.
    ///
    /// # Errors
    /// Returns the second authentication failure explicitly.
    pub fn open_with_credentials_refresh(
        host: CtraderHost,
        credentials: &AppCredentials,
        token: AccessToken,
        stop: &Arc<AtomicBool>,
        refetch: impl FnMut() -> Result<AppCredentials, SessionFault>,
    ) -> Result<Self, SessionFault> {
        Self::open_with_credentials_refresh_using(host, credentials, token, refetch, || {
            Ok(Transport::connect(host, Arc::clone(stop))?)
        })
    }
    pub(crate) fn open_with_credentials_refresh_using(
        host: CtraderHost,
        credentials: &AppCredentials,
        token: AccessToken,
        mut refetch: impl FnMut() -> Result<AppCredentials, SessionFault>,
        mut connect: impl FnMut() -> Result<Transport, SessionFault>,
    ) -> Result<Self, SessionFault> {
        let fresh_token = AccessToken(token.0.clone());
        match Self::open_with_transport(host, credentials, token, connect()?) {
            Err(SessionFault::ClientAuthFailure) => {
                Self::open_with_transport(host, &refetch()?, fresh_token, connect()?)
            }
            result => result,
        }
    }
    /// Re-establish the connection after EOF, timeout, or `ClientDisconnect`.
    /// The caller owns the backoff across attempts; a successful healthy
    /// session explicitly resets it with `ReconnectBackoff::healthy`.
    ///
    /// # Errors
    /// Returns cancellation, connection, or authentication faults.
    pub fn reconnect(
        &mut self,
        credentials: &AppCredentials,
        backoff: &mut ReconnectBackoff,
        stop: Arc<AtomicBool>,
    ) -> Result<(), SessionFault> {
        self.transport.close();
        let deadline = Instant::now() + backoff.next_delay();
        while Instant::now() < deadline {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return Err(SessionFault::Cancelled);
            }
            std::thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        let transport = Transport::connect(self.host, stop)?;
        self.reconnect_with_transport(credentials, transport)
    }
    /// Swap the entire connection only after authentication succeeds. A new
    /// generation prevents retired-session events from being treated as current.
    ///
    /// # Errors
    /// Returns the new connection's authentication fault.
    pub fn reconnect_with_transport(
        &mut self,
        credentials: &AppCredentials,
        transport: Transport,
    ) -> Result<(), SessionFault> {
        self.transport.close();
        let token = AccessToken(self.token.0.clone());
        let mut replacement = Self::open_with_transport(self.host, credentials, token, transport)?;
        replacement.generation = self.generation.wrapping_add(1);
        *self = replacement;
        Ok(())
    }
    #[must_use]
    pub fn accounts(&self) -> &[CtraderAccount] {
        &self.accounts
    }
    #[must_use]
    pub fn host(&self) -> CtraderHost {
        self.host
    }

    /// Load the bounded lightweight symbol catalog for an authorized account.
    ///
    /// # Errors
    /// Rejects an unknown account, missing wire fields, and oversized catalogs.
    pub fn symbol_names(&mut self, account: &CtraderAccount) -> Result<Vec<String>, SessionFault> {
        Ok(self
            .symbol_catalog(account)?
            .into_iter()
            .map(|symbol| symbol.name)
            .collect())
    }

    /// Load the bounded lightweight symbol catalog with ids and descriptions.
    ///
    /// # Errors
    /// Rejects an unknown account, missing wire fields, and oversized catalogs.
    pub fn symbol_catalog(
        &mut self,
        account: &CtraderAccount,
    ) -> Result<Vec<LightSymbol>, SessionFault> {
        self.authorize_account(account, || Err(SessionFault::NeedsReconnect))?;
        let request =
            MarketRequest::symbols_list(account.ctid).map_err(|_| SessionFault::Protocol)?;
        let response = self.request(
            request.payload_type,
            request.payload,
            request.response_type,
            request.bucket,
            REQUEST_TIMEOUT,
        )?;
        decode_symbol_list(&response, account.ctid).map_err(|_| SessionFault::Protocol)
    }

    /// # Errors
    /// Returns a fault when the account is unobserved, mismatches the host,
    /// or authorization fails after one refresh or account re-authorization.
    pub fn authorize_account(
        &mut self,
        account: &CtraderAccount,
        mut force_refresh: impl FnMut() -> Result<AccessToken, SessionFault>,
    ) -> Result<(), SessionFault> {
        if !self.accounts.iter().any(|known| known == account) {
            return Err(SessionFault::Protocol);
        }
        if account.is_live != (self.host == CtraderHost::Live) {
            return Err(SessionFault::Protocol);
        }
        if self.authorized.contains(&account.ctid) {
            return Ok(());
        }
        let ctid = i64::try_from(account.ctid).map_err(|_| SessionFault::Protocol)?;
        let mut account_retried = false;
        loop {
            let auth = ProtoOaAccountAuthReq {
                payload_type: None,
                ctid_trader_account_id: ctid,
                access_token: self.token.0.clone(),
            };
            match self.request(
                2102,
                auth.encode_to_vec(),
                2103,
                Bucket::General,
                REQUEST_TIMEOUT,
            ) {
                Ok(response) => {
                    let confirmed: crate::generated::ProtoOaAccountAuthRes = codec::decode_typed(
                        &response,
                        2103,
                        &[(2, "ctidTraderAccountId")],
                        |_| Ok(()),
                    )?;
                    if confirmed.ctid_trader_account_id != ctid {
                        return Err(SessionFault::Protocol);
                    }
                    self.authorized.insert(account.ctid);
                    return Ok(());
                }
                Err(SessionFault::NeedsReconnect) if !self.refreshed => {
                    self.token = force_refresh()?;
                    self.refreshed = true;
                }
                Err(SessionFault::AccountDisconnect(disconnected))
                    if disconnected == ctid
                        && !account_retried
                        && self.account_disconnect_retries.insert(account.ctid) =>
                {
                    account_retried = true;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// After an invalidation event, drop the old token and reauthorize every
    /// previously active account exactly once with a forced hosted refresh.
    ///
    /// # Errors
    /// A second invalidation returns `NeedsReconnect` without refreshing again.
    pub fn recover_invalidated(
        &mut self,
        mut force_refresh: impl FnMut() -> Result<AccessToken, SessionFault>,
    ) -> Result<(), SessionFault> {
        if self.refreshed {
            return Err(SessionFault::NeedsReconnect);
        }
        self.token = force_refresh()?;
        self.refreshed = true;
        let active: Vec<_> = self
            .accounts
            .iter()
            .filter(|account| self.authorized.contains(&account.ctid))
            .cloned()
            .collect();
        self.authorized.clear();
        for account in &active {
            self.authorize_account(account, || Err(SessionFault::NeedsReconnect))?;
        }
        Ok(())
    }

    /// Enqueue without blocking; the pending map has a hard cap and every id expires.
    ///
    /// # Errors
    /// Returns overflow, cancellation, or a failed transport.
    pub fn send_request(
        &mut self,
        kind: u32,
        payload: Vec<u8>,
        expected: u32,
        bucket: Bucket,
        timeout: Duration,
    ) -> Result<String, SessionFault> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(SessionFault::Protocol);
        }
        self.expire_pending();
        if self.pending.len() >= MAX_PENDING {
            return Err(SessionFault::Overflow);
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = format!("aeris-{}", self.next_id);
        self.transport.send(
            ProtoMessage {
                payload_type: kind,
                payload: Some(payload),
                client_msg_id: Some(id.clone()),
            },
            bucket,
        )?;
        self.pending.insert(
            id.clone(),
            Pending {
                expected,
                deadline: Instant::now() + timeout,
                bucket,
                response: None,
            },
        );
        Ok(id)
    }
    fn expire_pending(&mut self) {
        self.pending
            .retain(|_, pending| pending.deadline > Instant::now());
    }
    /// # Errors
    /// Returns a timeout, protocol fault, or provider fault.
    pub fn await_response(&mut self, id: &str) -> Result<ProtoMessage, SessionFault> {
        loop {
            let pending = self.pending.get_mut(id).ok_or(SessionFault::Timeout)?;
            if let Some(response) = pending.response.take() {
                self.pending.remove(id);
                return response;
            }
            let remaining = pending.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.pending.remove(id);
                return Err(SessionFault::Timeout);
            }
            let frame = match self
                .transport
                .receive(remaining.min(Duration::from_millis(100)))
            {
                Ok(frame) => frame,
                Err(TransportError::ReadTimeout) => continue,
                Err(error) => return Err(error.into()),
            };
            match frame.payload_type {
                2148 => return Err(SessionFault::Reconnect),
                2147 => return Err(SessionFault::NeedsReconnect),
                2164 => {
                    let event: crate::generated::ProtoOaAccountDisconnectEvent =
                        codec::decode_typed(&frame, 2164, &[(2, "ctidTraderAccountId")], |_| {
                            Ok(())
                        })?;
                    return Err(SessionFault::AccountDisconnect(
                        event.ctid_trader_account_id,
                    ));
                }
                2142 => {
                    if frame
                        .client_msg_id
                        .as_deref()
                        .is_some_and(|key| !self.pending.contains_key(key))
                    {
                        continue;
                    }
                    let error: ProtoOaErrorRes =
                        codec::decode_typed(&frame, 2142, &[(3, "errorCode")], |_| Ok(()))?;
                    let target = frame.client_msg_id.as_deref().unwrap_or(id);
                    let bucket = self
                        .pending
                        .get(target)
                        .map_or(Bucket::General, |entry| entry.bucket);
                    let fault = map_error(
                        &error.error_code,
                        error.retry_after,
                        error.maintenance_end_timestamp,
                        bucket,
                    );
                    if let SessionFault::RateLimited { bucket, wait } = fault {
                        self.transport.pause(bucket, wait);
                    }
                    if target != id {
                        if let Some(entry) = self.pending.get_mut(target) {
                            entry.response = Some(Err(fault));
                        }
                        continue;
                    }
                    return Err(fault);
                }
                50 => {
                    let error: ProtoErrorRes =
                        codec::decode_typed(&frame, 50, &[(2, "errorCode")], |_| Ok(()))?;
                    return Err(map_error(
                        &error.error_code,
                        None,
                        error
                            .maintenance_end_timestamp
                            .and_then(|millis| i64::try_from(millis / 1000).ok()),
                        Bucket::General,
                    ));
                }
                _ => {}
            }
            if let Some(key) = frame
                .client_msg_id
                .as_ref()
                .filter(|key| self.pending.contains_key(*key))
            {
                let expected = self.pending[key].expected;
                if frame.payload_type != expected {
                    return Err(SessionFault::Protocol);
                }
                if let Some(entry) = self.pending.get_mut(key) {
                    entry.response = Some(Ok(frame));
                }
            } else if frame.client_msg_id.is_none() {
                if self.events.len() == MAX_EVENTS {
                    return Err(SessionFault::Overflow);
                }
                self.events.push_back(frame);
            }
        }
    }
    /// # Errors
    /// Returns an explicit overflow, timeout, transport, or provider fault.
    pub fn request(
        &mut self,
        kind: u32,
        payload: Vec<u8>,
        expected: u32,
        bucket: Bucket,
        timeout: Duration,
    ) -> Result<ProtoMessage, SessionFault> {
        let id = self.send_request(kind, payload, expected, bucket, timeout)?;
        let result = self.await_response(&id);
        self.pending.remove(&id);
        result
    }
    /// # Errors
    /// Returns a connection fault or invalidated token.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<ProtoMessage>, SessionFault> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        match self.transport.receive(timeout) {
            Ok(frame) if frame.payload_type == 2148 => Err(SessionFault::Reconnect),
            Ok(frame) if frame.payload_type == 2147 => {
                if self.refreshed {
                    Err(SessionFault::NeedsReconnect)
                } else {
                    Err(SessionFault::TokenInvalidated)
                }
            }
            Ok(frame) if frame.payload_type == 2164 => {
                let event: crate::generated::ProtoOaAccountDisconnectEvent =
                    codec::decode_typed(&frame, 2164, &[(2, "ctidTraderAccountId")], |_| Ok(()))?;
                let ctid = u64::try_from(event.ctid_trader_account_id)
                    .map_err(|_| SessionFault::Protocol)?;
                self.authorized.remove(&ctid);
                if !self.account_disconnect_retries.insert(ctid) {
                    return Err(SessionFault::Reconnect);
                }
                Err(SessionFault::AccountDisconnect(
                    event.ctid_trader_account_id,
                ))
            }
            Ok(frame) => Ok(Some(frame)),
            Err(TransportError::ReadTimeout) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    /// Process an invalidation event by fetching one fresh token before
    /// delivering the next application event.
    ///
    /// # Errors
    /// Returns `NeedsReconnect` after a second invalidation or a hosted 401/404.
    pub fn next_event_with_refresh(
        &mut self,
        timeout: Duration,
        force_refresh: impl FnMut() -> Result<AccessToken, SessionFault>,
    ) -> Result<Option<ProtoMessage>, SessionFault> {
        match self.next_event(timeout) {
            Err(SessionFault::TokenInvalidated) => {
                self.recover_invalidated(force_refresh)?;
                Ok(None)
            }
            result => result,
        }
    }
    pub fn close(&mut self) {
        self.transport.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn faults_cover_all_recovery_rows() {
        assert_eq!(
            map_error("CH_CLIENT_AUTH_FAILURE", None, None, Bucket::General),
            SessionFault::ClientAuthFailure
        );
        assert_eq!(hosted_fault(401), SessionFault::NeedsReconnect);
        assert_eq!(hosted_fault(404), SessionFault::NeedsReconnect);
        assert_eq!(
            map_error("CONNECTIONS_LIMIT_EXCEEDED", None, None, Bucket::General),
            SessionFault::ConnectionLimit {
                message: "cTrader connection limit reached",
                wait: Duration::from_secs(300)
            }
        );
        for code in ["BLOCKED_PAYLOAD_TYPE", "REQUEST_FREQUENCY_EXCEEDED"] {
            assert_eq!(
                map_error(code, None, None, Bucket::Historical),
                SessionFault::RateLimited {
                    bucket: Bucket::Historical,
                    wait: Duration::from_secs(1)
                }
            );
            assert_eq!(
                map_error(code, Some(7), None, Bucket::General),
                SessionFault::RateLimited {
                    bucket: Bucket::General,
                    wait: Duration::from_secs(7)
                }
            );
        }
        assert!(matches!(
            map_error(
                "SERVER_IS_UNDER_MAINTENANCE",
                None,
                Some(i64::MAX),
                Bucket::General
            ),
            SessionFault::Maintenance { .. }
        ));
        assert_eq!(
            SessionFault::from(TransportError::Closed),
            SessionFault::Reconnect
        );
        assert_eq!(
            SessionFault::from(TransportError::ReadTimeout),
            SessionFault::Timeout
        );
        let disconnected = SessionFault::AccountDisconnect(987_654);
        assert!(!format!("{disconnected:?} {disconnected}").contains("987654"));
    }
    #[test]
    fn backoff_exponential_capped_jittered_and_resets() {
        let mut backoff = ReconnectBackoff::default();
        let mut saw_jitter = false;
        for min in [1, 2, 4, 8, 16, 32, 60, 60] {
            let wait = backoff.next_delay();
            saw_jitter |= wait.subsec_nanos() != 0;
            assert!(wait >= Duration::from_secs(min) && wait <= Duration::from_secs(60));
        }
        assert!(saw_jitter);
        backoff.healthy();
        assert!(backoff.next_delay() < Duration::from_secs(2));
    }
}
