//! The pane's price alerts: the persisted alert list, the create dialog and triggered announcements.

use super::{
    ChartAlertCreateRequest, ChartAlertPriceScale, InstallProviderInstrument,
    MAXIMUM_PRICE_ALERTS_PER_CONSUMER, MarketPriceAlertTrigger, PriceAlertCondition,
    PriceAlertFrequency, PriceAlertStatus, WorkspacePriceAlertState,
};
use num_traits::ToPrimitive;
use std::collections::VecDeque;
use std::fmt::Write as _;

const PRICE_SCALE_RIGHT: u32 = 0;
const PRICE_SCALE_LEFT: u32 = 1;
const PRICE_SCALE_OVERLAY: u32 = 2;
/// Triggers a pane holds until the toaster takes them; older ones give way past this.
const PENDING_ANNOUNCEMENTS: usize = 8;

#[derive(Clone)]
pub(super) struct PriceAlertDialogState {
    pub(super) request: ChartAlertCreateRequest,
    pub(super) instrument: InstallProviderInstrument,
    pub(super) condition: PriceAlertCondition,
    pub(super) frequency: PriceAlertFrequency,
    pub(super) open_dropdown: Option<PriceAlertDropdown>,
}

/// A triggered alert waiting to be shown as a toast.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PriceAlertAnnouncement {
    pub(super) title: String,
    pub(super) description: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PriceAlertDropdown {
    Condition,
    Frequency,
}

/// Whether the restored alerts' failed runtime install still owes the trader a notice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestoreNotice {
    PendingFailure,
    Handled,
}

/// What a runtime trigger changed in the pane's alerts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlertTriggerOutcome {
    /// The trigger names no alert this pane holds.
    Unknown,
    /// The alert was announced and stays active.
    Announced,
    /// The alert was announced and is now triggered, a persisted change.
    Retired,
}

pub(super) struct AlertsController {
    alerts: Vec<WorkspacePriceAlertState>,
    dialog: Option<PriceAlertDialogState>,
    restore_notice: RestoreNotice,
    announcements: VecDeque<PriceAlertAnnouncement>,
}

impl AlertsController {
    /// Adopts restored alerts; `installed` says whether the runtime accepted them.
    pub(super) fn restore(alerts: Vec<WorkspacePriceAlertState>, installed: bool) -> Self {
        let restore_notice = if installed || alerts.is_empty() {
            RestoreNotice::Handled
        } else {
            RestoreNotice::PendingFailure
        };
        Self {
            alerts,
            dialog: None,
            restore_notice,
            announcements: VecDeque::new(),
        }
    }

    pub(super) fn alerts(&self) -> &[WorkspacePriceAlertState] {
        &self.alerts
    }

    pub(super) const fn dialog(&self) -> Option<&PriceAlertDialogState> {
        self.dialog.as_ref()
    }

    pub(super) const fn dialog_mut(&mut self) -> Option<&mut PriceAlertDialogState> {
        self.dialog.as_mut()
    }

    pub(super) fn open_dialog(&mut self, dialog: PriceAlertDialogState) {
        self.dialog = Some(dialog);
    }

    pub(super) fn close_dialog(&mut self) {
        self.dialog = None;
    }

    /// Reports a failed restore install exactly once.
    pub(super) fn take_restore_failure(&mut self) -> bool {
        std::mem::replace(&mut self.restore_notice, RestoreNotice::Handled)
            == RestoreNotice::PendingFailure
    }

    /// The alert list with the open dialog's alert added, or why it cannot be created.
    pub(super) fn with_dialog_alert(
        &self,
        created_at_unix_nanos: i64,
    ) -> Result<Vec<WorkspacePriceAlertState>, &'static str> {
        let Some(dialog) = &self.dialog else {
            return Err("Select a live market before creating an alert.");
        };
        if self.alerts.len() >= MAXIMUM_PRICE_ALERTS_PER_CONSUMER {
            return Err("This chart has reached its alert limit.");
        }
        let instrument = &dialog.instrument;
        let price = scaled_price(dialog.request.price, instrument.price_scale)
            .ok_or("The selected alert price is invalid.")?;
        let id = new_alert_id(&self.alerts)
            .map_err(|_| "An alert identity could not be allocated. Try again.")?;
        let pane_index = u32::try_from(dialog.request.pane_index)
            .map_err(|_| "The selected chart pane is invalid.")?;
        let mut next = self.alerts.clone();
        next.push(WorkspacePriceAlertState {
            id,
            instrument: Some(instrument.clone()),
            price,
            pane_index,
            price_scale_side: persisted_price_scale(dialog.request.price_scale),
            condition: dialog.condition as i32,
            frequency: dialog.frequency as i32,
            status: PriceAlertStatus::Active as i32,
            created_at_unix_nanos,
        });
        Ok(next)
    }

    /// The alert list without `alert_id`, or `None` when the pane holds no such alert.
    pub(super) fn without_alert(&self, alert_id: &str) -> Option<Vec<WorkspacePriceAlertState>> {
        let next: Vec<_> = self
            .alerts
            .iter()
            .filter(|alert| alert.id != alert_id)
            .cloned()
            .collect();
        (next.len() != self.alerts.len()).then_some(next)
    }

    /// Adopts a list the runtime accepted; a created alert also closes its dialog.
    pub(super) fn adopt(&mut self, alerts: Vec<WorkspacePriceAlertState>) {
        self.alerts = alerts;
    }

    pub(super) fn adopt_created(&mut self, alerts: Vec<WorkspacePriceAlertState>) {
        self.alerts = alerts;
        self.dialog = None;
    }

    pub(super) fn apply_trigger(
        &mut self,
        trigger: &MarketPriceAlertTrigger,
        announcement: PriceAlertAnnouncement,
    ) -> AlertTriggerOutcome {
        let Some(alert) = self
            .alerts
            .iter_mut()
            .find(|alert| alert.id == trigger.alert_id)
        else {
            return AlertTriggerOutcome::Unknown;
        };
        let outcome = if trigger.remains_active {
            AlertTriggerOutcome::Announced
        } else {
            alert.status = PriceAlertStatus::Triggered as i32;
            AlertTriggerOutcome::Retired
        };
        if self.announcements.len() == PENDING_ANNOUNCEMENTS {
            self.announcements.pop_front();
        }
        self.announcements.push_back(announcement);
        outcome
    }

    pub(super) fn take_announcements(&mut self) -> Vec<PriceAlertAnnouncement> {
        self.announcements.drain(..).collect()
    }
}

pub(super) const fn chart_price_scale(value: u32) -> Option<ChartAlertPriceScale> {
    match value {
        PRICE_SCALE_RIGHT => Some(ChartAlertPriceScale::Right),
        PRICE_SCALE_LEFT => Some(ChartAlertPriceScale::Left),
        PRICE_SCALE_OVERLAY => Some(ChartAlertPriceScale::Overlay),
        _ => None,
    }
}

const fn persisted_price_scale(value: ChartAlertPriceScale) -> u32 {
    match value {
        ChartAlertPriceScale::Right => PRICE_SCALE_RIGHT,
        ChartAlertPriceScale::Left => PRICE_SCALE_LEFT,
        ChartAlertPriceScale::Overlay => PRICE_SCALE_OVERLAY,
    }
}

pub(super) fn scaled_price(price: f64, scale: u32) -> Option<i64> {
    if !price.is_finite() || scale > 18 {
        return None;
    }
    (price * 10_f64.powi(i32::try_from(scale).ok()?))
        .round()
        .to_i64()
}

fn random_alert_id() -> Result<String, String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| "system CSPRNG is unavailable".to_string())?;
    let mut id = String::with_capacity(32);
    for byte in random {
        write!(&mut id, "{byte:02x}")
            .map_err(|_| "alert identity could not be built".to_string())?;
    }
    Ok(id)
}

fn new_alert_id(alerts: &[WorkspacePriceAlertState]) -> Result<String, String> {
    for _ in 0..4 {
        let id = random_alert_id()?;
        if alerts.iter().all(|alert| alert.id != id) {
            return Ok(id);
        }
    }
    Err("a unique alert identity could not be allocated".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(id: &str) -> WorkspacePriceAlertState {
        WorkspacePriceAlertState {
            id: id.to_string(),
            status: PriceAlertStatus::Active as i32,
            ..WorkspacePriceAlertState::default()
        }
    }

    fn trigger(alert_id: &str, remains_active: bool) -> MarketPriceAlertTrigger {
        MarketPriceAlertTrigger {
            consumer_id: aeris_market_runtime::MarketConsumerId(std::num::NonZeroU64::MIN),
            alert_id: alert_id.to_string(),
            instrument: InstallProviderInstrument::default(),
            threshold_price: 100,
            observed_price: 101,
            condition: PriceAlertCondition::CrossingUp,
            frequency: PriceAlertFrequency::OnlyOnce,
            observed_unix_nanos: 1,
            remains_active,
        }
    }

    fn announcement(title: &str) -> PriceAlertAnnouncement {
        PriceAlertAnnouncement {
            title: title.to_string(),
            description: String::new(),
        }
    }

    #[test]
    fn a_failed_restore_install_is_reported_once() {
        let mut restored = AlertsController::restore(vec![alert("a")], false);
        assert!(restored.take_restore_failure());
        assert!(!restored.take_restore_failure());
        let mut empty = AlertsController::restore(Vec::new(), false);
        assert!(
            !empty.take_restore_failure(),
            "nothing to restore owes no notice"
        );
    }

    #[test]
    fn deleting_reports_unknown_alerts_and_keeps_the_rest() {
        let mut alerts = AlertsController::restore(vec![alert("a"), alert("b")], true);
        assert!(alerts.without_alert("missing").is_none());
        let next = alerts.without_alert("a").expect("known alert");
        alerts.adopt(next);
        assert_eq!(alerts.alerts().len(), 1);
        assert_eq!(alerts.alerts()[0].id, "b");
    }

    #[test]
    fn creating_needs_an_open_dialog() {
        let alerts = AlertsController::restore(Vec::new(), true);
        assert_eq!(
            alerts.with_dialog_alert(1).err(),
            Some("Select a live market before creating an alert.")
        );
    }

    #[test]
    fn triggers_retire_one_shot_alerts_and_keep_the_newest_announcements() {
        let mut alerts = AlertsController::restore(vec![alert("once"), alert("every")], true);
        assert_eq!(
            alerts.apply_trigger(&trigger("missing", false), announcement("x")),
            AlertTriggerOutcome::Unknown
        );
        assert_eq!(
            alerts.apply_trigger(&trigger("every", true), announcement("every")),
            AlertTriggerOutcome::Announced
        );
        assert_eq!(
            alerts.apply_trigger(&trigger("once", false), announcement("once")),
            AlertTriggerOutcome::Retired
        );
        assert_eq!(
            alerts.alerts()[0].status,
            PriceAlertStatus::Triggered as i32
        );
        for index in 0..PENDING_ANNOUNCEMENTS {
            alerts.apply_trigger(&trigger("every", true), announcement(&index.to_string()));
        }
        let taken = alerts.take_announcements();
        assert_eq!(taken.len(), PENDING_ANNOUNCEMENTS);
        assert_eq!(taken[0].title, "0");
        assert_eq!(alerts.take_announcements(), []);
    }

    #[test]
    fn price_scale_sides_round_trip() {
        for side in [PRICE_SCALE_RIGHT, PRICE_SCALE_LEFT, PRICE_SCALE_OVERLAY] {
            let scale = chart_price_scale(side).expect("known side");
            assert_eq!(persisted_price_scale(scale), side);
        }
        assert!(chart_price_scale(3).is_none());
    }
}
