use crate::ProtocolError;

#[cfg(rithmic_kit)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;
#[cfg(rithmic_kit)]
const MAX_FIELD_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolSearchResult {
    pub symbol: String,
    pub exchange: String,
    pub name: Option<String>,
    pub product_code: Option<String>,
    pub instrument_type: Option<String>,
    pub expiration_date: Option<String>,
}

/// One product the provider lists for an exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductCodeResult {
    pub product_code: String,
    pub exchange: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentReference {
    pub symbol: String,
    pub exchange: String,
    pub exchange_symbol: Option<String>,
    pub name: Option<String>,
    pub product_code: Option<String>,
    pub instrument_type: Option<String>,
    pub underlying_symbol: Option<String>,
    pub expiration_date: Option<String>,
    pub currency: Option<String>,
    pub price_display_format: Option<String>,
    pub minimum_price_change: Option<f64>,
    pub single_point_value: Option<f64>,
    pub price_precision: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DecodedCatalogMessage {
    SearchResult(SymbolSearchResult),
    SearchComplete { accepted: bool },
    ProductCode(ProductCodeResult),
    ProductCodesComplete { accepted: bool },
    InstrumentReference(Option<InstrumentReference>),
}

#[cfg(rithmic_kit)]
pub(crate) fn decode(frame: &[u8]) -> Result<DecodedCatalogMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    bound_frame(frame)?;
    match rti::MessageType::decode(frame)
        .map_err(|_| ProtocolError::Decode)?
        .template_id
    {
        15 => decode_reference(frame),
        110 => decode_search(frame),
        112 => decode_product_codes(frame),
        template => Err(ProtocolError::UnsupportedTemplate(template)),
    }
}

#[cfg(not(rithmic_kit))]
pub(crate) fn decode(_frame: &[u8]) -> Result<DecodedCatalogMessage, ProtocolError> {
    Err(ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
fn decode_search(frame: &[u8]) -> Result<DecodedCatalogMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::ResponseSearchSymbols::decode(frame).map_err(|_| ProtocolError::Decode)?;
    validate_codes(
        &message.user_msg,
        &message.rq_handler_rp_code,
        &message.rp_code,
    )?;
    match (
        message.rq_handler_rp_code.is_empty(),
        message.rp_code.is_empty(),
    ) {
        (false, true) => {
            if !accepted(&message.rq_handler_rp_code) {
                return Err(ProtocolError::RejectedDataFrame);
            }
            Ok(DecodedCatalogMessage::SearchResult(SymbolSearchResult {
                symbol: required_string("search.symbol", message.symbol)?,
                exchange: required_string("search.exchange", message.exchange)?,
                name: optional_string("search.symbol_name", message.symbol_name)?,
                product_code: optional_string("search.product_code", message.product_code)?,
                instrument_type: optional_string(
                    "search.instrument_type",
                    message.instrument_type,
                )?,
                expiration_date: optional_string(
                    "search.expiration_date",
                    message.expiration_date,
                )?,
            }))
        }
        (true, false) => Ok(DecodedCatalogMessage::SearchComplete {
            accepted: accepted(&message.rp_code),
        }),
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

/// Product-code responses share the search response shape: each product
/// carries only `rq_handler_rp_code` and the final frame only `rp_code`.
#[cfg(rithmic_kit)]
fn decode_product_codes(frame: &[u8]) -> Result<DecodedCatalogMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::ResponseProductCodes::decode(frame).map_err(|_| ProtocolError::Decode)?;
    validate_codes(
        &message.user_msg,
        &message.rq_handler_rp_code,
        &message.rp_code,
    )?;
    match (
        message.rq_handler_rp_code.is_empty(),
        message.rp_code.is_empty(),
    ) {
        (false, true) => {
            if !accepted(&message.rq_handler_rp_code) {
                return Err(ProtocolError::RejectedDataFrame);
            }
            Ok(DecodedCatalogMessage::ProductCode(ProductCodeResult {
                product_code: required_string("product.product_code", message.product_code)?,
                exchange: required_string("product.exchange", message.exchange)?,
                name: optional_string("product.symbol_name", message.symbol_name)?,
            }))
        }
        (true, false) => Ok(DecodedCatalogMessage::ProductCodesComplete {
            accepted: accepted(&message.rp_code),
        }),
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

#[cfg(rithmic_kit)]
fn decode_reference(frame: &[u8]) -> Result<DecodedCatalogMessage, ProtocolError> {
    use crate::generated::rti;
    use prost::Message;

    let message = rti::ResponseReferenceData::decode(frame).map_err(|_| ProtocolError::Decode)?;
    validate_codes(&message.user_msg, &[], &message.rp_code)?;
    if message.rp_code.is_empty() {
        return Err(ProtocolError::ResponseCodeShape);
    }
    if !accepted(&message.rp_code) {
        return Ok(DecodedCatalogMessage::InstrumentReference(None));
    }
    let minimum_price_change =
        finite_nonnegative("reference.min_qprice_change", message.min_qprice_change)?;
    let single_point_value =
        finite_nonnegative("reference.single_point_value", message.single_point_value)?;
    let price_precision = message
        .min_qprice_change_precision
        .map(|value| {
            u8::try_from(value).ok().filter(|value| *value <= 18).ok_or(
                ProtocolError::InvalidNumber("reference.min_qprice_change_precision"),
            )
        })
        .transpose()?;
    for (field, value) in [
        ("reference.strike_price", message.strike_price),
        ("reference.ftoq_price", message.ftoq_price),
        ("reference.qtof_price", message.qtof_price),
        ("reference.min_fprice_change", message.min_fprice_change),
    ] {
        finite_optional(field, value)?;
    }
    Ok(DecodedCatalogMessage::InstrumentReference(Some(
        InstrumentReference {
            symbol: required_string("reference.symbol", message.symbol)?,
            exchange: required_string("reference.exchange", message.exchange)?,
            exchange_symbol: optional_string("reference.exchange_symbol", message.exchange_symbol)?,
            name: optional_string("reference.symbol_name", message.symbol_name)?,
            product_code: optional_string("reference.product_code", message.product_code)?,
            instrument_type: optional_string("reference.instrument_type", message.instrument_type)?,
            underlying_symbol: optional_string(
                "reference.underlying_symbol",
                message.underlying_symbol,
            )?,
            expiration_date: optional_string("reference.expiration_date", message.expiration_date)?,
            currency: optional_string("reference.currency", message.currency)?,
            price_display_format: optional_string(
                "reference.price_display_format",
                message.price_display_format,
            )?,
            minimum_price_change,
            single_point_value,
            price_precision,
        },
    )))
}

#[cfg(rithmic_kit)]
fn validate_codes(
    user_messages: &[String],
    handler_codes: &[String],
    terminal_codes: &[String],
) -> Result<(), ProtocolError> {
    if user_messages.len() > 2 {
        return Err(ProtocolError::RepeatedFieldLimitExceeded {
            field: "user_msg",
            maximum: 2,
        });
    }
    for value in user_messages {
        validate_string("user_msg", value)?;
    }
    validate_code_field("rq_handler_rp_code", handler_codes)?;
    validate_code_field("rp_code", terminal_codes)
}

#[cfg(rithmic_kit)]
fn validate_code_field(field: &'static str, codes: &[String]) -> Result<(), ProtocolError> {
    match codes {
        [] => Ok(()),
        [code] if code == "0" => Ok(()),
        [code, detail] if code.parse::<u32>().is_ok_and(|value| value > 0) => {
            validate_string(field, code)?;
            validate_string(field, detail)
        }
        _ => Err(ProtocolError::ResponseCodeShape),
    }
}

#[cfg(rithmic_kit)]
fn accepted(codes: &[String]) -> bool {
    codes.len() == 1 && codes[0] == "0"
}

#[cfg(rithmic_kit)]
fn bound_frame(frame: &[u8]) -> Result<(), ProtocolError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            requested: frame.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(rithmic_kit)]
fn required_string(field: &'static str, value: Option<String>) -> Result<String, ProtocolError> {
    let value = value.ok_or(ProtocolError::MissingField(field))?;
    validate_string(field, &value)?;
    Ok(value)
}

/// Rithmic sends some optional descriptive fields as present but empty; that
/// is treated as absent rather than as a malformed frame.
#[cfg(rithmic_kit)]
fn optional_string(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<String>, ProtocolError> {
    let value = value.filter(|value| !value.is_empty());
    if let Some(value) = &value {
        validate_string(field, value)?;
    }
    Ok(value)
}

#[cfg(rithmic_kit)]
fn validate_string(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() {
        return Err(ProtocolError::EmptyField(field));
    }
    if value.len() > MAX_FIELD_BYTES {
        return Err(ProtocolError::FieldTooLong {
            field,
            maximum: MAX_FIELD_BYTES,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ProtocolError::ControlCharacter(field));
    }
    Ok(())
}

#[cfg(rithmic_kit)]
fn finite_optional(field: &'static str, value: Option<f64>) -> Result<(), ProtocolError> {
    if value.is_some_and(|value| !value.is_finite()) {
        return Err(ProtocolError::InvalidNumber(field));
    }
    Ok(())
}

#[cfg(rithmic_kit)]
fn finite_nonnegative(
    field: &'static str,
    value: Option<f64>,
) -> Result<Option<f64>, ProtocolError> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(ProtocolError::InvalidNumber(field));
    }
    Ok(value)
}

#[cfg(all(test, rithmic_kit))]
mod tests {
    use super::*;
    use crate::{RithmicProtocolCodec, generated::rti};
    use prost::Message;

    #[test]
    fn decodes_search_results_completion_and_reference_data() {
        let codec = RithmicProtocolCodec;
        let result = rti::ResponseSearchSymbols {
            template_id: 110,
            user_msg: Vec::new(),
            rq_handler_rp_code: vec!["0".to_string()],
            rp_code: Vec::new(),
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            symbol_name: Some("E-mini S&P 500".to_string()),
            product_code: Some("ES".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: Some("202706".to_string()),
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&result).expect("result decodes"),
            DecodedCatalogMessage::SearchResult(SymbolSearchResult { symbol, .. })
                if symbol == "ESM7"
        ));

        let complete = rti::ResponseSearchSymbols {
            template_id: 110,
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_catalog(&complete).expect("completion decodes"),
            DecodedCatalogMessage::SearchComplete { accepted: true }
        );

        let reference = rti::ResponseReferenceData {
            template_id: 15,
            rp_code: vec!["0".to_string()],
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            symbol_name: Some("E-mini S&P 500".to_string()),
            min_qprice_change: Some(0.25),
            single_point_value: Some(50.0),
            min_qprice_change_precision: Some(2),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&reference).expect("reference decodes"),
            DecodedCatalogMessage::InstrumentReference(Some(InstrumentReference {
                price_precision: Some(2),
                ..
            }))
        ));
    }

    #[test]
    fn empty_optional_catalog_fields_decode_as_absent() {
        let codec = RithmicProtocolCodec;
        let result = rti::ResponseSearchSymbols {
            template_id: 110,
            rq_handler_rp_code: vec!["0".to_string()],
            symbol: Some("MNQZ5".to_string()),
            exchange: Some("CME".to_string()),
            symbol_name: Some(String::new()),
            product_code: Some("MNQ".to_string()),
            instrument_type: Some(String::new()),
            expiration_date: Some(String::new()),
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_catalog(&result).expect("result decodes"),
            DecodedCatalogMessage::SearchResult(SymbolSearchResult {
                symbol: "MNQZ5".to_string(),
                exchange: "CME".to_string(),
                name: None,
                product_code: Some("MNQ".to_string()),
                instrument_type: None,
                expiration_date: None,
            })
        );

        let reference = rti::ResponseReferenceData {
            template_id: 15,
            rp_code: vec!["0".to_string()],
            symbol: Some("MNQZ5".to_string()),
            exchange: Some("CME".to_string()),
            exchange_symbol: Some(String::new()),
            currency: Some(String::new()),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&reference).expect("reference decodes"),
            DecodedCatalogMessage::InstrumentReference(Some(InstrumentReference {
                exchange_symbol: None,
                currency: None,
                ..
            }))
        ));

        let empty_symbol = rti::ResponseSearchSymbols {
            template_id: 110,
            rq_handler_rp_code: vec!["0".to_string()],
            symbol: Some(String::new()),
            exchange: Some("CME".to_string()),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&empty_symbol),
            Err(ProtocolError::EmptyField("search.symbol"))
        ));
    }

    #[test]
    fn decodes_product_codes_and_their_completion() {
        let codec = RithmicProtocolCodec;
        let product = rti::ResponseProductCodes {
            template_id: 112,
            rq_handler_rp_code: vec!["0".to_string()],
            exchange: Some("CME".to_string()),
            symbol_name: Some("E-Mini NASDAQ 100".to_string()),
            product_code: Some("NQ".to_string()),
            timezone_time_of_interest: Some("America/Chicago".to_string()),
            begin_time_of_interest_msm: Some(30_600),
            end_time_of_interest_msm: Some(54_000),
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_catalog(&product).expect("product decodes"),
            DecodedCatalogMessage::ProductCode(ProductCodeResult {
                product_code: "NQ".to_string(),
                exchange: "CME".to_string(),
                name: Some("E-Mini NASDAQ 100".to_string()),
            })
        );

        let unnamed = rti::ResponseProductCodes {
            template_id: 112,
            rq_handler_rp_code: vec!["0".to_string()],
            exchange: Some("CME".to_string()),
            symbol_name: Some(String::new()),
            product_code: Some("MNQ".to_string()),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec
                .decode_catalog(&unnamed)
                .expect("unnamed product decodes"),
            DecodedCatalogMessage::ProductCode(ProductCodeResult { name: None, .. })
        ));

        let complete = rti::ResponseProductCodes {
            template_id: 112,
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_catalog(&complete).expect("completion decodes"),
            DecodedCatalogMessage::ProductCodesComplete { accepted: true }
        );

        let rejected = rti::ResponseProductCodes {
            template_id: 112,
            rp_code: vec!["7".to_string(), "no products".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_catalog(&rejected).expect("rejection decodes"),
            DecodedCatalogMessage::ProductCodesComplete { accepted: false }
        );
    }

    #[test]
    fn product_codes_require_code_exchange_and_one_response_code_lane() {
        let codec = RithmicProtocolCodec;
        let missing_exchange = rti::ResponseProductCodes {
            template_id: 112,
            rq_handler_rp_code: vec!["0".to_string()],
            product_code: Some("NQ".to_string()),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&missing_exchange),
            Err(ProtocolError::MissingField("product.exchange"))
        ));

        let empty_code = rti::ResponseProductCodes {
            template_id: 112,
            rq_handler_rp_code: vec!["0".to_string()],
            exchange: Some("CME".to_string()),
            product_code: Some(String::new()),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&empty_code),
            Err(ProtocolError::EmptyField("product.product_code"))
        ));

        let mixed = rti::ResponseProductCodes {
            template_id: 112,
            rq_handler_rp_code: vec!["0".to_string()],
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&mixed),
            Err(ProtocolError::ResponseCodeShape)
        ));
    }

    #[test]
    fn mixed_search_codes_and_nonfinite_metadata_fail_closed() {
        let codec = RithmicProtocolCodec;
        let mixed = rti::ResponseSearchSymbols {
            template_id: 110,
            rq_handler_rp_code: vec!["0".to_string()],
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&mixed),
            Err(ProtocolError::ResponseCodeShape)
        ));

        let malformed_rejection = rti::ResponseSearchSymbols {
            template_id: 110,
            rp_code: vec!["1".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&malformed_rejection),
            Err(ProtocolError::ResponseCodeShape)
        ));

        let invalid = rti::ResponseReferenceData {
            template_id: 15,
            rp_code: vec!["0".to_string()],
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            min_qprice_change: Some(f64::NAN),
            ..Default::default()
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_catalog(&invalid),
            Err(ProtocolError::InvalidNumber("reference.min_qprice_change"))
        ));
    }
}
