/// A connection's environment determines both its endpoint and account eligibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CtraderHost {
    Demo,
    Live,
}

impl CtraderHost {
    pub const PORT: u16 = 5035;

    #[must_use]
    pub const fn hostname(self) -> &'static str {
        match self {
            Self::Demo => "demo.ctraderapi.com",
            Self::Live => "live.ctraderapi.com",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_and_live_hosts_use_separate_endpoints() {
        assert_eq!(CtraderHost::Demo.hostname(), "demo.ctraderapi.com");
        assert_eq!(CtraderHost::Live.hostname(), "live.ctraderapi.com");
        assert_eq!(CtraderHost::PORT, 5035);
    }
}
