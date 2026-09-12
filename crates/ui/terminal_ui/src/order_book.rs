use axiusflow_instruments::InstrumentPrecision;
use axiusflow_market_data::{
    OrderBookColumnLevel, OrderBookFrame, OrderBookPublication, OrderBookRow,
};

/// Identity and display precision for one selected depth stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderBookSelection {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub session_generation: u64,
    pub selection_generation: u64,
    pub precision: InstrumentPrecision,
    /// Authoritative minimum fixed-point trading increment for presentation.
    /// Canonical depth remains real-only; this only permits blank price ticks.
    pub price_increment: Option<i64>,
}

/// Projects the one authoritative runtime-owned order book into display rows.
///
/// This function is intentionally stateless. Sequence handling, snapshot/delta
/// validation, recovery, and depth ownership all live in `axiusflow_market_data::OrderBook`.
/// The UI never maintains another candidate book.
#[must_use]
pub fn project_order_book(
    selection: &OrderBookSelection,
    publication: &OrderBookPublication,
) -> Option<OrderBookFrame> {
    if !publication.provider_id.is_empty()
        && (selection.provider_id != publication.provider_id
            || selection.instrument_id != publication.instrument_id
            || selection.entitlement_id != publication.entitlement_id
            || selection.session_generation != publication.session_generation)
    {
        return None;
    }
    let maximum_quantity = publication
        .bids
        .iter()
        .chain(&publication.asks)
        .chain(publication.best_bid.iter())
        .chain(publication.best_ask.iter())
        .map(|level| level.quantity)
        .max()
        .unwrap_or(0);
    let row_count = publication.bids.len().max(publication.asks.len());
    // Complete canonical depth and the provider's independent BBO stream are
    // not one atomic image. Prefer each real depth edge when it exists so a
    // later-arriving but older quote cannot put the spread behind its own book
    // and force the continuous ladder into its real-level fallback. The BBO
    // remains the only valid fallback while that depth side is unavailable.
    let best_bid = publication.bids.first().copied().or(publication.best_bid);
    let best_ask = publication.asks.first().copied().or(publication.best_ask);
    let mut rows = Vec::with_capacity(row_count);
    for index in 0..row_count {
        rows.push(OrderBookRow {
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
    Some(OrderBookFrame {
        provider_id: selection.provider_id.clone(),
        instrument_id: selection.instrument_id.clone(),
        entitlement_id: selection.entitlement_id.clone(),
        session_generation: selection.session_generation,
        selection_generation: selection.selection_generation,
        revision: publication.revision,
        source_watermark: publication.source_watermark,
        bbo_source_watermark: publication.bbo_source_watermark,
        state: publication.state,
        price_scale: selection.precision.price_scale(),
        quantity_scale: selection.precision.quantity_scale(),
        price_increment: selection.price_increment.filter(|increment| *increment > 0),
        best_bid: best_bid.map(|level| {
            project_level(
                level,
                selection.precision.price_scale(),
                selection.precision.quantity_scale(),
                maximum_quantity,
            )
        }),
        best_ask: best_ask.map(|level| {
            project_level(
                level,
                selection.precision.price_scale(),
                selection.precision.quantity_scale(),
                maximum_quantity,
            )
        }),
        traded_volumes: publication.traded_volumes.clone(),
        trade_source_watermark: publication.trade_source_watermark,
        rows,
    })
}

fn project_level(
    level: axiusflow_market_data::DepthLevel,
    price_scale: u8,
    quantity_scale: u8,
    maximum_quantity: i64,
) -> OrderBookColumnLevel {
    OrderBookColumnLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
        price_text: grouped_fixed_point_text(level.price, price_scale),
        quantity_text: compact_quantity_text(level.quantity, quantity_scale),
        traded_volume: 0,
        traded_volume_text: String::new(),
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

fn compact_fixed_point_text(value: i64, scale: u8) -> String {
    let mut text = fixed_point_text(value, scale);
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

pub(crate) fn compact_quantity_text(value: i64, scale: u8) -> String {
    const DISPLAY_SCALE: u8 = 4;
    if scale <= DISPLAY_SCALE {
        return compact_fixed_point_text(value, scale);
    }
    let divisor = 10_i128.pow(u32::from(scale - DISPLAY_SCALE));
    let rounded = (i128::from(value).abs() + divisor / 2) / divisor;
    if value != 0 && rounded == 0 {
        return "<0.0001".into();
    }
    let signed = if value < 0 { -rounded } else { rounded };
    i64::try_from(signed).map_or_else(
        |_| compact_fixed_point_text(value, scale),
        |value| compact_fixed_point_text(value, DISPLAY_SCALE),
    )
}

pub(crate) fn grouped_fixed_point_text(value: i64, scale: u8) -> String {
    let mut significant_places = scale;
    let mut significant = value;
    while significant_places > 0 && significant % 10 == 0 {
        significant /= 10;
        significant_places -= 1;
    }
    let display_scale = significant_places.max(scale.min(2));
    let removable_places = scale.saturating_sub(display_scale);
    let reduced = value / 10_i64.pow(u32::from(removable_places));
    let fixed = fixed_point_text(reduced, display_scale);
    let (whole, fraction) = fixed.split_once('.').unwrap_or((fixed.as_str(), ""));
    let (sign, digits) = whole
        .strip_prefix('-')
        .map_or(("", whole), |digits| ("-", digits));
    let mut grouped = String::with_capacity(fixed.len() + digits.len() / 3);
    grouped.push_str(sign);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if !fraction.is_empty() {
        grouped.push('.');
        grouped.push_str(fraction);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_market_data::{DepthLevel, OrderBookState};
    use std::collections::BTreeMap;

    fn selection(instrument_id: &str, selection_generation: u64) -> OrderBookSelection {
        OrderBookSelection {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: "test".to_string(),
            session_generation: 7,
            selection_generation,
            precision: InstrumentPrecision::try_new(2, 0).expect("valid fixture precision"),
            price_increment: Some(25),
        }
    }

    fn publication(instrument_id: &str) -> OrderBookPublication {
        OrderBookPublication {
            provider_id: "rithmic".to_string(),
            instrument_id: instrument_id.to_string(),
            entitlement_id: "test".to_string(),
            session_generation: 7,
            revision: 3,
            source_watermark: 10,
            best_bid: Some(DepthLevel {
                price: 20_025,
                quantity: 12,
                order_count: Some(3),
            }),
            best_ask: Some(DepthLevel {
                price: 20_050,
                quantity: 3,
                order_count: Some(1),
            }),
            bbo_source_watermark: 10,
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
            traded_volumes: BTreeMap::from([(
                20_025,
                axiusflow_market_data::AggressorTradeVolumes { buy: 4, sell: 2 },
            )]),
            trade_source_watermark: 11,
            state: OrderBookState::Ready,
        }
    }

    #[test]
    fn authoritative_publication_projects_display_rows() {
        let frame = project_order_book(&selection("mnq", 1), &publication("mnq"))
            .expect("matching publication projects");
        assert_eq!(frame.state, OrderBookState::Ready);
        assert_eq!(frame.selection_generation, 1);
        assert_eq!(frame.price_scale, 2);
        assert_eq!(frame.price_increment, Some(25));
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
        assert_eq!(
            frame.traded_volumes.get(&20_025).copied(),
            Some(axiusflow_market_data::AggressorTradeVolumes { buy: 4, sell: 2 })
        );
        assert_eq!(frame.trade_source_watermark, 11);
        assert!(frame.rows[1].ask.is_none());
    }

    #[test]
    fn canonical_depth_edges_override_a_divergent_independent_bbo() {
        let mut publication = publication("mnq");
        publication.best_bid = Some(DepthLevel {
            price: 19_960,
            quantity: 8,
            order_count: Some(1),
        });
        publication.best_ask = Some(DepthLevel {
            price: 19_970,
            quantity: 9,
            order_count: Some(1),
        });
        publication.bbo_source_watermark = publication.source_watermark.saturating_add(1);

        let frame = project_order_book(&selection("mnq", 1), &publication)
            .expect("matching publication projects");

        assert_eq!(
            frame.best_bid.as_ref().map(|level| level.price),
            Some(20_025)
        );
        assert_eq!(
            frame.best_ask.as_ref().map(|level| level.price),
            Some(20_050)
        );
        assert_eq!(
            frame.rows[0].bid.as_ref().map(|level| level.price),
            frame.best_bid.as_ref().map(|level| level.price)
        );
        assert_eq!(
            frame.rows[0].ask.as_ref().map(|level| level.price),
            frame.best_ask.as_ref().map(|level| level.price)
        );
    }

    #[test]
    fn independent_bbo_remains_available_without_canonical_depth() {
        let mut publication = publication("mnq");
        publication.bids.clear();
        publication.asks.clear();

        let frame = project_order_book(&selection("mnq", 1), &publication)
            .expect("matching publication projects");

        assert_eq!(
            frame.best_bid.as_ref().map(|level| level.price),
            Some(20_025)
        );
        assert_eq!(
            frame.best_ask.as_ref().map(|level| level.price),
            Some(20_050)
        );
        assert!(frame.rows.is_empty());
    }

    #[test]
    fn stale_selection_cannot_project_another_instrument() {
        assert!(project_order_book(&selection("es", 2), &publication("mnq")).is_none());
    }

    #[test]
    fn fixed_point_formatting_handles_zeroes_and_signed_extremes() {
        assert_eq!(fixed_point_text(0, 2), "0.00");
        assert_eq!(fixed_point_text(-5, 3), "-0.005");
        assert_eq!(fixed_point_text(i64::MIN, 2), "-92233720368547758.08");
        assert_eq!(fixed_point_text(42, 0), "42");
        assert_eq!(grouped_fixed_point_text(7_796_038, 2), "77,960.38");
        assert_eq!(grouped_fixed_point_text(-123_456, 3), "-123.456");
        assert_eq!(grouped_fixed_point_text(7_978_500_000_000, 8), "79,785.00");
        assert_eq!(grouped_fixed_point_text(12_345, 8), "0.00012345");
        assert_eq!(compact_fixed_point_text(125_000_000, 8), "1.25");
        assert_eq!(compact_fixed_point_text(10_000, 8), "0.0001");
        assert_eq!(compact_fixed_point_text(0, 8), "0");
        assert_eq!(compact_quantity_text(9_200, 8), "0.0001");
        assert_eq!(compact_quantity_text(1, 8), "<0.0001");
        assert_eq!(compact_quantity_text(100_000, 8), "0.001");
        assert_eq!(compact_quantity_text(12_345_678, 8), "0.1235");
    }
}
