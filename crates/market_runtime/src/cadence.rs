//! The single provider-aware mapping between chart intervals and runtime cadences.

use std::fmt;

use aeris_contracts::{
    InstallProviderInstrument, ProviderDailyBarCadence, ProviderPresentationDescriptor,
    SeriesCadence, SeriesKey,
};
use aeris_market_data::{BarPeriod, BarSeriesKey, ChartInterval};

/// A chart interval cannot be represented by the selected provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CadenceError {
    provider: String,
    interval: String,
}

impl fmt::Display for CadenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} chart interval {} is unsupported",
            self.provider, self.interval
        )
    }
}

impl std::error::Error for CadenceError {}

/// Resolves a presentation interval to the canonical runtime cadence.
///
/// `ProviderDailyBarCadence::FixedDay` means UTC-aligned fixed-duration daily bars.
/// Session providers retain exchange-session day boundaries.
///
/// # Errors
///
/// Returns [`CadenceError`] when the provider does not offer the interval or
/// the interval cannot be represented as a valid runtime period.
pub fn resolve_cadence(
    interval: ChartInterval,
    presentation: &ProviderPresentationDescriptor,
) -> Result<BarPeriod, CadenceError> {
    if !presentation
        .chart_interval_labels
        .contains(&interval.label())
    {
        return Err(unsupported(presentation, interval));
    }
    let period = match interval {
        ChartInterval::Tick100 => BarPeriod::tick(100),
        ChartInterval::Second1 => BarPeriod::time(1),
        ChartInterval::Second5 => BarPeriod::time(5),
        ChartInterval::Second10 => BarPeriod::time(10),
        ChartInterval::Second15 => BarPeriod::time(15),
        ChartInterval::Second30 => BarPeriod::time(30),
        ChartInterval::Minute1 => BarPeriod::time(60),
        ChartInterval::Minute3 => BarPeriod::time(180),
        ChartInterval::Minute5 => BarPeriod::time(300),
        ChartInterval::Minute15 => BarPeriod::time(900),
        ChartInterval::Minute30 => BarPeriod::time(1_800),
        ChartInterval::Hour1 => BarPeriod::time(3_600),
        ChartInterval::Hour2 => BarPeriod::time(7_200),
        ChartInterval::Hour4 => BarPeriod::time(14_400),
        ChartInterval::Hour8 => BarPeriod::time(28_800),
        ChartInterval::Hour12 => BarPeriod::time(43_200),
        ChartInterval::Day1 => match presentation.daily_bars {
            ProviderDailyBarCadence::FixedDay => BarPeriod::time(86_400),
            ProviderDailyBarCadence::TradingSession => BarPeriod::session(1),
        },
        ChartInterval::Day3 => match presentation.daily_bars {
            ProviderDailyBarCadence::FixedDay => BarPeriod::time(259_200),
            ProviderDailyBarCadence::TradingSession => BarPeriod::session(3),
        },
        ChartInterval::Week1 => BarPeriod::week(1),
        ChartInterval::Month1 => BarPeriod::month(1),
    };
    period.map_err(|_| unsupported(presentation, interval))
}

/// Builds the canonical runtime series identity for a selected chart interval.
///
/// # Errors
///
/// Returns [`CadenceError`] when [`resolve_cadence`] cannot map the interval.
pub fn resolve_series_key(
    instrument: &InstallProviderInstrument,
    interval: ChartInterval,
    presentation: &ProviderPresentationDescriptor,
) -> Result<BarSeriesKey, CadenceError> {
    Ok(BarSeriesKey {
        provider_id: instrument.provider.clone(),
        instrument_id: instrument.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        period: resolve_cadence(interval, presentation)?,
        definition_version: 1,
    })
}

/// Parses the stable workspace representation of `ChartInterval`.
#[must_use]
pub fn persisted_interval(label: &str) -> Option<ChartInterval> {
    ChartInterval::ALL
        .into_iter()
        .find(|interval| interval.label() == label)
}

/// Reads the cadence representation used by pre-M1.3 workspace files.
///
/// Both historical Day3 encodings are accepted because released writers emitted each form.
#[must_use]
pub fn legacy_interval(series: &SeriesKey) -> Option<ChartInterval> {
    let cadence = SeriesCadence::try_from(series.cadence).ok()?;
    match (cadence, series.cadence_value) {
        (SeriesCadence::Trades, 100) => Some(ChartInterval::Tick100),
        (SeriesCadence::FixedSeconds, 1) => Some(ChartInterval::Second1),
        (SeriesCadence::FixedSeconds, 5) => Some(ChartInterval::Second5),
        (SeriesCadence::FixedSeconds, 10) => Some(ChartInterval::Second10),
        (SeriesCadence::FixedSeconds, 15) => Some(ChartInterval::Second15),
        (SeriesCadence::FixedSeconds, 30) => Some(ChartInterval::Second30),
        (SeriesCadence::FixedSeconds, 60) => Some(ChartInterval::Minute1),
        (SeriesCadence::FixedSeconds, 180) => Some(ChartInterval::Minute3),
        (SeriesCadence::FixedSeconds, 300) => Some(ChartInterval::Minute5),
        (SeriesCadence::FixedSeconds, 900) => Some(ChartInterval::Minute15),
        (SeriesCadence::FixedSeconds, 1_800) => Some(ChartInterval::Minute30),
        (SeriesCadence::FixedSeconds, 3_600) => Some(ChartInterval::Hour1),
        (SeriesCadence::FixedSeconds, 7_200) => Some(ChartInterval::Hour2),
        (SeriesCadence::FixedSeconds, 14_400) => Some(ChartInterval::Hour4),
        (SeriesCadence::FixedSeconds, 28_800) => Some(ChartInterval::Hour8),
        (SeriesCadence::FixedSeconds, 43_200) => Some(ChartInterval::Hour12),
        (SeriesCadence::FixedSeconds, 86_400) | (SeriesCadence::SessionDays, 1) => {
            Some(ChartInterval::Day1)
        }
        (SeriesCadence::FixedSeconds, 259_200) | (SeriesCadence::SessionDays, 3) => {
            Some(ChartInterval::Day3)
        }
        (SeriesCadence::CalendarWeeks, 1) => Some(ChartInterval::Week1),
        (SeriesCadence::CalendarMonths, 1) => Some(ChartInterval::Month1),
        _ => None,
    }
}

/// Converts a legacy persisted series to its canonical runtime period.
#[must_use]
pub fn legacy_series_period(series: &SeriesKey) -> Option<BarPeriod> {
    let interval = legacy_interval(series)?;
    match interval {
        ChartInterval::Tick100 => BarPeriod::tick(100).ok(),
        ChartInterval::Second1 => BarPeriod::time(1).ok(),
        ChartInterval::Second5 => BarPeriod::time(5).ok(),
        ChartInterval::Second10 => BarPeriod::time(10).ok(),
        ChartInterval::Second15 => BarPeriod::time(15).ok(),
        ChartInterval::Second30 => BarPeriod::time(30).ok(),
        ChartInterval::Minute1 => BarPeriod::time(60).ok(),
        ChartInterval::Minute3 => BarPeriod::time(180).ok(),
        ChartInterval::Minute5 => BarPeriod::time(300).ok(),
        ChartInterval::Minute15 => BarPeriod::time(900).ok(),
        ChartInterval::Minute30 => BarPeriod::time(1_800).ok(),
        ChartInterval::Hour1 => BarPeriod::time(3_600).ok(),
        ChartInterval::Hour2 => BarPeriod::time(7_200).ok(),
        ChartInterval::Hour4 => BarPeriod::time(14_400).ok(),
        ChartInterval::Hour8 => BarPeriod::time(28_800).ok(),
        ChartInterval::Hour12 => BarPeriod::time(43_200).ok(),
        ChartInterval::Day1 => match SeriesCadence::try_from(series.cadence).ok()? {
            SeriesCadence::SessionDays => BarPeriod::session(1).ok(),
            _ => BarPeriod::time(86_400).ok(),
        },
        ChartInterval::Day3 => match SeriesCadence::try_from(series.cadence).ok()? {
            SeriesCadence::SessionDays => BarPeriod::session(3).ok(),
            _ => BarPeriod::time(259_200).ok(),
        },
        ChartInterval::Week1 => BarPeriod::week(1).ok(),
        ChartInterval::Month1 => BarPeriod::month(1).ok(),
    }
}

fn unsupported(
    presentation: &ProviderPresentationDescriptor,
    interval: ChartInterval,
) -> CadenceError {
    CadenceError {
        provider: presentation.display_name.to_string(),
        interval: interval.label().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{legacy_interval, persisted_interval, resolve_cadence};
    use crate::built_in_provider_presentations;
    use aeris_contracts::{SeriesCadence, SeriesKey};
    use aeris_market_data::{BarPeriod, ChartInterval};

    #[test]
    fn every_offered_interval_round_trips_for_every_provider_presentation() {
        for presentation in built_in_provider_presentations() {
            for interval in ChartInterval::ALL {
                let persisted = interval.label();
                assert_eq!(persisted_interval(persisted), Some(interval));
                let resolution = resolve_cadence(interval, presentation);
                assert_eq!(
                    resolution.is_ok(),
                    presentation.chart_interval_labels.contains(&persisted),
                    "{} {persisted}",
                    presentation.id
                );
            }
        }
    }

    #[test]
    fn legacy_day3_accepts_fixed_and_session_encodings() {
        for (cadence, value) in [
            (SeriesCadence::FixedSeconds, 259_200),
            (SeriesCadence::SessionDays, 3),
        ] {
            assert_eq!(
                legacy_interval(&SeriesKey {
                    cadence: cadence as i32,
                    cadence_value: value,
                    ..SeriesKey::default()
                }),
                Some(ChartInterval::Day3)
            );
        }
    }

    #[test]
    fn hyperliquid_daily_cadences_are_fixed_utc_durations() {
        let presentation = built_in_provider_presentations()
            .iter()
            .find(|presentation| presentation.id == "hyperliquid")
            .expect("Hyperliquid presentation");
        assert_eq!(
            resolve_cadence(ChartInterval::Day1, presentation),
            Ok(BarPeriod::time(86_400).expect("one day"))
        );
        assert_eq!(
            resolve_cadence(ChartInterval::Day3, presentation),
            Ok(BarPeriod::time(259_200).expect("three days"))
        );
    }
}
