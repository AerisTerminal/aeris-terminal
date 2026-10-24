use crate::VenueRoute;
use aeris_trading::{OrderType, TimeInForce};

/// Order policy of the selected route, independent of provider market data. What an attached
/// broker venue itself supports is the venue's `VenueCapabilities`, declared at attach.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RouteCapabilities {
    protection: ProtectionSupport,
    tif_policy: TifPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtectionSupport {
    Native,
    Managed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TifPolicy {
    Simulated,
    Broker,
}

impl RouteCapabilities {
    pub const fn for_route(route: VenueRoute) -> Self {
        match route {
            VenueRoute::Simulated => Self {
                protection: ProtectionSupport::Managed,
                tif_policy: TifPolicy::Simulated,
            },
            VenueRoute::BrokerDemo => Self {
                protection: ProtectionSupport::Native,
                tif_policy: TifPolicy::Broker,
            },
        }
    }

    /// A broker market order is always immediate-or-cancel; every other instruction keeps
    /// its requested time in force, and the broker reports a DAY order's session expiry.
    pub const fn effective_tif(self, kind: OrderType, requested: TimeInForce) -> TimeInForce {
        if matches!(kind, OrderType::Market) && matches!(self.tif_policy, TifPolicy::Broker) {
            TimeInForce::ImmediateOrCancel
        } else {
            requested
        }
    }

    pub const fn managed_trailing(self) -> bool {
        matches!(self.protection, ProtectionSupport::Managed)
    }
}
