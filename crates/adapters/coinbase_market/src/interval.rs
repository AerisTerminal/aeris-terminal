use axiusflow_market_data::{ChartInterval, MarketBar};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CoinbaseInterval {
    Minute1,
    Minute3,
    Minute5,
    Minute15,
    Minute30,
    Hour1,
    Hour2,
    Hour4,
    Hour8,
    Hour12,
    Day1,
    Day3,
    Week1,
    Month1,
}

impl CoinbaseInterval {
    pub const ALL: [Self; 14] = [
        Self::Minute1,
        Self::Minute3,
        Self::Minute5,
        Self::Minute15,
        Self::Minute30,
        Self::Hour1,
        Self::Hour2,
        Self::Hour4,
        Self::Hour8,
        Self::Hour12,
        Self::Day1,
        Self::Day3,
        Self::Week1,
        Self::Month1,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Minute1 => "1m",
            Self::Minute3 => "3m",
            Self::Minute5 => "5m",
            Self::Minute15 => "15m",
            Self::Minute30 => "30m",
            Self::Hour1 => "1h",
            Self::Hour2 => "2h",
            Self::Hour4 => "4h",
            Self::Hour8 => "8h",
            Self::Hour12 => "12h",
            Self::Day1 => "1D",
            Self::Day3 => "3D",
            Self::Week1 => "1W",
            Self::Month1 => "1M",
        }
    }

    #[must_use]
    pub const fn fixed_seconds(self) -> Option<i64> {
        match self {
            Self::Minute1 => Some(60),
            Self::Minute3 => Some(180),
            Self::Minute5 => Some(300),
            Self::Minute15 => Some(900),
            Self::Minute30 => Some(1_800),
            Self::Hour1 => Some(3_600),
            Self::Hour2 => Some(7_200),
            Self::Hour4 => Some(14_400),
            Self::Hour8 => Some(28_800),
            Self::Hour12 => Some(43_200),
            Self::Day1 => Some(86_400),
            Self::Day3 => Some(259_200),
            Self::Week1 | Self::Month1 => None,
        }
    }
}

impl TryFrom<ChartInterval> for CoinbaseInterval {
    type Error = &'static str;

    fn try_from(value: ChartInterval) -> Result<Self, Self::Error> {
        match value {
            ChartInterval::Tick100 => Err("Coinbase public markets do not support tick bars"),
            ChartInterval::Minute1 => Ok(Self::Minute1),
            ChartInterval::Minute3 => Ok(Self::Minute3),
            ChartInterval::Minute5 => Ok(Self::Minute5),
            ChartInterval::Minute15 => Ok(Self::Minute15),
            ChartInterval::Minute30 => Ok(Self::Minute30),
            ChartInterval::Hour1 => Ok(Self::Hour1),
            ChartInterval::Hour2 => Ok(Self::Hour2),
            ChartInterval::Hour4 => Ok(Self::Hour4),
            ChartInterval::Hour8 => Ok(Self::Hour8),
            ChartInterval::Hour12 => Ok(Self::Hour12),
            ChartInterval::Day1 => Ok(Self::Day1),
            ChartInterval::Day3 => Ok(Self::Day3),
            ChartInterval::Week1 => Ok(Self::Week1),
            ChartInterval::Month1 => Ok(Self::Month1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoinbaseAggregationDiagnostics {
    pub source_bars: u64,
    pub output_bars: u64,
    pub duplicate_bars: u64,
    pub gaps: u64,
}

/// Aggregates sorted or unsorted Coinbase source bars into one provider-neutral interval.
///
/// # Errors
///
/// Returns an error for invalid bars, unsupported timestamps, or numeric overflow.
pub fn aggregate_coinbase_bars(
    source: &[MarketBar],
    interval: CoinbaseInterval,
) -> Result<(Vec<MarketBar>, CoinbaseAggregationDiagnostics), String> {
    let mut diagnostics = CoinbaseAggregationDiagnostics::default();
    let mut sorted = source.to_vec();
    sorted.sort_by_key(|bar| bar.exchange_timestamp_seconds);
    let mut deduped = Vec::with_capacity(sorted.len());
    for bar in sorted {
        bar.validate().map_err(|error| error.to_string())?;
        if deduped.last().is_some_and(|previous: &MarketBar| {
            previous.exchange_timestamp_seconds == bar.exchange_timestamp_seconds
        }) {
            diagnostics.duplicate_bars = diagnostics.duplicate_bars.saturating_add(1);
            deduped.pop();
        }
        deduped.push(bar);
    }
    diagnostics.source_bars = deduped.len() as u64;
    for pair in deduped.windows(2) {
        if pair[1].exchange_timestamp_seconds - pair[0].exchange_timestamp_seconds != 60 {
            diagnostics.gaps = diagnostics.gaps.saturating_add(1);
        }
    }
    let mut output = Vec::<MarketBar>::new();
    for bar in deduped {
        let bucket = bucket_start(bar.exchange_timestamp_seconds, interval)?;
        if let Some(current) = output.last_mut()
            && current.exchange_timestamp_seconds == bucket
        {
            current.high = current.high.max(bar.high);
            current.low = current.low.min(bar.low);
            current.close = bar.close;
            current.volume = current
                .volume
                .checked_add(bar.volume)
                .ok_or_else(|| "Coinbase aggregate volume overflow".to_string())?;
            continue;
        }
        output.push(MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: bucket,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
        });
    }
    for (index, bar) in output.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Coinbase aggregate sequence overflow".to_string())?;
    }
    diagnostics.output_bars = output.len() as u64;
    Ok((output, diagnostics))
}

fn bucket_start(timestamp: i64, interval: CoinbaseInterval) -> Result<i64, String> {
    if timestamp < 0 || timestamp % 60 != 0 {
        return Err("Coinbase source bar timestamp is invalid".to_string());
    }
    if let Some(seconds) = interval.fixed_seconds() {
        return Ok(timestamp.div_euclid(seconds) * seconds);
    }
    let day = timestamp.div_euclid(86_400);
    match interval {
        CoinbaseInterval::Week1 => {
            // 1970-01-01 was Thursday; ISO weeks begin Monday.
            Ok((day - (day + 3).rem_euclid(7)) * 86_400)
        }
        CoinbaseInterval::Month1 => {
            let (year, month, _) = civil_from_days(day);
            Ok(days_from_civil(year, month, 1) * 86_400)
        }
        _ => Err("Coinbase interval has no bucket rule".to_string()),
    }
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let day = doy - (153 * mp + 2).div_euclid(5) + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (
        year + i64::from(month <= 2),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{CoinbaseInterval, aggregate_coinbase_bars};
    use axiusflow_market_data::MarketBar;

    fn bar(timestamp: i64, price: i64) -> MarketBar {
        MarketBar {
            source_sequence: u64::try_from(timestamp / 60 + 1).expect("positive"),
            exchange_timestamp_seconds: timestamp,
            open: price,
            high: price + 2,
            low: price - 2,
            close: price + 1,
            volume: 10,
        }
    }

    #[test]
    fn interval_catalog_is_complete_and_stable() {
        assert_eq!(
            CoinbaseInterval::ALL.map(CoinbaseInterval::id),
            [
                "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "3D", "1W",
                "1M",
            ]
        );
    }

    #[test]
    fn aggregation_deduplicates_and_reports_source_gaps() {
        let input = [bar(0, 100), bar(60, 101), bar(60, 102), bar(180, 103)];
        let (output, diagnostics) =
            aggregate_coinbase_bars(&input, CoinbaseInterval::Minute3).expect("aggregates");
        assert_eq!(diagnostics.duplicate_bars, 1);
        assert_eq!(diagnostics.gaps, 1);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].open, 100);
        assert_eq!(output[0].close, 103);
        assert_eq!(output[0].volume, 20);
    }

    #[test]
    fn week_and_month_buckets_use_utc_calendar_boundaries() {
        // 2024-01-01 00:00:00 UTC was Monday and is also a month boundary.
        let timestamp = 1_704_067_200;
        for interval in [CoinbaseInterval::Week1, CoinbaseInterval::Month1] {
            let (output, _) = aggregate_coinbase_bars(&[bar(timestamp, 100)], interval)
                .expect("calendar aggregation succeeds");
            assert_eq!(output[0].exchange_timestamp_seconds, timestamp);
        }
    }
}
