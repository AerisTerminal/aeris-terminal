//! Read-only tastytrade broker authorization transport.
//!
//! The hosted callback owns the confidential OAuth client and provider refresh
//! tokens. The market runtime owns the desktop connection capability and all
//! provider sessions; UI surfaces never receive credentials.

use std::{
    fmt,
    io::Read as _,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aeris_platform_runtime::CancellableHttpClient;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

mod catalog;
mod session;
pub use catalog::{FutureInstrument, PriceIncrementBand, ResolvedInstrument, SearchInstrument};
pub use session::{
    DATA_SCALE, DxlinkSession, FeedEvent, Subscription, SubscriptionChangeBudget,
    SubscriptionChangeError, TradePrint,
};

/// Provider-reported current exchange session. Times are UTC nanoseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketSession {
    pub collection: MarketCollection,
    pub start_unix_nanos: i64,
    pub regular_open_unix_nanos: i64,
    pub regular_close_unix_nanos: i64,
    pub close_unix_nanos: i64,
    pub next_start_unix_nanos: i64,
    pub next_regular_open_unix_nanos: i64,
    pub next_regular_close_unix_nanos: i64,
    pub next_close_unix_nanos: i64,
}

impl MarketSession {
    #[must_use]
    pub fn contains(self, now: i64) -> bool {
        (self.start_unix_nanos..self.close_unix_nanos).contains(&now)
            || (self.next_start_unix_nanos > self.start_unix_nanos
                && (self.next_start_unix_nanos..self.next_close_unix_nanos).contains(&now))
    }

    #[must_use]
    pub fn replay_start(self, now: i64) -> Option<i64> {
        if self.next_start_unix_nanos > self.start_unix_nanos
            && now >= self.next_start_unix_nanos
            && now < self.next_close_unix_nanos
        {
            Some(self.next_start_unix_nanos)
        } else if now >= self.start_unix_nanos
            && (self.next_start_unix_nanos <= self.start_unix_nanos
                || now < self.next_start_unix_nanos)
        {
            Some(self.start_unix_nanos)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketCollection {
    Cme,
    Cfe,
    Equity,
}

/// Broker authorization service; separate from Aeris account authentication.
pub const TASTYTRADE_BROKER_ORIGIN: &str = "https://app.aeristerminal.com";
const TASTYTRADE_API_ORIGIN: &str = "https://api.tastyworks.com";
const MAXIMUM_RESPONSE_BYTES: usize = 512 * 1024;

/// Desktop-held proof of ownership of a hosted broker connection.
#[derive(Serialize, Deserialize)]
pub struct ConnectionCapability {
    connection_id: String,
    proof: String,
}

impl fmt::Debug for ConnectionCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConnectionCapability([redacted])")
    }
}

impl Drop for ConnectionCapability {
    fn drop(&mut self) {
        self.proof.zeroize();
    }
}

impl ConnectionCapability {
    /// Encodes a capability for protected native-vault storage only.
    ///
    /// # Errors
    /// Returns a redacted error if serialization fails.
    pub fn vault_bytes(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        serde_json::to_vec(self)
            .map(Zeroizing::new)
            .map_err(|_| "Broker connection could not be encoded".to_string())
    }

    /// Restores a capability read from the protected native vault.
    ///
    /// # Errors
    /// Rejects malformed or oversized credentials.
    pub fn from_vault(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 {
            return Err("Saved broker connection is oversized".to_string());
        }
        let result: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Saved broker connection is invalid".to_string())?;
        if !valid_opaque(&result.connection_id) || !valid_opaque(&result.proof) {
            return Err("Saved broker connection is invalid".to_string());
        }
        Ok(result)
    }
}

fn valid_opaque(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Hosted authorization phase; contains no provider credentials.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AuthorizationPhase {
    Pending,
    Exchanging,
    Ready,
    Failed,
}

/// Sanitized status returned by the hosted connection owner.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizationStatus {
    pub phase: AuthorizationPhase,
    pub expires_at: u64,
}

/// One new connection and the provider URL to open in the system browser.
pub struct PendingAuthorization {
    pub capability: ConnectionCapability,
    pub authorization_url: String,
    pub expires_at: u64,
}

/// Ephemeral streaming credentials; never publish them to the UI or logs.
#[derive(Deserialize)]
pub struct QuoteToken {
    pub token: String,
    pub dxlink_url: String,
    pub expires_at: String,
    pub level: String,
}

impl Drop for QuoteToken {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

/// Exclusive cancellable REST transport driven by a market-runtime worker.
#[derive(Default)]
pub struct TastytradeBrokerClient {
    http: CancellableHttpClient,
    access: Option<AccessToken>,
    #[cfg(test)]
    test_origin: Option<String>,
}

struct AccessToken {
    token: Zeroizing<String>,
    expires_at: u64,
}

impl TastytradeBrokerClient {
    /// Invalidates credentials after a capability changes or is disconnected.
    pub fn clear_access_token(&mut self) {
        self.access = None;
    }

    fn access_token(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<Zeroizing<String>, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock is invalid")?
            .as_secs();
        if self
            .access
            .as_ref()
            .is_none_or(|access| access.expires_at <= now + 60)
        {
            #[derive(Deserialize)]
            struct Response {
                access_token: String,
                expires_at: u64,
            }
            let response: Response = self.post("access_token", capability, stop)?;
            if response.access_token.is_empty()
                || response.access_token.len() > 16_384
                || response.expires_at <= now + 60
            {
                return Err("Broker access-token response is invalid".into());
            }
            self.access = Some(AccessToken {
                token: Zeroizing::new(response.access_token),
                expires_at: response.expires_at,
            });
        }
        self.access
            .as_ref()
            .map(|access| access.token.clone())
            .ok_or_else(|| "Broker access token is unavailable".into())
    }

    fn get<T: DeserializeOwned>(
        &mut self,
        capability: &ConnectionCapability,
        path: &str,
        query: &[(&str, &str)],
        stop: &Arc<AtomicBool>,
    ) -> Result<T, String> {
        if !path.starts_with('/') || path.contains(['?', '#']) {
            return Err("Tastytrade API path is invalid".into());
        }
        #[cfg(test)]
        let origin = self
            .test_origin
            .as_deref()
            .unwrap_or(TASTYTRADE_API_ORIGIN)
            .to_owned();
        #[cfg(not(test))]
        let origin = TASTYTRADE_API_ORIGIN;
        self.http.set_cancellation(stop);
        let mut refreshed = false;
        let mut limited = 0_u32;
        loop {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return Err("Tastytrade request cancelled".into());
            }
            let access = self.access_token(capability, stop)?;
            let mut request = self.http.agent().get(format!("{origin}{path}"));
            for (name, value) in query {
                request = request.query(name, value);
            }
            let authorization = Zeroizing::new(format!("Bearer {}", access.as_str()));
            let result = request
                .config()
                .timeout_global(Some(Duration::from_secs(10)))
                .max_redirects(0)
                .build()
                .header("Authorization", authorization.as_str())
                .header("Accept", "application/json")
                .header(
                    "User-Agent",
                    concat!("aeris-terminal/", env!("CARGO_PKG_VERSION")),
                )
                .call();
            let mut response = match result {
                Ok(response) => response,
                Err(ureq::Error::StatusCode(401)) if !refreshed => {
                    self.clear_access_token();
                    refreshed = true;
                    continue;
                }
                Err(ureq::Error::StatusCode(429)) if limited < 4 => {
                    limited += 1;
                    let mut jitter = [0_u8; 1];
                    getrandom::fill(&mut jitter).map_err(|_| "System random source unavailable")?;
                    let delay = Duration::from_millis((100_u64 << limited) + u64::from(jitter[0]));
                    let until = std::time::Instant::now() + delay;
                    while std::time::Instant::now() < until {
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            return Err("Tastytrade request cancelled".into());
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    continue;
                }
                Err(ureq::Error::StatusCode(401)) => {
                    return Err("Tastytrade authorization expired; connect again".into());
                }
                Err(ureq::Error::StatusCode(429)) => {
                    return Err("Tastytrade rate limit persists; retry later".into());
                }
                Err(ureq::Error::Timeout(_)) => {
                    return Err("Tastytrade API request timed out".into());
                }
                Err(error) => return Err(format!("Tastytrade API request failed: {error}")),
            };
            let mut bytes = Zeroizing::new(Vec::new());
            response
                .body_mut()
                .as_reader()
                .take(MAXIMUM_RESPONSE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Tastytrade API response could not be read")?;
            if bytes.len() > MAXIMUM_RESPONSE_BYTES {
                return Err("Tastytrade API response exceeded its size limit".into());
            }
            return serde_json::from_slice(&bytes)
                .map_err(|_| "Tastytrade API response is malformed".into());
        }
    }
    /// Creates a desktop-bound authorization transaction.
    ///
    /// # Errors
    /// Returns a redacted error for random-source, configuration or network failure.
    pub fn begin(&mut self, stop: &Arc<AtomicBool>) -> Result<PendingAuthorization, String> {
        #[derive(Deserialize)]
        struct Started {
            connection_id: String,
            authorization_url: String,
            expires_at: u64,
        }
        let mut random = Zeroizing::new([0_u8; 32]);
        getrandom::fill(random.as_mut())
            .map_err(|_| "System random source unavailable".to_string())?;
        let proof = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_ref()));
        let digest = Sha256::digest(proof.as_bytes());
        let challenge: String = digest
            .iter()
            .flat_map(|byte| {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                [
                    char::from(HEX[usize::from(byte >> 4)]),
                    char::from(HEX[usize::from(byte & 15)]),
                ]
            })
            .collect();
        let result: Started = self.post(
            "start",
            &serde_json::json!({"proof_challenge": challenge}),
            stop,
        )?;
        if !valid_opaque(&result.connection_id)
            || !valid_authorization_url(&result.authorization_url)
        {
            return Err("Broker service returned an invalid authorization".to_string());
        }
        Ok(PendingAuthorization {
            capability: ConnectionCapability {
                connection_id: result.connection_id,
                proof: proof.to_string(),
            },
            authorization_url: result.authorization_url,
            expires_at: result.expires_at,
        })
    }

    /// Returns the phase of an existing desktop-owned connection.
    ///
    /// # Errors
    /// Returns a redacted error when the capability expired or the service is unavailable.
    pub fn status(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<AuthorizationStatus, String> {
        self.post("status", capability, stop)
    }

    /// Deletes the hosted connection. Does not claim to revoke the provider grant.
    ///
    /// # Errors
    /// Returns a redacted error for service or network failure.
    pub fn disconnect(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Disconnected {
            disconnected: bool,
        }
        let result: Disconnected = self.post("disconnect", capability, stop)?;
        if !result.disconnected {
            return Err("Broker connection was not disconnected".to_string());
        }
        Ok(())
    }

    /// Obtains a separate `DXLink` token using the hosted OAuth session.
    ///
    /// # Errors
    /// Returns a redacted error for invalid streaming fields or missing entitlement.
    pub fn quote_token(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<QuoteToken, String> {
        #[derive(Deserialize)]
        struct Response {
            data: ProviderQuoteToken,
        }
        #[derive(Deserialize)]
        struct ProviderQuoteToken {
            token: String,
            #[serde(rename = "dxlink-url")]
            dxlink_url: String,
            #[serde(rename = "expires-at")]
            expires_at: String,
            level: String,
        }
        let response: Response = self.get(capability, "/api-quote-tokens", &[], stop)?;
        let token = QuoteToken {
            token: response.data.token,
            dxlink_url: response.data.dxlink_url,
            expires_at: response.data.expires_at,
            level: response.data.level,
        };
        let expiry = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
            .map_err(|_| "Broker quote-token expiry is invalid".to_string())?;
        if token.token.is_empty()
            || token.token.len() > 16_384
            || token.level.is_empty()
            || token.level.len() > 128
            || token.level.bytes().any(|byte| byte.is_ascii_control())
            || !valid_streamer_url(&token.dxlink_url)
            || u64::try_from(expiry.timestamp()).map_or(true, |expiry| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(true, |now| expiry <= now.as_secs())
            })
        {
            return Err("Broker quote-token response is invalid".to_string());
        }
        Ok(token)
    }

    /// Fetches the bounded active-futures catalog in provider page order.
    /// # Errors
    /// Rejects malformed pages or a catalog beyond the configured bound.
    pub fn active_futures(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<FutureInstrument>, String> {
        let mut instruments = Vec::new();
        for page in 0..100 {
            let offset = page.to_string();
            let response: catalog::CatalogPage = self.get(
                capability,
                "/instruments/futures",
                &[
                    ("page-offset", &offset),
                    ("per-page", "100"),
                    ("only-active-futures", "true"),
                ],
                stop,
            )?;
            let items = response.validated()?;
            let complete = items.len() < 100;
            instruments.extend(items);
            if complete {
                return Ok(instruments);
            }
        }
        Err("Tastytrade futures catalog exceeded its page bound".into())
    }

    /// Reads the current provider calendar for the two supported futures collections.
    /// # Errors
    /// Rejects missing collections, malformed times, or an oversized response.
    pub fn current_futures_sessions(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<[MarketSession; 2], String> {
        let response: serde_json::Value = self.get(
            capability,
            "/market-time/futures/sessions/current",
            &[],
            stop,
        )?;
        parse_futures_sessions(response)
    }

    /// Reads the current equity session to bound requested trade backfill.
    /// # Errors
    /// Rejects a missing collection, malformed times, or an oversized response.
    pub fn current_equity_session(
        &mut self,
        capability: &ConnectionCapability,
        stop: &Arc<AtomicBool>,
    ) -> Result<MarketSession, String> {
        let response: serde_json::Value = self.get(
            capability,
            "/market-time/equities/sessions/current",
            &[],
            stop,
        )?;
        parse_equity_session(&response)
    }

    /// Searches the provider's complete instrument universe without retaining it locally.
    /// # Errors
    /// Rejects malformed identities, overload, expired authorization and provider errors.
    pub fn search(
        &mut self,
        capability: &ConnectionCapability,
        query: &str,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<SearchInstrument>, String> {
        if query.len() > 128 || query.chars().any(char::is_control) {
            return Err("Tastytrade search query invalid".into());
        }
        let mut page: catalog::SearchPage = self.get(
            capability,
            "/instruments/search",
            &[("query", query), ("limit", "100")],
            stop,
        )?;
        if page.data.items.len() > 100 {
            return Err("Tastytrade search exceeded its page bound".into());
        }
        // Future Product results describe a contract family, not a streamable instrument.
        page.data
            .items
            .retain(|item| item.instrument_type != "Future Product");
        if page.data.items.len() > 100
            || page.data.items.iter().any(|item| {
                !catalog::valid_identity(&item.symbol)
                    || !matches!(
                        item.instrument_type.as_str(),
                        "Future"
                            | "Equity"
                            | "Index"
                            | "Equity Option"
                            | "Future Option"
                            | "Cryptocurrency"
                            | "Warrant"
                    )
            })
        {
            return Err(format!(
                "Tastytrade search returned unsupported instrument types: {:?}",
                page.data
                    .items
                    .iter()
                    .map(|item| &item.instrument_type)
                    .collect::<std::collections::BTreeSet<_>>()
            ));
        }
        Ok(page.data.items)
    }

    /// Resolves an exact tradable identity to its own streamer symbol and tick schedule.
    /// # Errors
    /// Rejects missing required metadata or a response for another instrument.
    pub fn instrument(
        &mut self,
        capability: &ConnectionCapability,
        instrument: &SearchInstrument,
        stop: &Arc<AtomicBool>,
    ) -> Result<ResolvedInstrument, String> {
        let collection = match instrument.instrument_type.as_str() {
            "Future" => "futures",
            "Equity" | "Index" => "equities",
            "Equity Option" => "equity-options",
            "Future Option" => "future-options",
            "Cryptocurrency" => "cryptocurrencies",
            "Warrant" => "warrants",
            _ => return Err("Tastytrade instrument type is unsupported".into()),
        };
        if !catalog::valid_identity(&instrument.symbol) {
            return Err("Tastytrade instrument identity is invalid".into());
        }
        let symbol = percent_encode_path_segment(&instrument.symbol);
        let response: serde_json::Value = self.get(
            capability,
            &format!("/instruments/{collection}/{symbol}"),
            &[],
            stop,
        )?;
        ResolvedInstrument::from_response(&response, instrument)
    }

    fn post<T: DeserializeOwned>(
        &mut self,
        route: &str,
        payload: &impl Serialize,
        stop: &Arc<AtomicBool>,
    ) -> Result<T, String> {
        #[cfg(test)]
        let origin = self
            .test_origin
            .as_deref()
            .unwrap_or(TASTYTRADE_BROKER_ORIGIN)
            .to_owned();
        #[cfg(not(test))]
        let origin = TASTYTRADE_BROKER_ORIGIN;
        self.http.set_cancellation(stop);
        let raw = Zeroizing::new(
            serde_json::to_string(payload)
                .map_err(|_| "Broker request could not be encoded".to_string())?,
        );
        let mut response = self
            .http
            .agent()
            .post(format!("{origin}/oauth/tastytrade/{route}"))
            .config()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .build()
            .header("Content-Type", "application/json")
            .header(
                "User-Agent",
                concat!("aeris-terminal/", env!("CARGO_PKG_VERSION")),
            )
            .send(raw.as_bytes())
            .map_err(|error| request_error(&error))?;
        let mut bytes = Zeroizing::new(Vec::new());
        response
            .body_mut()
            .as_reader()
            .take(MAXIMUM_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Broker response could not be read".to_string())?;
        if bytes.len() > MAXIMUM_RESPONSE_BYTES {
            return Err("Broker response exceeded its size limit".to_string());
        }
        serde_json::from_slice(&bytes).map_err(|_| "Broker response is malformed".to_string())
    }
}

#[derive(Deserialize)]
struct FuturesSessionsResponse {
    data: FuturesSessionsData,
}

#[derive(Deserialize)]
struct FuturesSessionsData {
    items: Vec<CurrentMarketSession>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct CurrentMarketSession {
    instrument_collection: String,
    start_at: String,
    open_at: Option<String>,
    close_at: String,
    close_at_ext: Option<String>,
    #[serde(rename = "next-session")]
    next_window: MarketSessionWindow,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct MarketSessionWindow {
    start_at: String,
    open_at: Option<String>,
    close_at: String,
    close_at_ext: Option<String>,
}

fn parse_futures_sessions(value: serde_json::Value) -> Result<[MarketSession; 2], String> {
    let items = value
        .pointer("/data/items")
        .and_then(serde_json::Value::as_array)
        .ok_or("Tastytrade futures calendar items missing")?;
    let missing_next = items.iter().any(|item| item.get("next-session").is_none());
    let missing_start = items.iter().any(|item| item.get("start-at").is_none());
    let missing_close = items.iter().any(|item| item.get("close-at").is_none());
    let response: FuturesSessionsResponse = serde_json::from_value(value).map_err(|_| {
        if missing_next {
            "Tastytrade futures calendar next session missing"
        } else if missing_start {
            "Tastytrade futures calendar start missing"
        } else if missing_close {
            "Tastytrade futures calendar close missing"
        } else {
            "Tastytrade futures calendar fields invalid"
        }
    })?;
    if response.data.items.len() > 8 {
        return Err("Tastytrade futures sessions exceeded their bound".into());
    }
    let mut cme = None;
    let mut cfe = None;
    for item in response.data.items {
        let collection = match item.instrument_collection.as_str() {
            "CME" => MarketCollection::Cme,
            "CFE" => MarketCollection::Cfe,
            _ => continue,
        };
        let session = parse_market_session(&item, collection)?;
        let slot = match collection {
            MarketCollection::Cme => &mut cme,
            MarketCollection::Cfe => &mut cfe,
            MarketCollection::Equity => continue,
        };
        if slot.replace(session).is_some() {
            return Err("Tastytrade futures session collection duplicated".into());
        }
    }
    Ok([
        cme.ok_or("Tastytrade CME session missing")?,
        cfe.ok_or("Tastytrade CFE session missing")?,
    ])
}

fn parse_equity_session(value: &serde_json::Value) -> Result<MarketSession, String> {
    let item: CurrentMarketSession = serde_json::from_value(
        value
            .get("data")
            .cloned()
            .ok_or("Tastytrade equity calendar data missing")?,
    )
    .map_err(|_| "Tastytrade equity calendar fields invalid")?;
    if item.instrument_collection != "Equity" {
        return Err("Tastytrade equity calendar collection invalid".into());
    }
    parse_market_session(&item, MarketCollection::Equity)
}

fn parse_market_session(
    item: &CurrentMarketSession,
    collection: MarketCollection,
) -> Result<MarketSession, String> {
    let parse = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .and_then(|time| time.timestamp_nanos_opt())
            .ok_or("Tastytrade market session time is invalid".to_string())
    };
    let start_unix_nanos = parse(&item.start_at)?;
    let regular_open_unix_nanos = parse(item.open_at.as_deref().unwrap_or(&item.start_at))?;
    let regular_close_unix_nanos = parse(&item.close_at)?;
    let close_unix_nanos = parse(item.close_at_ext.as_deref().unwrap_or(&item.close_at))?;
    let next_start_unix_nanos = parse(&item.next_window.start_at)?;
    let next_regular_open_unix_nanos = parse(
        item.next_window
            .open_at
            .as_deref()
            .unwrap_or(&item.next_window.start_at),
    )?;
    let next_regular_close_unix_nanos = parse(&item.next_window.close_at)?;
    let next_close_unix_nanos = parse(
        item.next_window
            .close_at_ext
            .as_deref()
            .unwrap_or(&item.next_window.close_at),
    )?;
    let valid_span = |start: i64, close: i64| {
        close
            .checked_sub(start)
            .is_some_and(|span| span > 0 && span <= 48 * 60 * 60 * 1_000_000_000)
    };
    if !valid_span(start_unix_nanos, close_unix_nanos) {
        return Err("Tastytrade current market session interval is invalid".into());
    }
    if !valid_span(next_start_unix_nanos, next_close_unix_nanos) {
        return Err("Tastytrade next market session interval is invalid".into());
    }
    if !(start_unix_nanos <= regular_open_unix_nanos
        && regular_open_unix_nanos < regular_close_unix_nanos
        && regular_close_unix_nanos <= close_unix_nanos
        && (next_start_unix_nanos <= start_unix_nanos
            || (next_start_unix_nanos <= next_regular_open_unix_nanos
                && next_regular_open_unix_nanos < next_regular_close_unix_nanos
                && next_regular_close_unix_nanos <= next_close_unix_nanos)))
    {
        return Err("Tastytrade regular market session interval is invalid".into());
    }
    // The provider can return an auxiliary next-session window that predates
    // its current window. Keep only the current window eligible for replay then.
    Ok(MarketSession {
        collection,
        start_unix_nanos,
        regular_open_unix_nanos,
        regular_close_unix_nanos,
        close_unix_nanos,
        next_start_unix_nanos,
        next_regular_open_unix_nanos,
        next_regular_close_unix_nanos,
        next_close_unix_nanos,
    })
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn request_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(503) => {
            "Tastytrade application credentials must be configured on AWS first".to_string()
        }
        ureq::Error::StatusCode(401) => "Tastytrade connection expired; connect again".to_string(),
        ureq::Error::StatusCode(409 | 429) => {
            "Tastytrade connection service is busy; retry later".to_string()
        }
        ureq::Error::Timeout(_) => "Tastytrade connection request timed out".to_string(),
        ureq::Error::StatusCode(status) => {
            format!("Tastytrade connection request failed (HTTP {status})")
        }
        _ => "Tastytrade connection service could not be reached".to_string(),
    }
}

fn valid_authorization_url(url: &str) -> bool {
    url.len() <= 2048
        && url.starts_with("https://my.tastytrade.com/auth.html?")
        && !url.bytes().any(|byte| byte.is_ascii_control())
        && !url.contains('#')
}

fn valid_streamer_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("wss://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    url.len() <= 2048
        && !host.contains(['@', ':', '?', '#'])
        && (host == "dxfeed.com" || host.ends_with(".dxfeed.com"))
        && !url.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write as _, net::TcpListener, thread};

    fn fake_api(responses: Vec<(&'static str, u16, String)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fixture");
        let origin = format!("http://{}", listener.local_addr().expect("fixture address"));
        let server = thread::spawn(move || {
            for (path, status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept fixture request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("bound fixture read");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).expect("read fixture request");
                    assert!(read > 0 && request.len() + read <= 8192);
                    request.extend_from_slice(&buffer[..read]);
                }
                let header_end = request
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .expect("fixture header ends")
                    + 4;
                let content_length = String::from_utf8_lossy(&request[..header_end])
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|length| length.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).expect("read fixture body");
                    assert!(read > 0 && request.len() + read <= 8192);
                    request.extend_from_slice(&buffer[..read]);
                }
                let header = String::from_utf8(request).expect("fixture request header");
                assert!(
                    header
                        .lines()
                        .next()
                        .is_some_and(|line| line.contains(path))
                );
                assert!(
                    header
                        .to_ascii_lowercase()
                        .contains("user-agent: aeris-terminal/")
                );
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("write fixture response");
            }
        });
        (origin, server)
    }

    fn fixture_capability() -> ConnectionCapability {
        ConnectionCapability {
            connection_id: "a".repeat(43),
            proof: "p".repeat(43),
        }
    }

    fn fixture_access(expires_at: u64) -> AccessToken {
        AccessToken {
            token: Zeroizing::new("a".repeat(32)),
            expires_at,
        }
    }

    fn future_expiry() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("fixture clock")
            .as_secs()
            + 3_600
    }

    #[test]
    fn direct_api_refreshes_once_after_unauthorized_response() {
        let renewed = serde_json::json!({
            "access_token": "b".repeat(32),
            "expires_at": future_expiry(),
        })
        .to_string();
        let (origin, server) = fake_api(vec![
            ("/instruments/search", 401, "{}".into()),
            ("/oauth/tastytrade/access_token", 200, renewed),
            (
                "/instruments/search",
                200,
                r#"{"data":{"items":[]}}"#.into(),
            ),
        ]);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry())),
            test_origin: Some(origin),
            ..Default::default()
        };
        let result = client.search(
            &fixture_capability(),
            "ES",
            &Arc::new(AtomicBool::new(false)),
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(
            client
                .access
                .as_ref()
                .is_some_and(|access| access.token.starts_with('b'))
        );
        server.join().expect("fixture server completes");
    }

    #[test]
    fn direct_api_retries_rate_limit_with_a_bounded_result() {
        let (origin, server) = fake_api(vec![
            ("/instruments/search", 429, "{}".into()),
            ("/instruments/search", 429, "{}".into()),
            (
                "/instruments/search",
                200,
                r#"{"data":{"items":[]}}"#.into(),
            ),
        ]);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry())),
            test_origin: Some(origin),
            ..Default::default()
        };
        assert!(
            client
                .search(
                    &fixture_capability(),
                    "ES",
                    &Arc::new(AtomicBool::new(false)),
                )
                .is_ok()
        );
        server.join().expect("fixture server completes");
    }

    #[test]
    fn direct_api_stops_after_one_failed_authorization_refresh() {
        let renewed = serde_json::json!({
            "access_token": "b".repeat(32),
            "expires_at": future_expiry(),
        })
        .to_string();
        let (origin, server) = fake_api(vec![
            ("/instruments/search", 401, "{}".into()),
            ("/oauth/tastytrade/access_token", 200, renewed),
            ("/instruments/search", 401, "{}".into()),
        ]);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry())),
            test_origin: Some(origin),
            ..Default::default()
        };
        let error = client
            .search(
                &fixture_capability(),
                "ES",
                &Arc::new(AtomicBool::new(false)),
            )
            .expect_err("second unauthorized response must surface");
        assert_eq!(error, "Tastytrade authorization expired; connect again");
        server.join().expect("fixture server completes");
    }

    #[test]
    fn direct_api_refreshes_access_before_expiry() {
        let renewed = serde_json::json!({
            "access_token": "b".repeat(32),
            "expires_at": future_expiry(),
        })
        .to_string();
        let (origin, server) = fake_api(vec![
            ("/oauth/tastytrade/access_token", 200, renewed),
            (
                "/instruments/search",
                200,
                r#"{"data":{"items":[]}}"#.into(),
            ),
        ]);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry() - 3_550)),
            test_origin: Some(origin),
            ..Default::default()
        };
        assert!(
            client
                .search(
                    &fixture_capability(),
                    "ES",
                    &Arc::new(AtomicBool::new(false)),
                )
                .is_ok()
        );
        server.join().expect("fixture server completes");
    }

    #[test]
    fn direct_api_stops_after_bounded_rate_limit_retries() {
        let responses = (0..5)
            .map(|_| ("/instruments/search", 429, "{}".to_string()))
            .collect();
        let (origin, server) = fake_api(responses);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry())),
            test_origin: Some(origin),
            ..Default::default()
        };
        let error = client
            .search(
                &fixture_capability(),
                "ES",
                &Arc::new(AtomicBool::new(false)),
            )
            .expect_err("persistent rate limit must surface");
        assert_eq!(error, "Tastytrade rate limit persists; retry later");
        server.join().expect("fixture server completes");
    }

    #[test]
    fn current_futures_sessions_validate_both_exchange_windows() {
        let body = r#"{"data":{"items":[{"instrument-collection":"CME","start-at":"2026-10-01T22:00:00Z","close-at":"2026-10-02T21:00:00Z","next-session":{"start-at":"2026-10-04T22:00:00Z","close-at":"2026-10-05T21:00:00Z"}},{"instrument-collection":"CFE","start-at":"2026-10-01T13:00:00Z","close-at":"2026-10-01T21:00:00Z","next-session":{"start-at":"2026-10-02T13:00:00Z","close-at":"2026-10-02T21:00:00Z"}}]}}"#;
        let (origin, server) = fake_api(vec![(
            "/market-time/futures/sessions/current",
            200,
            body.into(),
        )]);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry())),
            test_origin: Some(origin),
            ..Default::default()
        };
        let sessions = client
            .current_futures_sessions(&fixture_capability(), &Arc::new(AtomicBool::new(false)))
            .expect("valid current sessions");
        assert_eq!(sessions[0].collection, MarketCollection::Cme);
        assert_eq!(sessions[1].collection, MarketCollection::Cfe);
        assert!(sessions[0].start_unix_nanos < sessions[0].close_unix_nanos);
        assert_eq!(
            sessions[0].replay_start(sessions[0].start_unix_nanos + 1),
            Some(sessions[0].start_unix_nanos)
        );
        assert_eq!(
            sessions[0].replay_start(sessions[0].next_start_unix_nanos + 1),
            Some(sessions[0].next_start_unix_nanos)
        );
        assert_eq!(
            sessions[0].replay_start(sessions[0].start_unix_nanos - 1),
            None
        );
        server.join().expect("fixture server completes");
    }

    #[test]
    fn current_equity_session_bounds_trade_replay_and_rejects_wrong_collection() {
        let body = r#"{"data":{"instrument-collection":"Equity","start-at":"2026-10-01T08:00:00Z","open-at":"2026-10-01T13:30:00Z","close-at":"2026-10-01T20:00:00Z","close-at-ext":"2026-10-02T00:00:00Z","next-session":{"start-at":"2026-10-01T23:00:00Z","close-at":"2026-10-02T20:00:00Z","close-at-ext":"2026-10-03T00:00:00Z"}}}"#;
        let (origin, server) = fake_api(vec![(
            "/market-time/equities/sessions/current",
            200,
            body.into(),
        )]);
        let mut client = TastytradeBrokerClient {
            access: Some(fixture_access(future_expiry())),
            test_origin: Some(origin),
            ..Default::default()
        };
        let session = client
            .current_equity_session(&fixture_capability(), &Arc::new(AtomicBool::new(false)))
            .expect("valid equity session");
        assert_eq!(session.collection, MarketCollection::Equity);
        assert_eq!(
            session.replay_start(session.next_start_unix_nanos - 1),
            Some(session.start_unix_nanos)
        );
        assert_eq!(
            session.replay_start(session.close_unix_nanos - 1),
            Some(session.next_start_unix_nanos)
        );
        assert_eq!(
            session.replay_start(session.next_start_unix_nanos + 1),
            Some(session.next_start_unix_nanos)
        );
        server.join().expect("fixture server completes");
        let mut wrong: serde_json::Value = serde_json::from_str(body).unwrap();
        wrong["data"]["instrument-collection"] = "CME".into();
        assert!(parse_equity_session(&wrong).is_err());
        let mut reversed: serde_json::Value = serde_json::from_str(body).unwrap();
        reversed["data"]["next-session"]["start-at"] = "2026-09-30T08:00:00Z".into();
        reversed["data"]["next-session"]["close-at-ext"] = "2026-10-01T00:00:00Z".into();
        let current = parse_equity_session(&reversed).expect("current window stays valid");
        assert_eq!(
            current.replay_start(current.start_unix_nanos + 1),
            Some(current.start_unix_nanos)
        );
        assert_eq!(
            current.replay_start(current.next_start_unix_nanos + 1),
            None
        );
    }

    #[test]
    fn futures_calendar_rejects_missing_or_invalid_session_bounds() {
        let missing = serde_json::json!({"data":{"items":[{
            "instrument-collection":"CME",
            "start-at":"2026-10-01T22:00:00Z",
            "close-at":"2026-10-02T21:00:00Z"
        }]}});
        assert!(parse_futures_sessions(missing).is_err());
        let reversed = serde_json::json!({"data":{"items":[{
            "instrument-collection":"CME",
            "start-at":"2026-10-02T22:00:00Z",
            "close-at":"2026-10-02T21:00:00Z",
            "next-session":{"start-at":"2026-10-04T22:00:00Z","close-at":"2026-10-05T21:00:00Z"}
        },{
            "instrument-collection":"CFE",
            "start-at":"2026-10-01T13:00:00Z",
            "close-at":"2026-10-01T21:00:00Z",
            "next-session":{"start-at":"2026-10-02T13:00:00Z","close-at":"2026-10-02T21:00:00Z"}
        }]}});
        assert!(parse_futures_sessions(reversed).is_err());
    }

    #[test]
    fn protected_capability_roundtrip_validates_and_redacts_proof() {
        let capability = ConnectionCapability {
            connection_id: "a".repeat(43),
            proof: "p".repeat(43),
        };
        let bytes = capability.vault_bytes().expect("encode");
        let restored = ConnectionCapability::from_vault(&bytes).expect("restore");
        assert_eq!(restored.proof, capability.proof);
        assert!(!format!("{restored:?}").contains(&capability.proof));
        assert!(ConnectionCapability::from_vault(br#"{"connection_id":"a","proof":"p"}"#).is_err());
        assert!(ConnectionCapability::from_vault(&vec![b'a'; 1025]).is_err());
    }

    #[test]
    fn provider_urls_cannot_redirect_credentials_to_another_origin() {
        assert!(valid_authorization_url(
            "https://my.tastytrade.com/auth.html?state=x"
        ));
        assert!(!valid_authorization_url(
            "https://my.tastytrade.com.attacker.example/auth.html?state=x"
        ));
        assert!(valid_streamer_url(
            "wss://tasty-openapi-ws.dxfeed.com/realtime"
        ));
        assert!(!valid_streamer_url(
            "wss://dxfeed.com@attacker.example/realtime"
        ));
        assert!(!valid_streamer_url(
            "ws://tasty-openapi-ws.dxfeed.com/realtime"
        ));
        assert!(!valid_streamer_url(
            "wss://dxfeed.com.attacker.example/realtime"
        ));
    }

    #[test]
    fn instrument_path_segments_encode_slashes_and_spaces() {
        assert_eq!(percent_encode_path_segment("/ESZ6"), "%2FESZ6");
        assert_eq!(
            percent_encode_path_segment("SPY 261002C00650000"),
            "SPY%20261002C00650000"
        );
    }
}
