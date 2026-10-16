use super::*;
use aeris_instruments::InstrumentDecimal;

#[test]
fn fixed_and_significant_figure_grids_reject_off_grid_prices() {
    let fixed = PriceGrid::Fixed(InstrumentDecimal::try_new(25, 2).expect("tick"));
    for (units, accepted) in [(50_010, false), (50_025, true)] {
        assert_eq!(
            validate_price_grid(fixed, FixedPoint::try_new(units, 2).expect("price")),
            if accepted {
                Ok(())
            } else {
                Err(RejectReason::OffGrid)
            },
        );
    }
    let significant = PriceGrid::SignificantFigures {
        figures: 5,
        max_decimals: 5,
    };
    for (units, scale, accepted) in [
        (800_005, 1, false),
        (80_001, 0, true),
        (8_000_050, 2, false),
        (1_234, 5, true),
    ] {
        assert_eq!(
            validate_price_grid(
                significant,
                FixedPoint::try_new(units, scale).expect("price")
            ),
            if accepted {
                Ok(())
            } else {
                Err(RejectReason::OffGrid)
            },
        );
    }
    assert_eq!(
        validate_price_grid(
            PriceGrid::Unknown,
            FixedPoint::try_new(800_005, 1).expect("price")
        ),
        Ok(())
    );
}
