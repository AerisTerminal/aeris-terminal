use super::{MarketDecodeError, PriceScale, check_account};
use crate::{
    ProtoMessage,
    codec::{self, require_nested_fields},
    generated::{ProtoOaAssetListRes, ProtoOaSymbolByIdRes, ProtoOaSymbolsListRes},
};

/// Observed demo catalogs hold under a thousand symbols per account.
pub const MAXIMUM_CATALOG_SYMBOLS: usize = 65_536;
const MAXIMUM_SYMBOL_TEXT_BYTES: usize = 256;

/// One searchable catalog entry (`ProtoOALightSymbol`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightSymbol {
    pub symbol_id: u64,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    /// Prices, and so profit and loss, are in this asset.
    pub quote_asset_id: Option<u64>,
}

/// One account asset (`ProtoOAAsset`): a currency or other priced unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Asset {
    pub asset_id: u64,
    pub name: String,
}

/// Full trading specification (`ProtoOASymbol`). Volumes are in cents
/// (0.01 of the base unit): an observed EURUSD lot of 100,000 EUR is 10,000,000.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolSpec {
    pub symbol_id: u64,
    pub price_scale: PriceScale,
    pub pip_position: u8,
    pub lot_size: i64,
    pub min_volume: i64,
    pub step_volume: i64,
    pub max_volume: Option<i64>,
}

impl SymbolSpec {
    /// The minimum price change in canonical units at `price_scale`. cTrader
    /// moves prices by one point of `digits` (cAlgo `TickSize = 10^-digits`),
    /// but the wire carries only five decimals, so symbols with more digits
    /// step by `10^(digits - 5)` units. `pipPosition` only names the pip.
    #[must_use]
    pub fn tick_units(&self) -> i64 {
        10_i64.pow(u32::from(
            self.price_scale
                .digits()
                .saturating_sub(super::WIRE_PRICE_DIGITS),
        ))
    }

    /// One pip in canonical units at `price_scale`.
    #[must_use]
    pub fn pip_units(&self) -> i64 {
        10_i64.pow(u32::from(self.price_scale.digits() - self.pip_position))
    }
}

fn positive_id(symbol_id: i64) -> Result<u64, MarketDecodeError> {
    u64::try_from(symbol_id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(MarketDecodeError::InvalidField("symbolId"))
}

fn bounded_text(value: String, field: &'static str) -> Result<String, MarketDecodeError> {
    if value.trim().is_empty() || value.len() > MAXIMUM_SYMBOL_TEXT_BYTES {
        return Err(MarketDecodeError::InvalidField(field));
    }
    Ok(value)
}

/// Decode a `ProtoOASymbolsListRes` (2115) for one account.
///
/// # Errors
/// Rejects another account, missing ids or names, and oversized catalogs.
pub fn decode_symbol_list(
    frame: &ProtoMessage,
    ctid: u64,
) -> Result<Vec<LightSymbol>, MarketDecodeError> {
    let list: ProtoOaSymbolsListRes =
        codec::decode_typed(frame, 2115, &[(2, "ctidTraderAccountId")], |_| Ok(()))?;
    require_nested_fields(
        frame.payload.as_deref().unwrap_or_default(),
        3,
        &[(1, "symbolId")],
    )?;
    check_account(ctid, list.ctid_trader_account_id)?;
    if list.symbol.len() > MAXIMUM_CATALOG_SYMBOLS {
        return Err(MarketDecodeError::LimitExceeded("symbol catalog"));
    }
    list.symbol
        .into_iter()
        .map(|symbol| {
            Ok(LightSymbol {
                symbol_id: positive_id(symbol.symbol_id)?,
                name: bounded_text(
                    symbol
                        .symbol_name
                        .ok_or(MarketDecodeError::MissingField("symbolName"))?,
                    "symbolName",
                )?,
                description: symbol
                    .description
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| bounded_text(text, "description"))
                    .transpose()?,
                enabled: symbol.enabled.unwrap_or(true),
                quote_asset_id: symbol.quote_asset_id.map(positive_asset_id).transpose()?,
            })
        })
        .collect()
}

fn positive_asset_id(asset_id: i64) -> Result<u64, MarketDecodeError> {
    u64::try_from(asset_id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(MarketDecodeError::InvalidField("assetId"))
}

/// Decode a `ProtoOAAssetListRes` (2113) for one account.
///
/// # Errors
/// Rejects another account, missing ids or names, and oversized lists.
pub fn decode_asset_list(frame: &ProtoMessage, ctid: u64) -> Result<Vec<Asset>, MarketDecodeError> {
    let list: ProtoOaAssetListRes =
        codec::decode_typed(frame, 2113, &[(2, "ctidTraderAccountId")], |_| Ok(()))?;
    require_nested_fields(
        frame.payload.as_deref().unwrap_or_default(),
        3,
        &[(1, "assetId"), (2, "name")],
    )?;
    check_account(ctid, list.ctid_trader_account_id)?;
    if list.asset.len() > MAXIMUM_CATALOG_SYMBOLS {
        return Err(MarketDecodeError::LimitExceeded("asset list"));
    }
    list.asset
        .into_iter()
        .map(|asset| {
            Ok(Asset {
                asset_id: positive_asset_id(asset.asset_id)?,
                name: bounded_text(asset.name, "name")?,
            })
        })
        .collect()
}

/// Decode a `ProtoOASymbolByIdRes` (2117). The price scale is the symbol's
/// `digits`; lot, minimum and step volume are required for order sizing.
///
/// # Errors
/// Rejects another account, missing required fields and inconsistent volumes.
pub fn decode_symbol_by_id(
    frame: &ProtoMessage,
    ctid: u64,
) -> Result<Vec<SymbolSpec>, MarketDecodeError> {
    let response: ProtoOaSymbolByIdRes =
        codec::decode_typed(frame, 2117, &[(2, "ctidTraderAccountId")], |_| Ok(()))?;
    require_nested_fields(
        frame.payload.as_deref().unwrap_or_default(),
        3,
        &[(1, "symbolId"), (2, "digits"), (3, "pipPosition")],
    )?;
    check_account(ctid, response.ctid_trader_account_id)?;
    if response.symbol.len() > MAXIMUM_CATALOG_SYMBOLS {
        return Err(MarketDecodeError::LimitExceeded("symbol details"));
    }
    response
        .symbol
        .into_iter()
        .map(|symbol| {
            let price_scale = PriceScale::new(symbol.digits)?;
            let pip_position = u8::try_from(symbol.pip_position)
                .ok()
                .filter(|pip| *pip <= price_scale.digits())
                .ok_or(MarketDecodeError::InvalidField("pipPosition"))?;
            let positive = |value: Option<i64>, field| {
                value
                    .ok_or(MarketDecodeError::MissingField(field))
                    .and_then(|value| {
                        if value > 0 {
                            Ok(value)
                        } else {
                            Err(MarketDecodeError::InvalidField(field))
                        }
                    })
            };
            let lot_size = positive(symbol.lot_size, "lotSize")?;
            let min_volume = positive(symbol.min_volume, "minVolume")?;
            let step_volume = positive(symbol.step_volume, "stepVolume")?;
            let max_volume = symbol
                .max_volume
                .map(|max| positive(Some(max), "maxVolume"))
                .transpose()?;
            if max_volume.is_some_and(|max| max < min_volume) {
                return Err(MarketDecodeError::InvalidField("maxVolume"));
            }
            Ok(SymbolSpec {
                symbol_id: positive_id(symbol.symbol_id)?,
                price_scale,
                pip_position,
                lot_size,
                min_volume,
                step_volume,
                max_volume,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generated::{ProtoOaLightSymbol, ProtoOaSymbol},
        market::fixtures::{
            CTID, CTID_WIRE, EURUSD, USDJPY, XAUUSD, bytes_frame, frame, strip, strip_nested,
        },
    };
    use prost::Message;

    fn light(symbol_id: i64, name: &str, description: &str) -> ProtoOaLightSymbol {
        ProtoOaLightSymbol {
            symbol_id,
            symbol_name: Some(name.into()),
            enabled: Some(true),
            base_asset_id: Some(4),
            quote_asset_id: Some(11),
            symbol_category_id: Some(1),
            description: Some(description.into()),
            sorting_number: Some(0.0),
        }
    }

    fn list() -> ProtoOaSymbolsListRes {
        ProtoOaSymbolsListRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            symbol: vec![
                light(EURUSD, "EURUSD", "Euro vs US Dollar"),
                light(USDJPY, "USDJPY", "US Dollar vs Japanese Yen"),
            ],
            archived_symbol: Vec::new(),
        }
    }

    fn spec(symbol_id: i64, digits: i32, pip: i32, lot: i64, min: i64) -> ProtoOaSymbol {
        ProtoOaSymbol {
            symbol_id,
            digits,
            pip_position: pip,
            lot_size: Some(lot),
            min_volume: Some(min),
            step_volume: Some(min),
            max_volume: Some(min * 10_000),
            measurement_units: Some("EUR".into()),
            schedule_time_zone: Some("America/New_York".into()),
            ..ProtoOaSymbol::default()
        }
    }

    fn by_id() -> ProtoOaSymbolByIdRes {
        ProtoOaSymbolByIdRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            symbol: vec![
                spec(EURUSD, 5, 4, 10_000_000, 100_000),
                spec(USDJPY, 3, 2, 10_000_000, 100_000),
                spec(XAUUSD, 2, 2, 10_000, 100),
            ],
            archived_symbol: Vec::new(),
        }
    }

    #[test]
    fn symbol_list_decodes_names_and_ids() {
        let symbols = decode_symbol_list(&frame(2115, &list()), CTID).expect("catalog");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].symbol_id, 1);
        assert_eq!(symbols[0].name, "EURUSD");
        assert_eq!(symbols[0].description.as_deref(), Some("Euro vs US Dollar"));
        assert!(symbols[0].enabled);
        assert_eq!(symbols[0].quote_asset_id, Some(11));
    }

    #[test]
    fn asset_list_decodes_names_and_rejects_bad_entries() {
        use crate::generated::ProtoOaAsset;
        let asset = |asset_id, name: &str| ProtoOaAsset {
            asset_id,
            name: name.into(),
            display_name: None,
            digits: Some(2),
        };
        let list = ProtoOaAssetListRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            asset: vec![asset(4, "EUR"), asset(11, "USD")],
        };
        let assets = decode_asset_list(&frame(2113, &list), CTID).expect("assets");
        assert_eq!(
            assets,
            [
                Asset {
                    asset_id: 4,
                    name: "EUR".into()
                },
                Asset {
                    asset_id: 11,
                    name: "USD".into()
                }
            ]
        );
        assert!(matches!(
            decode_asset_list(&frame(2113, &list), CTID + 1),
            Err(MarketDecodeError::AccountMismatch)
        ));
        let payload = list.encode_to_vec();
        for inner in [1, 2] {
            assert!(
                decode_asset_list(&bytes_frame(2113, strip_nested(&payload, 3, inner)), CTID)
                    .is_err(),
                "field {inner} must be required"
            );
        }
        let mut bad = list;
        bad.asset[0].name = " ".into();
        assert!(decode_asset_list(&frame(2113, &bad), CTID).is_err());
    }

    #[test]
    fn symbol_list_rejects_missing_fields_and_other_accounts() {
        let payload = list().encode_to_vec();
        assert!(decode_symbol_list(&bytes_frame(2115, strip(&payload, 2)), CTID).is_err());
        assert!(
            decode_symbol_list(&bytes_frame(2115, strip_nested(&payload, 3, 1)), CTID).is_err()
        );
        assert!(matches!(
            decode_symbol_list(&bytes_frame(2115, strip_nested(&payload, 3, 2)), CTID),
            Err(MarketDecodeError::MissingField("symbolName"))
        ));
        assert!(matches!(
            decode_symbol_list(&frame(2115, &list()), CTID + 1),
            Err(MarketDecodeError::AccountMismatch)
        ));
        let mut bad = list();
        bad.symbol[0].symbol_id = 0;
        assert!(decode_symbol_list(&frame(2115, &bad), CTID).is_err());
    }

    #[test]
    fn symbol_details_carry_digits_pips_and_volume_rules() {
        let specs = decode_symbol_by_id(&frame(2117, &by_id()), CTID).expect("details");
        let eurusd = &specs[0];
        assert_eq!(eurusd.symbol_id, 1);
        assert_eq!(eurusd.price_scale.digits(), 5);
        assert_eq!(eurusd.pip_position, 4);
        assert_eq!(eurusd.lot_size, 10_000_000);
        assert_eq!(eurusd.min_volume, 100_000);
        assert_eq!(eurusd.step_volume, 100_000);
        assert_eq!(eurusd.max_volume, Some(1_000_000_000));
        assert_eq!(specs[1].price_scale, PriceScale::new(3).expect("3"));
        assert_eq!(specs[1].pip_position, 2);
        assert_eq!(specs[2].price_scale.digits(), 2);
        assert_eq!(specs[2].lot_size, 10_000);
        assert_eq!(specs[2].min_volume, 100);
    }

    #[test]
    fn symbol_ticks_follow_digits_and_the_five_decimal_wire() {
        let specs = decode_symbol_by_id(&frame(2117, &by_id()), CTID).expect("details");
        // EURUSD 1.08543: one tick is 0.00001, one pip ten ticks.
        assert_eq!((specs[0].tick_units(), specs[0].pip_units()), (1, 10));
        // USDJPY 158.023: one tick is 0.001, one pip ten ticks.
        assert_eq!((specs[1].tick_units(), specs[1].pip_units()), (1, 10));
        // XAUUSD 2345.67: the pip equals the tick.
        assert_eq!((specs[2].tick_units(), specs[2].pip_units()), (1, 1));
        let mut fine = by_id();
        fine.symbol[0].digits = 7;
        fine.symbol[0].pip_position = 4;
        let fine = decode_symbol_by_id(&frame(2117, &fine), CTID).expect("seven digits");
        // The wire cannot carry the sixth and seventh decimals.
        assert_eq!((fine[0].tick_units(), fine[0].pip_units()), (100, 1_000));
    }

    #[test]
    fn symbol_details_reject_missing_required_fields() {
        let payload = by_id().encode_to_vec();
        for inner in [1, 2, 3, 30, 10, 11] {
            assert!(
                decode_symbol_by_id(&bytes_frame(2117, strip_nested(&payload, 3, inner)), CTID)
                    .is_err(),
                "field {inner} must be required"
            );
        }
        assert!(decode_symbol_by_id(&bytes_frame(2117, strip(&payload, 2)), CTID).is_err());
        let mut pip = by_id();
        pip.symbol[0].pip_position = 6;
        assert!(decode_symbol_by_id(&frame(2117, &pip), CTID).is_err());
        let mut max = by_id();
        max.symbol[0].max_volume = Some(1);
        assert!(decode_symbol_by_id(&frame(2117, &max), CTID).is_err());
    }
}
