use crate::VenueRoute;
use aeris_trading::{OrderType, TimeInForce};

/// Trading capabilities of the selected route, independent of provider market data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VenueCapabilities {
    pub client_id_limit: Option<usize>,
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

impl VenueCapabilities {
    pub const fn for_route(route: VenueRoute) -> Self {
        match route {
            VenueRoute::Simulated => Self {
                client_id_limit: None,
                protection: ProtectionSupport::Managed,
                tif_policy: TifPolicy::Simulated,
            },
            VenueRoute::BrokerDemo => Self {
                client_id_limit: Some(50),
                protection: ProtectionSupport::Native,
                tif_policy: TifPolicy::Broker,
            },
        }
    }

    pub const fn effective_tif(self, kind: OrderType, requested: TimeInForce) -> TimeInForce {
        if matches!(kind, OrderType::Market) && matches!(self.tif_policy, TifPolicy::Broker) {
            TimeInForce::ImmediateOrCancel
        } else if matches!(requested, TimeInForce::Day)
            && matches!(self.tif_policy, TifPolicy::Broker)
        {
            TimeInForce::GoodTillCancelled
        } else {
            requested
        }
    }

    pub const fn native_sltp(self) -> bool {
        matches!(self.protection, ProtectionSupport::Native)
    }

    pub const fn managed_trailing(self) -> bool {
        matches!(self.protection, ProtectionSupport::Managed)
    }
}
