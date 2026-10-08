use crate::{
    ProtoMessage,
    codec::{self, CodecError},
    host::CtraderHost,
    session::CtraderSession,
};

#[derive(Clone, Eq, PartialEq)]
pub struct CtraderAccount {
    pub ctid: u64,
    pub is_live: bool,
    pub trader_login: Option<i64>,
    pub broker_title: Option<String>,
}
impl std::fmt::Debug for CtraderAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtraderAccount")
            .field("is_live", &self.is_live)
            .finish_non_exhaustive()
    }
}

impl CtraderAccount {
    /// Reject a missing environment before an account is ever eligible for trading.
    ///
    /// # Errors
    /// Rejects missing required wire fields or a list exceeding the session bound.
    pub fn from_list(message: &ProtoMessage) -> Result<Vec<Self>, CodecError> {
        let list = codec::decode_account_list(message)?;
        if list.ctid_trader_account.len() > 256 {
            return Err(CodecError::FrameTooLarge {
                length: list.ctid_trader_account.len(),
            });
        }
        Ok(list
            .ctid_trader_account
            .into_iter()
            .map(|item| Self {
                ctid: item.ctid_trader_account_id,
                // decode_account_list has already verified this is present.
                is_live: item.is_live == Some(true),
                trader_login: item.trader_login,
                broker_title: item.broker_title_short,
            })
            .collect())
    }
}

/// Only an observed demo-host, non-live account can be passed to trading encoders.
#[derive(Clone, Debug)]
pub struct DemoAccount(CtraderAccount);

impl DemoAccount {
    /// Check the account against an actual Demo-host account list. Callers
    /// cannot assert an environment or manufacture an observed account.
    ///
    /// # Errors
    /// Rejects accounts absent from that session or belonging to a live host.
    pub fn from_session(
        session: &CtraderSession,
        account: &CtraderAccount,
    ) -> Result<Self, &'static str> {
        if !session.accounts().contains(account) {
            return Err("cTrader account was not observed on this session");
        }
        Self::from_observed(session.host(), account.clone())
    }
    /// # Errors
    /// Rejects live accounts or accounts seen on the live host.
    fn from_observed(host: CtraderHost, account: CtraderAccount) -> Result<Self, &'static str> {
        if host != CtraderHost::Demo || account.is_live {
            return Err("cTrader live accounts are data-only; trading is disabled");
        }
        Ok(Self(account))
    }

    #[must_use]
    pub fn account(&self) -> &CtraderAccount {
        &self.0
    }

    /// An observed demo account for encoder tests, which have no session.
    #[cfg(test)]
    pub(crate) fn observed_for_tests(ctid: u64) -> Self {
        Self(CtraderAccount {
            ctid,
            is_live: false,
            trader_login: None,
            broker_title: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_account_requires_observed_demo_host_and_non_live_account() {
        let account = CtraderAccount {
            ctid: 1,
            is_live: false,
            trader_login: None,
            broker_title: None,
        };
        assert!(DemoAccount::from_observed(CtraderHost::Live, account.clone()).is_err());
        assert!(
            DemoAccount::from_observed(
                CtraderHost::Demo,
                CtraderAccount {
                    is_live: true,
                    ..account.clone()
                }
            )
            .is_err()
        );
        assert_eq!(
            DemoAccount::from_observed(CtraderHost::Demo, account)
                .unwrap()
                .account()
                .ctid,
            1
        );
    }

    #[test]
    fn account_debug_redacts_identifiers() {
        let account = CtraderAccount {
            ctid: 987_654,
            is_live: false,
            trader_login: Some(456_789),
            broker_title: None,
        };
        let debug = format!("{account:?}");
        assert!(!debug.contains("987654") && !debug.contains("456789"));
    }
}
