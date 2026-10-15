//! Bounded command transport for the single trading owner.

pub(crate) mod owner;

use super::{
    BracketPlacement, BracketStrategyTemplate, ClientOrderId, CreatePracticeAccount,
    EconomicEventRiskOutcome, EconomicEventRiskTrigger, Fill, FixedPoint, FlattenOutcome,
    ModifyOrder, MovePlannedBracketLeg, Order, PathBuf, PlaceBracket, PlaceInlineBracket,
    PlaceOrder, PlaceProtectiveOrder, PlannedBracketLeg, PracticeMarketSession, Receiver, Reply,
    RiskEvaluation, RiskProfile, SessionPlan, SimulatedMarketObservation, TradeCopierConfig,
    TradingAccount, TradingAccountId, TradingInstrument, TradingServiceStatus, TradingSnapshot,
    UserRecord, VenueRequest,
};
use std::sync::Arc;
#[cfg(test)]
use std::sync::mpsc::SyncSender;

pub(crate) enum Command {
    Status(Reply<TradingServiceStatus>),
    RegisterAccount(TradingAccount, Reply<()>),
    CreatePracticeAccount(CreatePracticeAccount, Reply<TradingAccount>),
    DeletePracticeAccount(TradingAccountId, Reply<TradingAccount>),
    RegisterInstrument(TradingInstrument, Reply<()>),
    InstallPracticeMarketSession(Arc<PracticeMarketSession>, Reply<()>),
    Place(PlaceOrder, Reply<Order>),
    PlaceBracket(PlaceBracket, Reply<BracketPlacement>),
    PlaceInlineBracket(PlaceInlineBracket, Reply<BracketPlacement>),
    PlaceProtective(PlaceProtectiveOrder, Reply<Order>),
    EvaluateRisk(PlaceOrder, Reply<RiskEvaluation>),
    Modify(ModifyOrder, Reply<Order>),
    MovePlannedLeg(MovePlannedBracketLeg, Reply<PlannedBracketLeg>),
    Cancel(ClientOrderId, Reply<Order>),
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
    Flatten(
        TradingAccountId,
        SimulatedMarketObservation,
        Reply<FlattenOutcome>,
    ),
    Reverse(
        TradingAccountId,
        SimulatedMarketObservation,
        Reply<FlattenOutcome>,
    ),
    FlattenAll(SimulatedMarketObservation, Reply<FlattenOutcome>),
    ApplyEconomicEventRisk(
        EconomicEventRiskTrigger,
        Option<SimulatedMarketObservation>,
        Reply<EconomicEventRiskOutcome>,
    ),
    Observe(SimulatedMarketObservation, Reply<Vec<Fill>>),
    DrainObservations,
    PutUserRecord(UserRecord, Reply<()>),
    Snapshot(Reply<TradingSnapshot>),
    Export(PathBuf, Reply<()>),
    AttachVenue(Reply<(u64, Receiver<VenueRequest>)>),
    DrainVenueEvents,
    #[cfg(test)]
    BlockOwnerForTest(SyncSender<()>, Arc<std::sync::Barrier>),
    #[cfg(test)]
    OwnerDiagnostics(Reply<(String, usize)>),
    Shutdown(Reply<()>),
}
