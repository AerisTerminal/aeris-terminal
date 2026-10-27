//! Bounded command transport for the single trading owner.

pub(crate) mod owner;

#[cfg(any(test, feature = "test-support"))]
use super::TradingSnapshot;
use super::{
    BracketStrategyTemplate, ClientOrderId, CreatePracticeAccount, EconomicEventRiskOutcome,
    EconomicEventRiskTrigger, EmergencyOutcome, FeeSchedule, FlattenOutcome, FlattenScope,
    ModifyOrder, MoveBracketLevel, MovePlannedLeg, Order, PathBuf, PlaceBracket, PlaceChartBracket,
    PlaceInlineBracket, PlaceOco, PlaceOrder, PlaceProtection, PlacementOutcome, PlannedLeg, Reply,
    RevisionedCancelOrder, RevisionedModifyOrder, RiskEvaluation, RiskProfile, SessionPlan,
    TradeCopierConfig, TradingAccount, TradingAccountId, TradingInstrument, TradingServiceStatus,
    UserRecord,
};
#[cfg(test)]
use crate::SimulatedMarketObservation;
#[cfg(test)]
use aeris_trading::Fill;
use aeris_trading::market::{MarketRoute, QuoteInterestSink};
use std::sync::Arc;
#[cfg(test)]
use std::sync::mpsc::SyncSender;

pub(crate) enum Command {
    Status(Reply<TradingServiceStatus>),
    Subscribe(Reply<(u64, Arc<crate::core::publication::SubscriberSlot>)>),
    Unsubscribe(u64),
    WorkingSet(Reply<crate::core::publication::WorkingSet>),
    RegisterAccount(TradingAccount, Reply<()>),
    CreatePracticeAccount(CreatePracticeAccount, Reply<TradingAccount>),
    DeletePracticeAccount(TradingAccountId, Reply<TradingAccount>),
    RegisterInstrument(TradingInstrument, Reply<()>),
    Place(PlaceOrder, bool, Reply<Order>),
    #[cfg(test)]
    CallerPlace(PlaceOrder, Reply<Order>),
    PlaceBracket(PlaceBracket, Reply<PlacementOutcome>),
    PlaceOco(PlaceOco, Reply<[Order; 2]>),
    #[cfg(test)]
    CallerOco(PlaceOco, Reply<[Order; 2]>),
    PlaceInlineBracket(PlaceInlineBracket, Reply<PlacementOutcome>),
    #[cfg(test)]
    CallerInlineBracket(PlaceInlineBracket, Reply<PlacementOutcome>),
    PlaceChartBracket(PlaceChartBracket, Reply<PlacementOutcome>),
    PlaceProtective(PlaceProtection, Reply<Order>),
    #[cfg(test)]
    CallerProtective(PlaceProtection, Reply<Order>),
    EvaluateRisk(PlaceOrder, Reply<RiskEvaluation>),
    Modify(ModifyOrder, Reply<Order>),
    #[cfg(test)]
    CallerModify(ModifyOrder, Reply<Order>),
    ModifyChecked(RevisionedModifyOrder, Reply<Result<Order, Box<Order>>>),
    MovePlannedLeg(MovePlannedLeg, Reply<PlannedLeg>),
    Cancel(ClientOrderId, Reply<Order>),
    CancelChecked(RevisionedCancelOrder, Reply<Result<Order, Box<Order>>>),
    CancelAll(Option<TradingAccountId>, Reply<Vec<Order>>),
    MoveBracketLevel(MoveBracketLevel, Reply<()>),
    SetFeeSchedule(TradingAccountId, FeeSchedule, Reply<()>),
    RegisterRiskProfile(RiskProfile, Reply<()>),
    RegisterSessionPlan(SessionPlan, Reply<()>),
    RegisterTradeCopier(TradeCopierConfig, Reply<()>),
    RegisterStrategyTemplate(BracketStrategyTemplate, Reply<()>),
    LockAccount(TradingAccountId, String, i64, Reply<()>),
    UnlockAccount(TradingAccountId, Reply<()>),
    SetRiskSessionBoundary(TradingAccountId, crate::RiskSessionBoundary, Reply<()>),
    KillSwitch(
        Option<TradingAccountId>,
        String,
        i64,
        Reply<EmergencyOutcome>,
    ),
    Flatten(FlattenScope, Reply<FlattenOutcome>),
    ReverseAt(
        TradingAccountId,
        aeris_instruments::InstrumentId,
        Reply<FlattenOutcome>,
    ),
    ApplyEconomicEventRisk(EconomicEventRiskTrigger, Reply<EconomicEventRiskOutcome>),
    #[cfg(test)]
    Observe(SimulatedMarketObservation, Reply<Vec<Fill>>),
    AttachMarketFeed(Arc<dyn QuoteInterestSink>, Reply<()>),
    SetTicketInterest(Vec<MarketRoute>, Reply<()>),
    DrainMarketFeed,
    PutUserRecord(UserRecord, Reply<()>),
    #[cfg(any(test, feature = "test-support"))]
    Snapshot(Reply<TradingSnapshot>),
    Export(PathBuf, Reply<()>),
    AttachVenue(
        aeris_trading::venue::VenueCapabilities,
        Reply<(u64, aeris_trading::venue_outbox::VenueRequestReceiver)>,
    ),
    DrainVenueEvents,
    #[cfg(test)]
    BlockOwnerForTest(SyncSender<()>, Arc<std::sync::Barrier>),
    #[cfg(test)]
    OwnerDiagnostics(Reply<(String, usize)>),
    Shutdown(Reply<()>),
}
