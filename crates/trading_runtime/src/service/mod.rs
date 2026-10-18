//! Bounded command transport for the single trading owner.

pub(crate) mod owner;

use super::{
    BracketStrategyTemplate, ClientOrderId, CreatePracticeAccount, EconomicEventRiskOutcome,
    EconomicEventRiskTrigger, EmergencyOutcome, FeeSchedule, FixedPoint, FlattenOutcome,
    FlattenScope, ModifyOrder, MovePlannedLeg, Order, PathBuf, PlaceBracket, PlaceChartBracket,
    PlaceInlineBracket, PlaceOco, PlaceOrder, PlaceProtection, PlacementOutcome, PlannedLeg, Reply,
    RevisionedCancelOrder, RevisionedModifyOrder, RiskEvaluation, RiskProfile, SessionPlan,
    TradeCopierConfig, TradingAccount, TradingAccountId, TradingInstrument, TradingServiceStatus,
    TradingSnapshot, UserRecord,
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
    Place(PlaceOrder, Reply<Order>),
    PlaceFromMarket(PlaceOrder, bool, Reply<Order>),
    PlaceBracket(PlaceBracket, Reply<PlacementOutcome>),
    PlaceOco(PlaceOco, Reply<[Order; 2]>),
    PlaceBracketFromMarket(PlaceBracket, Reply<PlacementOutcome>),
    PlaceInlineBracket(PlaceInlineBracket, Reply<PlacementOutcome>),
    PlaceInlineBracketFromMarket(PlaceInlineBracket, Reply<PlacementOutcome>),
    PlaceChartBracketFromMarket(PlaceChartBracket, Reply<PlacementOutcome>),
    PlaceProtective(PlaceProtection, Reply<Order>),
    PlaceProtectiveFromMarket(PlaceProtection, Reply<Order>),
    EvaluateRisk(PlaceOrder, Reply<RiskEvaluation>),
    Modify(ModifyOrder, Reply<Order>),
    ModifyChecked(RevisionedModifyOrder, Reply<Result<Order, Box<Order>>>),
    ModifyFromMarket(ModifyOrder, Reply<Order>),
    ModifyFromMarketChecked(RevisionedModifyOrder, Reply<Result<Order, Box<Order>>>),
    MovePlannedLeg(MovePlannedLeg, Reply<PlannedLeg>),
    Cancel(ClientOrderId, Reply<Order>),
    CancelChecked(RevisionedCancelOrder, Reply<Result<Order, Box<Order>>>),
    CancelAll(Option<TradingAccountId>, Reply<Vec<Order>>),
    CloseBrokerPosition(TradingAccountId, String, Reply<()>),
    AmendBrokerPositionSltp(
        TradingAccountId,
        String,
        Option<FixedPoint>,
        Option<FixedPoint>,
        Reply<()>,
    ),
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
    Snapshot(Reply<TradingSnapshot>),
    Export(PathBuf, Reply<()>),
    AttachVenue(Reply<(u64, aeris_trading::venue_outbox::VenueRequestReceiver)>),
    DrainVenueEvents,
    #[cfg(test)]
    BlockOwnerForTest(SyncSender<()>, Arc<std::sync::Barrier>),
    #[cfg(test)]
    OwnerDiagnostics(Reply<(String, usize)>),
    Shutdown(Reply<()>),
}
