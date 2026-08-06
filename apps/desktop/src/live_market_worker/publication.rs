use super::{CoinbaseDesktopWorker, SUBSCRIPTION_ID};
use crate::market_worker::{
    DesktopMarketGeneration, MarketWorkerBootstrap, MarketWorkerMessage, MarketWorkerPublication,
    MarketWorkerSender, PendingUiDiagnostics,
};
use axiusflow_application::{
    MarketBarClientModel, MarketBarModelOutcome, MarketStreamPublication, ProvenancedMarketBar,
    ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate,
};
use axiusflow_chart_integration::ReplayRecoveryCommand;
use axiusflow_desktop_provider_runtime::SessionGeneration;
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::BarDefinition;
use std::collections::VecDeque;

pub(super) fn publish_ready_recovery(
    streaming_generation: Option<SessionGeneration>,
    pending_recovery: &mut VecDeque<ReplayRecoveryCommand>,
    message_tx: &MarketWorkerSender,
    series: (&InstrumentRevision, &BarDefinition),
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> bool {
    streaming_generation.is_none()
        || publish_recovery_commands(
            pending_recovery,
            message_tx,
            series.0,
            series.1,
            retained,
            model,
            worker_label,
        )
}

fn publish_recovery_commands(
    pending_recovery: &mut VecDeque<ReplayRecoveryCommand>,
    message_tx: &MarketWorkerSender,
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> bool {
    while let Some(command) = pending_recovery.pop_front() {
        let result = recovery_snapshot(instrument, bar_definition, retained, model, worker_label);
        if message_tx
            .send(MarketWorkerMessage::Recovery {
                request_id: command.request_id,
                result,
            })
            .is_err()
        {
            return false;
        }
    }
    true
}

pub(super) fn publish_update(
    worker: &mut CoinbaseDesktopWorker,
    provider_generation: SessionGeneration,
    model: &mut MarketBarClientModel,
    update: ReplayStreamUpdate,
    worker_label: &str,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let generation = published_generation(model, update.clone())?;
    let publication = MarketStreamPublication::try_new(
        SUBSCRIPTION_ID.to_string(),
        update.clone(),
        generation.clone(),
    )
    .map_err(|error| error.to_string())?;
    let mut ui_diagnostics = PendingUiDiagnostics::new(provider_generation);
    worker
        .publish(provider_generation, publication)
        .map_err(|error| error.to_string())?;
    ui_diagnostics.mark_ui_enqueue();
    message_tx
        .send(MarketWorkerMessage::Update(MarketWorkerPublication {
            update,
            generation,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: worker_label.to_string(),
            ui_diagnostics: Some(ui_diagnostics),
        }))
        .map_err(|()| "desktop market UI channel disconnected".to_string())
}

fn recovery_snapshot(
    instrument: &InstrumentRevision,
    bar_definition: &BarDefinition,
    retained: &VecDeque<ProvenancedMarketBar>,
    model: &mut MarketBarClientModel,
    worker_label: &str,
) -> Result<MarketWorkerBootstrap, String> {
    let generation = model
        .current_generation()
        .map_or(1, |current| current.generation().saturating_add(1));
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        instrument.clone(),
        ReplayProvenance::LiveProvider,
        bar_definition.clone(),
        generation,
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    let model_generation =
        published_generation(model, ReplayStreamUpdate::Snapshot(snapshot.clone()))?;
    Ok(MarketWorkerBootstrap {
        snapshot,
        subscription_id: SUBSCRIPTION_ID.to_string(),
        generation: model_generation,
        worker_label: worker_label.to_string(),
    })
}

fn published_generation(
    model: &mut MarketBarClientModel,
    update: ReplayStreamUpdate,
) -> Result<DesktopMarketGeneration, String> {
    match model
        .apply_update(update)
        .map_err(|error| error.to_string())?
    {
        MarketBarModelOutcome::Published(generation) => Ok(generation),
        outcome => Err(format!(
            "desktop market update was not published: {outcome:?}"
        )),
    }
}
