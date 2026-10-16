//! Bounded command transport for the single trading owner.

pub(crate) mod owner;

use super::{
    BracketPlacement, BracketStrategyTemplate, ClientOrderId, CreatePracticeAccount,
    EconomicEventRiskOutcome, EconomicEventRiskTrigger, FixedPoint, FlattenOutcome, ModifyOrder,
    MovePlannedBracketLeg, Order, PathBuf, PlaceBracket, PlaceChartBracket, PlaceInlineBracket,
    PlaceOrder, PlaceProtectiveOrder, PlannedBracketLeg, Reply, RevisionedCancelOrder,
    RevisionedModifyOrder, RiskEvaluation, RiskProfile, SessionPlan, TradeCopierConfig,
    TradingAccount, TradingAccountId, TradingInstrument, TradingServiceStatus, TradingSnapshot,
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
    RegisterAccount(TradingAccount, Reply<()>),
    CreatePracticeAccount(CreatePracticeAccount, Reply<TradingAccount>),
    DeletePracticeAccount(TradingAccountId, Reply<TradingAccount>),
    RegisterInstrument(TradingInstrument, Reply<()>),
    Place(PlaceOrder, Reply<Order>),
    PlaceFromMarket(PlaceOrder, bool, Reply<Order>),
    PlaceBracket(PlaceBracket, Reply<BracketPlacement>),
    PlaceBracketFromMarket(PlaceBracket, Reply<BracketPlacement>),
    PlaceInlineBracket(PlaceInlineBracket, Reply<BracketPlacement>),
    PlaceInlineBracketFromMarket(PlaceInlineBracket, Reply<BracketPlacement>),
    PlaceChartBracketFromMarket(PlaceChartBracket, Reply<BracketPlacement>),
    PlaceProtective(PlaceProtectiveOrder, Reply<Order>),
    PlaceProtectiveFromMarket(PlaceProtectiveOrder, Reply<Order>),
    EvaluateRisk(PlaceOrder, Reply<RiskEvaluation>),
    Modify(ModifyOrder, Reply<Order>),
    ModifyChecked(RevisionedModifyOrder, Reply<Result<Order, Box<Order>>>),
    ModifyFromMarket(ModifyOrder, Reply<Order>),
    ModifyFromMarketChecked(RevisionedModifyOrder, Reply<Result<Order, Box<Order>>>),
    MovePlannedLeg(MovePlannedBracketLeg, Reply<PlannedBracketLeg>),
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
    RegisterRiskProfile(RiskProfile, Reply<()>),
    RegisterSessionPlan(SessionPlan, Reply<()>),
    RegisterTradeCopier(TradeCopierConfig, Reply<()>),
    RegisterStrategyTemplate(BracketStrategyTemplate, Reply<()>),
    LockAccount(TradingAccountId, String, i64, Reply<()>),
    UnlockAccount(TradingAccountId, Reply<()>),
    KillSwitch(Option<TradingAccountId>, String, i64, Reply<usize>),
    #[cfg(test)]
    Flatten(
        TradingAccountId,
        SimulatedMarketObservation,
        Reply<FlattenOutcome>,
    ),
    #[cfg(test)]
    Reverse(
        TradingAccountId,
        SimulatedMarketObservation,
        Reply<FlattenOutcome>,
    ),
    #[cfg(test)]
    FlattenAll(SimulatedMarketObservation, Reply<FlattenOutcome>),
    FlattenAt(
        TradingAccountId,
        aeris_instruments::InstrumentId,
        Reply<FlattenOutcome>,
    ),
    ReverseAt(
        TradingAccountId,
        aeris_instruments::InstrumentId,
        Reply<FlattenOutcome>,
    ),
    FlattenAllAt(aeris_instruments::InstrumentId, Reply<FlattenOutcome>),
    #[cfg(test)]
    ApplyEconomicEventRisk(
        EconomicEventRiskTrigger,
        Option<SimulatedMarketObservation>,
        Reply<EconomicEventRiskOutcome>,
    ),
    ApplyEconomicEventRiskAt(
        EconomicEventRiskTrigger,
        Option<aeris_instruments::InstrumentId>,
        Reply<EconomicEventRiskOutcome>,
    ),
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
