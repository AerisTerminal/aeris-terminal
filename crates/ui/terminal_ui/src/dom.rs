use axiusflow_instruments::InstrumentPrecision;
use axiusflow_market_data::{
    DepthDelta, DepthSnapshot, DomColumnLevel, DomFrame, DomRow, MarketDataValidationError,
    MarketEvent, OrderBook, OrderBookApplyOutcome, OrderBookPublication, OrderBookRecoveryReason,
};
use std::num::NonZeroUsize;

/// Identity and display precision for one selected depth stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomSelection {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub session_generation: u64,
    pub selection_generation: u64,
    pub precision: InstrumentPrecision,
}

impl DomSelection {
    fn matches_snapshot(&self, snapshot: &DepthSnapshot) -> bool {
        self.matches_metadata(
            &snapshot.metadata.provider_id,
            &snapshot.metadata.instrument_id,
            &snapshot.metadata.entitlement_id,
            snapshot.metadata.session_generation,
        )
    }

    fn matches_delta(&self, delta: &DepthDelta) -> bool {
        self.matches_metadata(
            &delta.metadata.provider_id,
            &delta.metadata.instrument_id,
            &delta.metadata.entitlement_id,
            delta.metadata.session_generation,
        )
    }

    fn matches_metadata(
        &self,
        provider_id: &str,
        instrument_id: &str,
        entitlement_id: &str,
        session_generation: u64,
    ) -> bool {
        self.provider_id == provider_id
            && self.instrument_id == instrument_id
            && self.entitlement_id == entitlement_id
            && self.session_generation == session_generation
    }
}

/// Result of offering a canonical event to the selected DOM runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomUpdateOutcome {
    Published(DomFrame),
    RecoveryRequired(DomFrame, OrderBookRecoveryReason),
    Ignored,
}

/// Single-writer, bounded order-book runtime and display projection.
///
/// Callers run this outside the UI thread and publish only the latest immutable
/// [`DomFrame`] on a frame boundary.
pub struct ReadOnlyDom {
    maximum_levels: NonZeroUsize,
    selection: Option<DomSelection>,
    book: OrderBook,
}

impl ReadOnlyDom {
    #[must_use]
    pub fn new(maximum_levels: NonZeroUsize) -> Self {
        Self {
            maximum_levels,
            selection: None,
            book: OrderBook::new(maximum_levels),
        }
    }

    /// Replaces the selected stream and fences every prior selection immediately.
    pub fn select(&mut self, selection: DomSelection) {
        self.selection = Some(selection);
        self.book = OrderBook::new(self.maximum_levels);
    }

    /// Clears the selected stream and all retained depth.
    pub fn clear(&mut self) {
        self.selection = None;
        self.book = OrderBook::new(self.maximum_levels);
    }

    #[must_use]
    pub const fn selection(&self) -> Option<&DomSelection> {
        self.selection.as_ref()
    }

    /// Applies one depth event when it belongs to the exact current selection.
    ///
    /// # Errors
    ///
    /// Returns canonical validation and gap errors after the book has failed
    /// closed into recovery.
    pub fn apply_event(
        &mut self,
        event: &MarketEvent,
    ) -> Result<DomUpdateOutcome, MarketDataValidationError> {
        let Some(selection) = self.selection.as_ref() else {
            return Ok(DomUpdateOutcome::Ignored);
        };
        let outcome = match event {
            MarketEvent::DepthSnapshot(snapshot) if selection.matches_snapshot(snapshot) => {
                self.book.install_snapshot(snapshot)?
            }
            MarketEvent::DepthDelta(delta) if selection.matches_delta(delta) => {
                self.book.apply_delta(delta)?
            }
            _ => return Ok(DomUpdateOutcome::Ignored),
        };
        Ok(self.project_outcome(outcome))
    }

    /// Marks the last valid image stale and returns the updated immutable frame.
    #[must_use]
    pub fn mark_stale(&mut self) -> Option<DomFrame> {
        self.selection.as_ref()?;
        self.book.mark_stale();
        Some(self.project(&self.book.publication()))
    }

    /// Returns the current frame, including the initial awaiting-snapshot state.
    #[must_use]
    pub fn frame(&self) -> Option<DomFrame> {
        self.selection
            .as_ref()
            .map(|_| self.project(&self.book.publication()))
    }

    fn project_outcome(&self, outcome: OrderBookApplyOutcome) -> DomUpdateOutcome {
        match outcome {
            OrderBookApplyOutcome::Published(publication) => {
                DomUpdateOutcome::Published(self.project(&publication))
            }
            OrderBookApplyOutcome::RecoveryRequired(reason) => {
                DomUpdateOutcome::RecoveryRequired(self.project(&self.book.publication()), reason)
            }
            OrderBookApplyOutcome::IgnoredStale => DomUpdateOutcome::Ignored,
        }
    }

    fn project(&self, publication: &OrderBookPublication) -> DomFrame {
        let selection = self
            .selection
            .as_ref()
            .expect("DOM projection requires an active selection");
        let maximum_quantity = publication
            .bids
            .iter()
            .chain(&publication.asks)
            .map(|level| level.quantity)
            .max()
            .unwrap_or(0);
        let row_count = publication.bids.len().max(publication.asks.len());
        let mut rows = Vec::with_capacity(row_count);
        for index in 0..row_count {
            rows.push(DomRow {
                bid: publication.bids.get(index).map(|level| {
                    project_level(
                        *level,
                        selection.precision.price_scale(),
                        selection.precision.quantity_scale(),
                        maximum_quantity,
                    )
                }),
                ask: publication.asks.get(index).map(|level| {
                    project_level(
                        *level,
                        selection.precision.price_scale(),
                        selection.precision.quantity_scale(),
                        maximum_quantity,
                    )
                }),
            });
        }
        DomFrame {
            provider_id: selection.provider_id.clone(),
            instrument_id: selection.instrument_id.clone(),
            entitlement_id: selection.entitlement_id.clone(),
            session_generation: selection.session_generation,
            selection_generation: selection.selection_generation,
            revision: publication.revision,
            source_watermark: publication.source_watermark,
            state: publication.state,
            rows,
        }
    }
}

fn project_level(
    level: axiusflow_market_data::DepthLevel,
    price_scale: u8,
    quantity_scale: u8,
    maximum_quantity: i64,
) -> DomColumnLevel {
    DomColumnLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
        price_text: fixed_point_text(level.price, price_scale),
        quantity_text: fixed_point_text(level.quantity, quantity_scale),
        relative_size_bps: relative_size_bps(level.quantity, maximum_quantity),
    }
}

fn relative_size_bps(quantity: i64, maximum_quantity: i64) -> u16 {
    if maximum_quantity <= 0 {
        return 0;
    }
    let scaled = i128::from(quantity) * 10_000 / i128::from(maximum_quantity);
    u16::try_from(scaled.clamp(0, 10_000)).unwrap_or(10_000)
}

fn fixed_point_text(value: i64, scale: u8) -> String {
    if scale == 0 {
        return value.to_string();
    }
    let divisor = 10_i128.pow(u32::from(scale));
    let absolute = i128::from(value).abs();
    let whole = absolute / divisor;
    let fraction = absolute % divisor;
    let sign = if value < 0 { "-" } else { "" };
    format!(
        "{sign}{whole}.{fraction:0width$}",
        width = usize::from(scale)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_market_data::{
        AggressorSide, BookSide, DepthLevel, EventMetadata, MarketTrade, OrderBookState,
        QualifiedTimestamp,
    };

    fn selection(generation: u64, instrument_id: &str) -> DomSelection {
        DomSelection {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: "test".to_string(),
            session_generation: 7,
            selection_generation: generation,
            precision: InstrumentPrecision::try_new(2, 0).expect("valid fixture precision"),
        }
    }

    fn metadata(sequence: u64, instrument_id: &str) -> EventMetadata {
        EventMetadata {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: "test".to_string(),
            source_sequence: sequence,
            session_generation: 7,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(1),
                provider_unix_nanos: Some(2),
                received_unix_nanos: 3,
            },
        }
    }

    fn snapshot(sequence: u64, instrument_id: &str) -> MarketEvent {
        MarketEvent::DepthSnapshot(DepthSnapshot {
            metadata: metadata(sequence, instrument_id),
            bids: vec![
                DepthLevel {
                    price: 20_025,
                    quantity: 12,
                    order_count: Some(3),
                },
                DepthLevel {
                    price: 20_000,
                    quantity: 6,
                    order_count: Some(2),
                },
            ],
            asks: vec![DepthLevel {
                price: 20_050,
                quantity: 3,
                order_count: Some(1),
            }],
        })
    }

    fn delta(sequence: u64, side: BookSide, price: i64, quantity: i64) -> MarketEvent {
        MarketEvent::DepthDelta(DepthDelta {
            metadata: metadata(sequence, "mnq"),
            side,
            level: DepthLevel {
                price,
                quantity,
                order_count: Some(4),
            },
        })
    }

    #[test]
    fn snapshot_projects_bounded_display_rows_and_relative_sizes() {
        let mut dom = ReadOnlyDom::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
        dom.select(selection(1, "mnq"));
        let DomUpdateOutcome::Published(frame) = dom
            .apply_event(&snapshot(10, "mnq"))
            .expect("snapshot projects")
        else {
            panic!("snapshot must publish");
        };
        assert_eq!(frame.state, OrderBookState::Ready);
        assert_eq!(frame.selection_generation, 1);
        assert_eq!(frame.source_watermark, 10);
        assert_eq!(frame.rows.len(), 2);
        assert_eq!(
            frame.rows[0]
                .bid
                .as_ref()
                .map(|level| level.price_text.as_str()),
            Some("200.25")
        );
        assert_eq!(
            frame.rows[0]
                .bid
                .as_ref()
                .map(|level| level.quantity_text.as_str()),
            Some("12")
        );
        assert_eq!(
            frame.rows[0]
                .bid
                .as_ref()
                .map(|level| level.relative_size_bps),
            Some(10_000)
        );
        assert_eq!(
            frame.rows[0]
                .ask
                .as_ref()
                .map(|level| level.relative_size_bps),
            Some(2_500)
        );
        assert!(frame.rows[1].ask.is_none());
    }

    #[test]
    fn ordered_delta_publishes_and_gap_fails_closed_until_covering_snapshot() {
        let mut dom = ReadOnlyDom::new(NonZeroUsize::new(3).unwrap_or(NonZeroUsize::MIN));
        dom.select(selection(1, "mnq"));
        dom.apply_event(&snapshot(10, "mnq")).expect("snapshot");
        let DomUpdateOutcome::Published(updated) = dom
            .apply_event(&delta(11, BookSide::Ask, 20_050, 9))
            .expect("ordered delta")
        else {
            panic!("ordered delta must publish");
        };
        assert_eq!(updated.source_watermark, 11);
        assert_eq!(
            updated.rows[0].ask.as_ref().map(|level| level.quantity),
            Some(9)
        );

        assert!(matches!(
            dom.apply_event(&delta(13, BookSide::Bid, 20_025, 1)),
            Err(MarketDataValidationError::DepthGap {
                expected: 12,
                actual: 13
            })
        ));
        let recovering = dom.frame().expect("recovery frame");
        assert_eq!(
            recovering.state,
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        );
        assert!(recovering.rows.is_empty());
        assert_eq!(
            dom.apply_event(&snapshot(12, "mnq"))
                .expect("stale snapshot"),
            DomUpdateOutcome::Ignored
        );
        assert!(matches!(
            dom.apply_event(&snapshot(13, "mnq")),
            Ok(DomUpdateOutcome::Published(_))
        ));
    }

    #[test]
    fn selection_replacement_fences_late_depth_and_resets_book() {
        let mut dom = ReadOnlyDom::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        dom.select(selection(1, "mnq"));
        dom.apply_event(&snapshot(10, "mnq")).expect("snapshot");
        dom.select(selection(2, "es"));
        let reset = dom.frame().expect("selected frame");
        assert_eq!(reset.selection_generation, 2);
        assert_eq!(
            reset.state,
            OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot)
        );
        assert!(reset.rows.is_empty());
        assert_eq!(
            dom.apply_event(&delta(11, BookSide::Bid, 20_025, 4))
                .expect("late event ignored"),
            DomUpdateOutcome::Ignored
        );
        assert!(matches!(
            dom.apply_event(&snapshot(1, "es")),
            Ok(DomUpdateOutcome::Published(_))
        ));
    }

    #[test]
    fn non_depth_events_are_ignored_and_stale_retains_last_rows() {
        let mut dom = ReadOnlyDom::new(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        dom.select(selection(1, "mnq"));
        let trade = MarketEvent::Trade(MarketTrade {
            metadata: metadata(1, "mnq"),
            trade_id: "trade-1".to_string(),
            price: 20_025,
            quantity: 1,
            aggressor: AggressorSide::Unknown,
        });
        assert_eq!(
            dom.apply_event(&trade).expect("trade ignored"),
            DomUpdateOutcome::Ignored
        );
        dom.apply_event(&snapshot(10, "mnq")).expect("snapshot");
        let stale = dom.mark_stale().expect("stale frame");
        assert_eq!(stale.state, OrderBookState::Stale);
        assert_eq!(stale.rows.len(), 2);
    }

    #[test]
    fn fixed_point_formatting_handles_zeroes_and_signed_extremes() {
        assert_eq!(fixed_point_text(0, 2), "0.00");
        assert_eq!(fixed_point_text(-5, 3), "-0.005");
        assert_eq!(fixed_point_text(i64::MIN, 2), "-92233720368547758.08");
        assert_eq!(fixed_point_text(42, 0), "42");
    }
}
