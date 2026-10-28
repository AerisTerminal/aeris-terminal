//! What the account's plan permits. Product limits are decided here, at the
//! account boundary, never by the workspace code that enforces them.

/// Plan-imposed limits on top of the desktop's own resource bounds. `None`
/// means the plan adds no limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanPolicy {
    /// Most workspace tabs open at once.
    pub open_workspaces: Option<usize>,
    /// Most studies on one chart.
    pub studies_per_chart: Option<usize>,
    /// Whether intervals beyond the starter set may be selected.
    pub extended_timeframes: bool,
}

impl PlanPolicy {
    /// A plan that adds no limit.
    pub const UNRESTRICTED: Self = Self {
        open_workspaces: None,
        studies_per_chart: None,
        extended_timeframes: true,
    };
}

/// The policy for this desktop. Every signed-in account has the Unlimited
/// plan and a validation root has no account, so nothing is restricted;
/// paid-plan enforcement changes only this function.
#[must_use]
pub const fn plan_policy() -> PlanPolicy {
    PlanPolicy::UNRESTRICTED
}

#[cfg(test)]
mod tests {
    use super::{PlanPolicy, plan_policy};

    #[test]
    fn every_account_is_unrestricted() {
        assert_eq!(plan_policy(), PlanPolicy::UNRESTRICTED);
        assert_eq!(plan_policy().open_workspaces, None);
        assert_eq!(plan_policy().studies_per_chart, None);
        assert!(plan_policy().extended_timeframes);
    }
}
