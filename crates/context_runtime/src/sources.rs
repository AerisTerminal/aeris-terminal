//! Bounded official-source adapters for public context data.

use crate::{
    ContextFetchError, ContextFetcher, ContextMetric, ContextProvenance, ContextSource,
    CotPosition, DegreeDayMetric, EconomicEvent, EventImportance, SourcePublication,
};
use chrono::{Datelike, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday};
use serde_json::Value;
use std::{io::Read, time::Duration};

const MAXIMUM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const USER_AGENT: &str = "Aeris/0.2.3 (+https://github.com/AerisTerminal/aeris-terminal)";
const BLS_CALENDAR_URL: &str = "https://www.bls.gov/schedule/news_release/bls.ics";
const BEA_CALENDAR_URL: &str = "https://www.bea.gov/news/schedule";
const FED_CALENDAR_URL: &str = "https://www.federalreserve.gov/monetarypolicy/fomccalendars.htm";
const CFTC_COT_URL: &str = "https://publicreporting.cftc.gov/resource/72hh-3qpy.json";
const NOAA_CPC_DEGREE_DAY_ROOT: &str =
    "https://ftp.cpc.ncep.noaa.gov/htdocs/degree_days/weighted/daily_forecasts_7day";
const USDA_URL: &str = "https://quickstats.nass.usda.gov/api/api_GET/";
const USDA_WASDE_URL: &str =
    "https://esmis.nal.usda.gov/publication/world-agricultural-supply-and-demand-estimates";
const USDA_FAS_URL: &str = "https://api.fas.usda.gov/api/esr";
const FRED_URL: &str = "https://api.stlouisfed.org/fred/series/observations";
const EIA_CRUDE_URL: &str = "https://api.eia.gov/v2/petroleum/stoc/wstk/data/";
const EIA_GAS_URL: &str = "https://api.eia.gov/v2/natural-gas/stor/wkly/data/";
const EIA_RELEASE_SCHEDULE_URL: &str = "https://www.eia.gov/petroleum/supply/weekly/schedule.php";

/// Production client for the official public endpoints listed in the roadmap.
pub struct OfficialContextFetcher {
    agent: ureq::Agent,
}

impl OfficialContextFetcher {
    /// Builds one shared TLS client with a global per-request timeout.
    ///
    /// # Errors
    /// Returns an error when the timeout is outside the context service's safe range.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        if !(Duration::from_secs(1)..=Duration::from_secs(30)).contains(&timeout) {
            return Err("context HTTP timeout is invalid".to_string());
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
        })
    }

    fn get(&self, source: ContextSource, url: &str) -> Result<Vec<u8>, ContextFetchError> {
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json, text/calendar, text/html;q=0.8")
            .call()
            .map_err(|error| transport_error(source, error))?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAXIMUM_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                ContextFetchError::unavailable(format!(
                    "{} response could not be read",
                    source.label()
                ))
            })?;
        if bytes.len() > MAXIMUM_RESPONSE_BYTES {
            return Err(ContextFetchError::unavailable(format!(
                "{} response exceeds the byte limit",
                source.label()
            )));
        }
        Ok(bytes)
    }

    fn get_text(&self, source: ContextSource, url: &str) -> Result<String, ContextFetchError> {
        String::from_utf8(self.get(source, url)?).map_err(|_| {
            ContextFetchError::unavailable(format!("{} response is not UTF-8", source.label()))
        })
    }

    fn get_json(&self, source: ContextSource, url: &str) -> Result<Value, ContextFetchError> {
        serde_json::from_slice(&self.get(source, url)?).map_err(|_| {
            ContextFetchError::unavailable(format!("{} response is malformed", source.label()))
        })
    }

    fn get_json_with_api_key(
        &self,
        source: ContextSource,
        url: &str,
        api_key: &str,
    ) -> Result<Value, ContextFetchError> {
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json")
            .header("X-Api-Key", api_key)
            .call()
            .map_err(|error| transport_error(source, error))?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAXIMUM_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ContextFetchError::unavailable("USDA FAS response could not be read"))?;
        if bytes.len() > MAXIMUM_RESPONSE_BYTES {
            return Err(ContextFetchError::unavailable(
                "USDA FAS response exceeds the byte limit",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| ContextFetchError::unavailable("USDA FAS response is malformed"))
    }
}

impl ContextFetcher for OfficialContextFetcher {
    fn fetch(
        &self,
        source: ContextSource,
        credential: Option<&str>,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        match source {
            ContextSource::Bls => {
                let text = self.get_text(source, BLS_CALENDAR_URL)?;
                parse_bls_calendar(&text, fetched_unix_seconds)
            }
            ContextSource::Bea => {
                let text = self.get_text(source, BEA_CALENDAR_URL)?;
                parse_bea_calendar(&text, fetched_unix_seconds)
            }
            ContextSource::FederalReserve => {
                let text = self.get_text(source, FED_CALENDAR_URL)?;
                parse_fed_calendar(&text, fetched_unix_seconds)
            }
            ContextSource::Eia => self.fetch_eia(
                credential.ok_or_else(|| ContextFetchError::missing_credential(source))?,
                fetched_unix_seconds,
            ),
            ContextSource::Noaa => self.fetch_noaa(fetched_unix_seconds),
            ContextSource::Cftc => self.fetch_cftc(fetched_unix_seconds),
            ContextSource::Usda => self.fetch_usda(
                credential.ok_or_else(|| ContextFetchError::missing_credential(source))?,
                fetched_unix_seconds,
            ),
            ContextSource::UsdaWasde => self.fetch_usda_wasde(fetched_unix_seconds),
            ContextSource::UsdaFas => self.fetch_usda_fas(
                credential.ok_or_else(|| ContextFetchError::missing_credential(source))?,
                fetched_unix_seconds,
            ),
            ContextSource::Fred => self.fetch_fred(
                credential.ok_or_else(|| ContextFetchError::missing_credential(source))?,
                fetched_unix_seconds,
            ),
        }
    }
}

impl OfficialContextFetcher {
    fn fetch_eia(
        &self,
        api_key: &str,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let crude_url = format!(
            "{EIA_CRUDE_URL}?api_key={api_key}&frequency=weekly&data[0]=value&facets[series][]=WCESTUS1&sort[0][column]=period&sort[0][direction]=desc&length=270"
        );
        let gas_url = format!(
            "{EIA_GAS_URL}?api_key={api_key}&frequency=weekly&data[0]=value&facets[series][]=NW2_EPG0_SWO_R48_BCF&sort[0][column]=period&sort[0][direction]=desc&length=270"
        );
        let crude = self.get_json(ContextSource::Eia, &crude_url)?;
        let gas = self.get_json(ContextSource::Eia, &gas_url)?;
        let release_schedule = self.get_text(ContextSource::Eia, EIA_RELEASE_SCHEDULE_URL)?;
        let mut publication = SourcePublication::empty(ContextSource::Eia);
        publication.economic_events =
            parse_eia_release_schedule(&release_schedule, fetched_unix_seconds)?;
        publication.energy.push(parse_eia_series(
            &crude,
            "commercial-crude-stocks",
            "Commercial crude oil stocks",
            "thousand barrels",
            EIA_CRUDE_URL,
            fetched_unix_seconds,
        )?);
        publication.energy.push(parse_eia_series(
            &gas,
            "lower-48-natural-gas-storage",
            "Lower 48 working natural gas storage",
            "billion cubic feet",
            EIA_GAS_URL,
            fetched_unix_seconds,
        )?);
        Ok(publication)
    }

    fn fetch_noaa(
        &self,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let fetched_date = Utc
            .timestamp_opt(fetched_unix_seconds, 0)
            .single()
            .map(|time| time.date_naive())
            .ok_or_else(|| ContextFetchError::unavailable("NOAA timestamp is invalid"))?;
        let mut last_error = ContextFetchError::unavailable("NOAA CPC forecast is unavailable");
        for age_days in 0..=3 {
            let Some(issue_date) = fetched_date.checked_sub_days(chrono::Days::new(age_days))
            else {
                continue;
            };
            let directory = format!(
                "{NOAA_CPC_DEGREE_DAY_ROOT}/{}/{:02}/{:02}",
                issue_date.year(),
                issue_date.month(),
                issue_date.day()
            );
            let cooling_url = format!("{directory}/Population.Cooling.txt");
            let heating_url = format!("{directory}/Population.Heating.txt");
            let cooling = match self.get_text(ContextSource::Noaa, &cooling_url) {
                Ok(value) => value,
                Err(error) => {
                    last_error = error;
                    continue;
                }
            };
            let heating = match self.get_text(ContextSource::Noaa, &heating_url) {
                Ok(value) => value,
                Err(error) => {
                    last_error = error;
                    continue;
                }
            };
            let metric =
                parse_noaa_degree_days(&heating, &cooling, &directory, fetched_unix_seconds)?;
            let mut publication = SourcePublication::empty(ContextSource::Noaa);
            publication.weather.push(metric);
            return Ok(publication);
        }
        Err(last_error)
    }

    fn fetch_cftc(
        &self,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let url = format!("{CFTC_COT_URL}?$limit=200&$order=report_date_as_yyyy_mm_dd%20DESC");
        let json = self.get_json(ContextSource::Cftc, &url)?;
        parse_cftc(&json, fetched_unix_seconds)
    }

    fn fetch_usda(
        &self,
        api_key: &str,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let year = Utc
            .timestamp_opt(fetched_unix_seconds, 0)
            .single()
            .ok_or_else(|| ContextFetchError::unavailable("USDA timestamp is invalid"))?
            .year();
        let url = format!(
            "{USDA_URL}?key={api_key}&format=JSON&commodity_desc=CORN&statisticcat_desc=CONDITION&agg_level_desc=NATIONAL&freq_desc=WEEKLY&year__GE={}&short_desc__LIKE=CONDITION",
            year - 1
        );
        let json = self.get_json(ContextSource::Usda, &url)?;
        parse_usda(&json, fetched_unix_seconds)
    }

    fn fetch_usda_wasde(
        &self,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let landing = self.get_text(ContextSource::UsdaWasde, USDA_WASDE_URL)?;
        let (release_unix_seconds, text_url) = parse_wasde_landing(&landing)?;
        let report = self.get_text(ContextSource::UsdaWasde, &text_url)?;
        parse_wasde_report(
            &report,
            &text_url,
            release_unix_seconds,
            fetched_unix_seconds,
        )
    }

    fn fetch_usda_fas(
        &self,
        api_key: &str,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let commodities = self.get_json_with_api_key(
            ContextSource::UsdaFas,
            &format!("{USDA_FAS_URL}/commodities"),
            api_key,
        )?;
        let releases = self.get_json_with_api_key(
            ContextSource::UsdaFas,
            &format!("{USDA_FAS_URL}/datareleasedates"),
            api_key,
        )?;
        let requests = fas_export_requests(&commodities, &releases)?;
        let mut publication = SourcePublication::empty(ContextSource::UsdaFas);
        for request in requests.into_iter().take(3) {
            let url = format!(
                "{USDA_FAS_URL}/exports/commodityCode/{}/allCountries/marketYear/{}",
                request.commodity_code, request.market_year
            );
            let exports = self.get_json_with_api_key(ContextSource::UsdaFas, &url, api_key)?;
            publication.agriculture.push(parse_fas_exports(
                &exports,
                &request,
                &url,
                fetched_unix_seconds,
            )?);
        }
        if publication.agriculture.is_empty() {
            return Err(ContextFetchError::unavailable(
                "USDA FAS export sales contain no supported commodities",
            ));
        }
        Ok(publication)
    }

    fn fetch_fred(
        &self,
        api_key: &str,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError> {
        let series = [
            ("DGS10", "10-year Treasury yield", "percent"),
            ("DTWEXBGS", "Trade-weighted U.S. dollar index", "index"),
            ("CPIAUCSL", "Consumer Price Index", "index"),
            ("PAYEMS", "Total nonfarm payrolls", "thousands of persons"),
        ];
        let mut publication = SourcePublication::empty(ContextSource::Fred);
        for (series_id, label, unit) in series {
            let url = format!(
                "{FRED_URL}?series_id={series_id}&api_key={api_key}&file_type=json&sort_order=desc&limit=2"
            );
            let json = self.get_json(ContextSource::Fred, &url)?;
            publication.macro_observations.push(parse_fred_series(
                &json,
                series_id,
                label,
                unit,
                fetched_unix_seconds,
            )?);
        }
        Ok(publication)
    }
}

fn transport_error(source: ContextSource, error: ureq::Error) -> ContextFetchError {
    let detail = match error {
        ureq::Error::StatusCode(status) => {
            format!("{} request failed with HTTP {status}", source.label())
        }
        ureq::Error::Timeout(_) => format!("{} request timed out", source.label()),
        ureq::Error::HostNotFound => format!("{} host could not be resolved", source.label()),
        ureq::Error::ConnectionFailed => format!("{} connection failed", source.label()),
        ureq::Error::Protocol(_) => format!("{} HTTP protocol failed", source.label()),
        ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects => {
            format!("{} redirect validation failed", source.label())
        }
        ureq::Error::Http(_) | ureq::Error::BadUri(_) => {
            format!("{} request URI is invalid", source.label())
        }
        ureq::Error::Io(error) => {
            format!("{} transport failed: {:?}", source.label(), error.kind())
        }
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => {
            format!("{} TLS validation failed", source.label())
        }
        _ => format!("{} request failed", source.label()),
    };
    ContextFetchError::unavailable(detail)
}

#[derive(Clone, Debug)]
struct FasExportRequest {
    commodity_code: i64,
    commodity_name: String,
    market_year: i32,
    release_unix_seconds: i64,
}

fn parse_wasde_landing(html: &str) -> Result<(i64, String), ContextFetchError> {
    let marker = "/sites/default/release-files/";
    let text_end = html
        .find(".txt")
        .map(|index| index + 4)
        .ok_or_else(|| ContextFetchError::unavailable("USDA WASDE text release is missing"))?;
    let start = html[..text_end]
        .rfind(marker)
        .ok_or_else(|| ContextFetchError::unavailable("USDA WASDE release link is missing"))?;
    let text_url = format!("https://esmis.nal.usda.gov{}", &html[start..text_end]);
    let datetime = after(&html[start..], "datetime=\"")
        .and_then(|value| value.split_once('"').map(|(value, _)| value))
        .ok_or_else(|| ContextFetchError::unavailable("USDA WASDE release date is missing"))?;
    let release = chrono::DateTime::parse_from_rfc3339(datetime)
        .map(|value| value.timestamp())
        .map_err(|_| ContextFetchError::unavailable("USDA WASDE release date is invalid"))?;
    Ok((release, text_url))
}

fn parse_wasde_report(
    text: &str,
    source_url: &str,
    release_unix_seconds: i64,
    fetched_unix_seconds: i64,
) -> Result<SourcePublication, ContextFetchError> {
    if release_unix_seconds > fetched_unix_seconds {
        return Err(ContextFetchError::unavailable(
            "USDA WASDE release is not yet public",
        ));
    }
    let mut lines = text
        .lines()
        .skip_while(|line| line.trim() != "CORN")
        .skip(1);
    let corn = lines
        .by_ref()
        .take_while(|line| !line.contains("===="))
        .collect::<Vec<_>>();
    if corn.is_empty() {
        return Err(ContextFetchError::unavailable(
            "USDA WASDE corn table is missing",
        ));
    }
    let release_period = Utc
        .timestamp_opt(release_unix_seconds, 0)
        .single()
        .map(|value| value.format("%Y-%m-%d").to_string())
        .ok_or_else(|| ContextFetchError::unavailable("USDA WASDE release date is invalid"))?;
    let mut publication = SourcePublication::empty(ContextSource::UsdaWasde);
    for (id, label, row_label) in [
        (
            "corn-production",
            "WASDE U.S. corn production",
            "Production",
        ),
        ("corn-exports", "WASDE U.S. corn exports", "Exports"),
        (
            "corn-ending-stocks",
            "WASDE U.S. corn ending stocks",
            "Ending Stocks",
        ),
    ] {
        let line = corn
            .iter()
            .copied()
            .find(|line| line.trim_start().starts_with(row_label))
            .ok_or_else(|| ContextFetchError::unavailable("USDA WASDE corn row is missing"))?;
        let value = line
            .split_whitespace()
            .next_back()
            .and_then(parse_decimal)
            .ok_or_else(|| ContextFetchError::unavailable("USDA WASDE corn value is invalid"))?;
        publication.agriculture.push(ContextMetric {
            id: format!("usda-wasde:{id}"),
            label: label.to_string(),
            period: release_period.clone(),
            value_units: value.0,
            value_scale: value.1,
            unit: "million bushels".to_string(),
            expected_units: None,
            five_year_min_units: None,
            five_year_max_units: None,
            provenance: ContextProvenance {
                source: ContextSource::UsdaWasde,
                source_url: source_url.to_string(),
                release_unix_seconds,
                fetched_unix_seconds,
            },
        });
    }
    Ok(publication)
}

fn fas_export_requests(
    commodities: &Value,
    releases: &Value,
) -> Result<Vec<FasExportRequest>, ContextFetchError> {
    let commodity_rows = json_rows(commodities).ok_or_else(|| {
        ContextFetchError::unavailable("USDA FAS commodity response contains no rows")
    })?;
    let mut names = std::collections::BTreeMap::new();
    for row in commodity_rows {
        let Some(code) = json_i64(row, &["commodityCode", "commodity_code"]) else {
            continue;
        };
        let Some(name) = json_string(
            row,
            &["commodityName", "commodityDescription", "commodity_name"],
        ) else {
            continue;
        };
        names.insert(code, name.to_string());
    }
    let release_rows = json_rows(releases).ok_or_else(|| {
        ContextFetchError::unavailable("USDA FAS release response contains no rows")
    })?;
    let mut selected: std::collections::BTreeMap<&'static str, FasExportRequest> =
        std::collections::BTreeMap::new();
    for row in release_rows {
        let Some(code) = json_i64(row, &["commodityCode", "commodity_code"]) else {
            continue;
        };
        let Some(name) = names.get(&code) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let family = if lower == "corn" || lower.contains("corn -") {
            Some("corn")
        } else if lower == "soybeans" || lower.contains("soybeans -") {
            Some("soybeans")
        } else if lower == "wheat" || lower.contains("wheat - total") {
            Some("wheat")
        } else {
            None
        };
        let Some(family) = family else { continue };
        let Some(market_year) = json_i64(row, &["marketYear", "market_year"])
            .and_then(|value| i32::try_from(value).ok())
        else {
            continue;
        };
        let Some(release) = json_string(row, &["releaseDate", "dataReleaseDate", "release_date"])
            .and_then(parse_publication_timestamp)
        else {
            continue;
        };
        let request = FasExportRequest {
            commodity_code: code,
            commodity_name: name.clone(),
            market_year,
            release_unix_seconds: release,
        };
        if selected.get(family).is_none_or(|current| {
            (request.release_unix_seconds, request.market_year)
                > (current.release_unix_seconds, current.market_year)
        }) {
            selected.insert(family, request);
        }
    }
    if selected.is_empty() {
        return Err(ContextFetchError::unavailable(
            "USDA FAS release response has no corn, soybean, or wheat data",
        ));
    }
    Ok(selected.into_values().collect())
}

fn parse_fas_exports(
    json: &Value,
    request: &FasExportRequest,
    source_url: &str,
    fetched_unix_seconds: i64,
) -> Result<ContextMetric, ContextFetchError> {
    let rows = json_rows(json)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| {
            ContextFetchError::unavailable("USDA FAS export response contains no rows")
        })?;
    let latest_period = rows
        .iter()
        .filter_map(|row| json_string(row, &["weekEndingDate", "weekEnding", "week_ending_date"]))
        .max()
        .ok_or_else(|| ContextFetchError::unavailable("USDA FAS export period is missing"))?;
    let mut values = Vec::new();
    for row in rows {
        if json_string(row, &["weekEndingDate", "weekEnding", "week_ending_date"])
            != Some(latest_period)
        {
            continue;
        }
        if let Some(value) = json_decimal(row, &["weeklyExports", "weeklyExport", "weekly_exports"])
        {
            values.push(value);
        }
    }
    let (value_units, value_scale) = sum_fixed(&values).ok_or_else(|| {
        ContextFetchError::unavailable("USDA FAS weekly export values are missing")
    })?;
    Ok(ContextMetric {
        id: format!("usda-fas:{}-weekly-exports", request.commodity_code),
        label: format!("{} weekly export sales", request.commodity_name),
        period: latest_period.to_string(),
        value_units,
        value_scale,
        unit: "metric tons".to_string(),
        expected_units: None,
        five_year_min_units: None,
        five_year_max_units: None,
        provenance: ContextProvenance {
            source: ContextSource::UsdaFas,
            source_url: source_url.to_string(),
            release_unix_seconds: request.release_unix_seconds,
            fetched_unix_seconds,
        },
    })
}

fn provenance(
    source: ContextSource,
    source_url: &str,
    fetched_unix_seconds: i64,
) -> ContextProvenance {
    ContextProvenance {
        source,
        source_url: source_url.to_string(),
        release_unix_seconds: fetched_unix_seconds,
        fetched_unix_seconds,
    }
}

fn parse_bls_calendar(
    text: &str,
    fetched_unix_seconds: i64,
) -> Result<SourcePublication, ContextFetchError> {
    let mut publication = SourcePublication::empty(ContextSource::Bls);
    let mut unfolded = String::with_capacity(text.len());
    for line in text.replace("\r\n", "\n").lines() {
        if (line.starts_with(' ') || line.starts_with('\t')) && !unfolded.is_empty() {
            unfolded.push_str(line.trim_start());
        } else {
            unfolded.push('\n');
            unfolded.push_str(line);
        }
    }
    for block in unfolded.split("BEGIN:VEVENT").skip(1) {
        let block = block.split("END:VEVENT").next().unwrap_or(block);
        let summary = ics_value(block, "SUMMARY").filter(|value| !value.trim().is_empty());
        let start = block.lines().find_map(|line| {
            line.strip_prefix("DTSTART")
                .and_then(|rest| rest.split_once(':').map(|(_, value)| value.trim()))
        });
        let (Some(summary), Some(start)) = (summary, start) else {
            continue;
        };
        let scheduled = parse_ics_datetime(start).map_err(ContextFetchError::unavailable)?;
        publication.economic_events.push(EconomicEvent {
            id: format!("bls:{}:{scheduled}", slug(summary)),
            title: unescape_ics(summary),
            scheduled_unix_seconds: scheduled,
            importance: importance_for_title(summary),
            provenance: provenance(ContextSource::Bls, BLS_CALENDAR_URL, fetched_unix_seconds),
        });
    }
    if publication.economic_events.is_empty() {
        return Err(ContextFetchError::unavailable(
            "BLS calendar contains no scheduled events",
        ));
    }
    Ok(publication)
}

fn ics_value<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    block.lines().find_map(|line| {
        line.strip_prefix(name)
            .and_then(|rest| rest.split_once(':').map(|(_, value)| value.trim()))
    })
}

fn parse_ics_datetime(value: &str) -> Result<i64, String> {
    if let Some(utc) = value.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
            .map(|date| date.and_utc().timestamp())
            .map_err(|_| "BLS calendar timestamp is invalid".to_string());
    }
    let local = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .map_err(|_| "BLS calendar timestamp is invalid".to_string())?;
    eastern_timestamp(local).ok_or_else(|| "BLS calendar timestamp is out of range".to_string())
}

fn unescape_ics(value: &str) -> String {
    value
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\n", " ")
        .replace("\\\\", "\\")
}

fn parse_bea_calendar(
    html: &str,
    fetched_unix_seconds: i64,
) -> Result<SourcePublication, ContextFetchError> {
    let year = Utc
        .timestamp_opt(fetched_unix_seconds, 0)
        .single()
        .map_or(1970, |time| time.year());
    let mut publication = SourcePublication::empty(ContextSource::Bea);
    for row in html.split("<tr class=\"scheduled-releases-type-").skip(1) {
        let Some(date_text) = between(row, "<div class=\"release-date\">", "</div>") else {
            continue;
        };
        let Some(time_text) = between(row, "<small class=\"text-muted\">", "</small>") else {
            continue;
        };
        let Some(title_raw) = after(row, "class=\"release-title")
            .and_then(|rest| rest.split_once('>').map(|(_, value)| value))
            .and_then(|rest| rest.split_once("</td>").map(|(value, _)| value))
        else {
            continue;
        };
        let title = strip_html(title_raw);
        let local = parse_named_month_datetime(year, date_text, &strip_html(time_text))
            .map_err(ContextFetchError::unavailable)?;
        let scheduled = eastern_timestamp(local)
            .ok_or_else(|| ContextFetchError::unavailable("BEA timestamp is out of range"))?;
        publication.economic_events.push(EconomicEvent {
            id: format!("bea:{}:{scheduled}", slug(&title)),
            importance: importance_for_title(&title),
            title,
            scheduled_unix_seconds: scheduled,
            provenance: provenance(ContextSource::Bea, BEA_CALENDAR_URL, fetched_unix_seconds),
        });
    }
    if publication.economic_events.is_empty() {
        return Err(ContextFetchError::unavailable(
            "BEA release schedule contains no dated events",
        ));
    }
    Ok(publication)
}

fn parse_fed_calendar(
    html: &str,
    fetched_unix_seconds: i64,
) -> Result<SourcePublication, ContextFetchError> {
    let now = Utc
        .timestamp_opt(fetched_unix_seconds, 0)
        .single()
        .ok_or_else(|| ContextFetchError::unavailable("Federal Reserve timestamp is invalid"))?;
    let earliest_year = now.year();
    let latest_year = earliest_year + 1;
    let mut seen = std::collections::BTreeSet::new();
    let mut publication = SourcePublication::empty(ContextSource::FederalReserve);
    for prefix in ["fomcpresconf", "fomcpressconf"] {
        for suffix in html.split(prefix).skip(1) {
            let digits = suffix.chars().take(8).collect::<String>();
            if digits.len() != 8 || !digits.chars().all(|value| value.is_ascii_digit()) {
                continue;
            }
            let Ok(date) = NaiveDate::parse_from_str(&digits, "%Y%m%d") else {
                continue;
            };
            if !(earliest_year..=latest_year).contains(&date.year()) || !seen.insert(date) {
                continue;
            }
            let local = date
                .and_hms_opt(14, 0, 0)
                .ok_or_else(|| ContextFetchError::unavailable("FOMC timestamp is invalid"))?;
            let scheduled = eastern_timestamp(local)
                .ok_or_else(|| ContextFetchError::unavailable("FOMC timestamp is out of range"))?;
            publication.economic_events.push(EconomicEvent {
                id: format!("federal-reserve:fomc:{digits}"),
                title: "FOMC rate decision and press conference".to_string(),
                scheduled_unix_seconds: scheduled,
                importance: EventImportance::High,
                provenance: provenance(
                    ContextSource::FederalReserve,
                    FED_CALENDAR_URL,
                    fetched_unix_seconds,
                ),
            });
        }
    }
    if publication.economic_events.is_empty() {
        return Err(ContextFetchError::unavailable(
            "Federal Reserve calendar contains no current FOMC events",
        ));
    }
    Ok(publication)
}

fn parse_eia_series(
    json: &Value,
    id: &str,
    label: &str,
    unit: &str,
    source_url: &str,
    fetched_unix_seconds: i64,
) -> Result<ContextMetric, ContextFetchError> {
    let rows = json
        .pointer("/response/data")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| ContextFetchError::unavailable("EIA response contains no observations"))?;
    let latest = &rows[0];
    let period = required_string(latest, "period", "EIA period")?;
    let value = required_decimal(latest, "value", "EIA value")?;
    let (minimum, maximum) = seasonal_range(rows, period)?;
    Ok(ContextMetric {
        id: format!("eia:{id}"),
        label: label.to_string(),
        period: period.to_string(),
        value_units: value.0,
        value_scale: value.1,
        unit: unit.to_string(),
        expected_units: None,
        five_year_min_units: minimum,
        five_year_max_units: maximum,
        provenance: provenance(ContextSource::Eia, source_url, fetched_unix_seconds),
    })
}

fn parse_eia_release_schedule(
    html: &str,
    fetched_unix_seconds: i64,
) -> Result<Vec<EconomicEvent>, ContextFetchError> {
    let fetched_date = Utc
        .timestamp_opt(fetched_unix_seconds, 0)
        .single()
        .map(|time| time.date_naive())
        .ok_or_else(|| ContextFetchError::unavailable("EIA schedule timestamp is invalid"))?;
    let mut overrides = std::collections::BTreeMap::new();
    for row in html.split("<tr").skip(1) {
        let row = row.split("</tr>").next().unwrap_or(row);
        let Some(week_ending) = after(row, "scope=\"row\">")
            .and_then(|value| {
                value
                    .split_once("</th>")
                    .map(|(value, _)| strip_html(value))
            })
            .and_then(|value| NaiveDate::parse_from_str(value.trim(), "%B %d, %Y").ok())
        else {
            continue;
        };
        let cells = row
            .split("<td")
            .skip(1)
            .filter_map(|cell| {
                cell.split_once('>').and_then(|(_, value)| {
                    value
                        .split_once("</td>")
                        .map(|(value, _)| strip_html(value))
                })
            })
            .collect::<Vec<_>>();
        if cells.len() < 3 {
            continue;
        }
        let release_date = NaiveDate::parse_from_str(cells[0].trim(), "%B %d, %Y").ok();
        let release_time = NaiveDateTime::parse_from_str(
            &format!("{} {}", cells[0].trim(), cells[2].replace('.', "")),
            "%B %d, %Y %I:%M %p",
        )
        .ok();
        let standard_date = week_ending.checked_add_days(chrono::Days::new(5));
        if let (Some(standard_date), Some(release_date), Some(release_time)) =
            (standard_date, release_date, release_time)
            && release_date >= fetched_date
        {
            overrides.insert(standard_date, release_time);
        }
    }
    let days_until_wednesday = (7 + i64::from(Weekday::Wed.num_days_from_monday())
        - i64::from(fetched_date.weekday().num_days_from_monday()))
        % 7;
    let first_wednesday = fetched_date
        .checked_add_days(chrono::Days::new(
            u64::try_from(days_until_wednesday).unwrap_or(0),
        ))
        .ok_or_else(|| ContextFetchError::unavailable("EIA schedule date is out of range"))?;
    let mut events = Vec::with_capacity(13);
    for week in 0..13_u64 {
        let standard_date = first_wednesday
            .checked_add_days(chrono::Days::new(week * 7))
            .ok_or_else(|| ContextFetchError::unavailable("EIA schedule date is out of range"))?;
        let local = overrides
            .get(&standard_date)
            .copied()
            .or_else(|| standard_date.and_hms_opt(10, 30, 0));
        let Some(scheduled_unix_seconds) = local.and_then(eastern_timestamp) else {
            return Err(ContextFetchError::unavailable(
                "EIA release timestamp is out of range",
            ));
        };
        events.push(EconomicEvent {
            id: format!("eia:wpsr:{standard_date}"),
            title: "EIA Weekly Petroleum Status Report".to_string(),
            scheduled_unix_seconds,
            importance: EventImportance::High,
            provenance: provenance(
                ContextSource::Eia,
                EIA_RELEASE_SCHEDULE_URL,
                fetched_unix_seconds,
            ),
        });
    }
    Ok(events)
}

fn seasonal_range(
    rows: &[Value],
    latest_period: &str,
) -> Result<(Option<i128>, Option<i128>), ContextFetchError> {
    let target_week = iso_week(latest_period);
    let Some(target_week) = target_week else {
        return Ok((None, None));
    };
    let mut values = Vec::new();
    let mut years = std::collections::BTreeSet::new();
    for row in rows.iter().skip(1) {
        let Some(period) = row.get("period").and_then(Value::as_str) else {
            continue;
        };
        let Some((year, week)) = iso_week(period) else {
            continue;
        };
        if week.abs_diff(target_week.1) > 1 || !years.insert(year) || years.len() > 5 {
            continue;
        }
        let (units, scale) = required_decimal(row, "value", "EIA value")?;
        if scale == 0 {
            values.push(units);
        }
    }
    Ok(values
        .iter()
        .min()
        .copied()
        .zip(values.iter().max().copied())
        .map_or((None, None), |(min, max)| (Some(min), Some(max))))
}

fn iso_week(value: &str) -> Option<(i32, u32)> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    let week = date.iso_week();
    Some((week.year(), week.week()))
}

fn parse_noaa_degree_days(
    heating: &str,
    cooling: &str,
    source_url: &str,
    fetched_unix_seconds: i64,
) -> Result<DegreeDayMetric, ContextFetchError> {
    let (heating_issue, period, heating_total) = parse_cpc_degree_day_file(heating)?;
    let (cooling_issue, cooling_period, cooling_total) = parse_cpc_degree_day_file(cooling)?;
    if heating_issue != cooling_issue || period != cooling_period {
        return Err(ContextFetchError::unavailable(
            "NOAA CPC degree-day files do not describe the same forecast",
        ));
    }
    let release_unix_seconds = NaiveDate::parse_from_str(&heating_issue, "%Y%m%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc().timestamp())
        .ok_or_else(|| ContextFetchError::unavailable("NOAA CPC issue date is invalid"))?;
    Ok(DegreeDayMetric {
        region: "CONUS population weighted".to_string(),
        period,
        heating_degree_days_units: heating_total,
        cooling_degree_days_units: cooling_total,
        scale: 0,
        provenance: ContextProvenance {
            source: ContextSource::Noaa,
            source_url: source_url.to_string(),
            release_unix_seconds,
            fetched_unix_seconds,
        },
    })
}

fn parse_cpc_degree_day_file(text: &str) -> Result<(String, String, i64), ContextFetchError> {
    let issue = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().last())
        .filter(|value| {
            value.len() == 8 && value.chars().all(|character| character.is_ascii_digit())
        })
        .ok_or_else(|| ContextFetchError::unavailable("NOAA CPC issue date is missing"))?;
    let header = text
        .lines()
        .find(|line| line.starts_with("Region|"))
        .ok_or_else(|| ContextFetchError::unavailable("NOAA CPC forecast header is missing"))?;
    let dates = header.split('|').skip(1).collect::<Vec<_>>();
    let first = dates.first().copied().unwrap_or_default();
    let last = dates
        .iter()
        .rev()
        .find(|value| **value != "Total")
        .copied()
        .unwrap_or_default();
    let period = format!("{first} to {last}");
    let total = text
        .lines()
        .find(|line| line.starts_with("CONUS|"))
        .and_then(|line| line.split('|').next_back())
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| ContextFetchError::unavailable("NOAA CPC CONUS total is missing"))?;
    Ok((issue.to_string(), period, total))
}

fn parse_cftc(
    json: &Value,
    fetched_unix_seconds: i64,
) -> Result<SourcePublication, ContextFetchError> {
    let rows = json
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| ContextFetchError::unavailable("CFTC response contains no positions"))?;
    let latest_date = required_string(&rows[0], "report_date_as_yyyy_mm_dd", "CFTC report date")?;
    let report_date = parse_date_prefix(latest_date, "CFTC report date")?;
    let mut publication = SourcePublication::empty(ContextSource::Cftc);
    for row in rows.iter().take_while(|row| {
        row.get("report_date_as_yyyy_mm_dd")
            .and_then(Value::as_str)
            .is_some_and(|value| value == latest_date)
    }) {
        let market_code = required_string(row, "cftc_contract_market_code", "CFTC market code")?;
        let market_name = required_string(row, "market_and_exchange_names", "CFTC market name")?;
        let open_interest = required_i64(row, "open_interest_all", "CFTC open interest")?;
        for (category, long_key, short_key, spread_key) in [
            (
                "Producer/Merchant",
                "prod_merc_positions_long",
                "prod_merc_positions_short",
                None,
            ),
            (
                "Swap Dealer",
                "swap_positions_long_all",
                "swap__positions_short_all",
                Some("swap__positions_spread_all"),
            ),
            (
                "Managed Money",
                "m_money_positions_long_all",
                "m_money_positions_short_all",
                Some("m_money_positions_spread"),
            ),
            (
                "Other Reportable",
                "other_rept_positions_long",
                "other_rept_positions_short",
                Some("other_rept_positions_spread"),
            ),
        ] {
            publication.commitments.push(CotPosition {
                market_code: market_code.to_string(),
                market_name: market_name.to_string(),
                report_date_unix_seconds: report_date,
                category: category.to_string(),
                long_contracts: required_i64(row, long_key, "CFTC long position")?,
                short_contracts: required_i64(row, short_key, "CFTC short position")?,
                spreading_contracts: spread_key
                    .map(|key| required_i64(row, key, "CFTC spreading position"))
                    .transpose()?,
                open_interest,
                provenance: provenance(ContextSource::Cftc, CFTC_COT_URL, fetched_unix_seconds),
            });
        }
    }
    Ok(publication)
}

fn parse_usda(
    json: &Value,
    fetched_unix_seconds: i64,
) -> Result<SourcePublication, ContextFetchError> {
    let rows = json
        .get("data")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| ContextFetchError::unavailable("USDA response contains no crop data"))?;
    let mut publication = SourcePublication::empty(ContextSource::Usda);
    for row in rows.iter().take(512) {
        let short_desc = required_string(row, "short_desc", "USDA statistic")?;
        let period = required_string(row, "week_ending", "USDA week ending")?;
        let value = required_string(row, "Value", "USDA value")?.replace(',', "");
        let (value_units, value_scale) = parse_decimal(&value)
            .ok_or_else(|| ContextFetchError::unavailable("USDA value is invalid"))?;
        let unit = required_string(row, "unit_desc", "USDA unit")?;
        publication.agriculture.push(ContextMetric {
            id: format!("usda:{}:{}", slug(short_desc), period),
            label: short_desc.to_string(),
            period: period.to_string(),
            value_units,
            value_scale,
            unit: unit.to_string(),
            expected_units: None,
            five_year_min_units: None,
            five_year_max_units: None,
            provenance: provenance(ContextSource::Usda, USDA_URL, fetched_unix_seconds),
        });
    }
    Ok(publication)
}

fn parse_fred_series(
    json: &Value,
    series_id: &str,
    label: &str,
    unit: &str,
    fetched_unix_seconds: i64,
) -> Result<ContextMetric, ContextFetchError> {
    let observation = json
        .get("observations")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .find(|value| value.get("value").and_then(Value::as_str) != Some("."))
        })
        .ok_or_else(|| ContextFetchError::unavailable("FRED response contains no observation"))?;
    let period = required_string(observation, "date", "FRED observation date")?;
    let (value_units, value_scale) = required_decimal(observation, "value", "FRED value")?;
    Ok(ContextMetric {
        id: format!("fred:{series_id}"),
        label: label.to_string(),
        period: period.to_string(),
        value_units,
        value_scale,
        unit: unit.to_string(),
        expected_units: None,
        five_year_min_units: None,
        five_year_max_units: None,
        provenance: provenance(ContextSource::Fred, FRED_URL, fetched_unix_seconds),
    })
}

fn required_string<'a>(
    value: &'a Value,
    key: &str,
    label: &str,
) -> Result<&'a str, ContextFetchError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ContextFetchError::unavailable(format!("{label} is missing")))
}

fn required_i64(value: &Value, key: &str, label: &str) -> Result<i64, ContextFetchError> {
    required_string(value, key, label)?
        .replace(',', "")
        .parse::<i64>()
        .map_err(|_| ContextFetchError::unavailable(format!("{label} is invalid")))
}

fn required_decimal(
    value: &Value,
    key: &str,
    label: &str,
) -> Result<(i128, u8), ContextFetchError> {
    let raw = value.get(key).and_then(|value| {
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_i64().map(|value| value.to_string()))
    });
    raw.as_deref()
        .and_then(parse_decimal)
        .ok_or_else(|| ContextFetchError::unavailable(format!("{label} is invalid")))
}

fn parse_decimal(value: &str) -> Option<(i128, u8)> {
    let value = value.trim();
    let (negative, unsigned) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if whole.is_empty()
        || !whole.chars().all(|value| value.is_ascii_digit())
        || !fraction.chars().all(|value| value.is_ascii_digit())
        || fraction.len() > 9
    {
        return None;
    }
    let scale = u8::try_from(fraction.len()).ok()?;
    let joined = format!("{whole}{fraction}");
    let units = joined.parse::<i128>().ok()?;
    Some((if negative { -units } else { units }, scale))
}

fn parse_date_prefix(value: &str, label: &str) -> Result<i64, ContextFetchError> {
    let date = value.get(..10).unwrap_or(value);
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc().timestamp())
        .ok_or_else(|| ContextFetchError::unavailable(format!("{label} is invalid")))
}

fn parse_named_month_datetime(year: i32, date: &str, time: &str) -> Result<NaiveDateTime, String> {
    let combined = format!("{} {date} {}", year, time.replace('.', ""));
    for format in ["%Y %B %d %I:%M %p", "%Y %B %e %I:%M %p"] {
        if let Ok(value) = NaiveDateTime::parse_from_str(&combined, format) {
            return Ok(value);
        }
    }
    Err("official release timestamp is invalid".to_string())
}

fn eastern_timestamp(local: NaiveDateTime) -> Option<i64> {
    let offset_seconds = if is_us_eastern_dst(local.date()) {
        4 * 60 * 60
    } else {
        5 * 60 * 60
    };
    FixedOffset::west_opt(offset_seconds)?
        .from_local_datetime(&local)
        .single()
        .map(|time| time.timestamp())
}

fn is_us_eastern_dst(date: NaiveDate) -> bool {
    let Some(march_start) = nth_weekday(date.year(), 3, Weekday::Sun, 2) else {
        return false;
    };
    let Some(november_end) = nth_weekday(date.year(), 11, Weekday::Sun, 1) else {
        return false;
    };
    date >= march_start && date < november_end
}

fn nth_weekday(year: i32, month: u32, weekday: Weekday, ordinal: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let offset = (7 + i64::from(weekday.num_days_from_monday())
        - i64::from(first.weekday().num_days_from_monday()))
        % 7;
    first.checked_add_days(chrono::Days::new(
        u64::try_from(offset).ok()? + 7 * u64::from(ordinal.saturating_sub(1)),
    ))
}

fn importance_for_title(title: &str) -> EventImportance {
    let lower = title.to_ascii_lowercase();
    if [
        "consumer price",
        "employment situation",
        "gross domestic product",
        "gdp",
        "personal income and outlays",
        "producer price",
        "fomc",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        EventImportance::High
    } else if [
        "job openings",
        "international trade",
        "retail sales",
        "productivity",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        EventImportance::Medium
    } else {
        EventImportance::Low
    }
}

fn between<'a>(value: &'a str, start: &str, end: &str) -> Option<&'a str> {
    after(value, start)?.split_once(end).map(|(value, _)| value)
}

fn after<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    value.split_once(marker).map(|(_, value)| value)
}

fn strip_html(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => {
                in_tag = true;
                if output
                    .chars()
                    .next_back()
                    .is_some_and(|value| !value.is_whitespace())
                {
                    output.push(' ');
                }
            }
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn json_rows(value: &Value) -> Option<&[Value]> {
    value.as_array().map(Vec::as_slice).or_else(|| {
        value
            .as_object()
            .and_then(|object| object.values().find_map(Value::as_array))
            .map(Vec::as_slice)
    })
}

fn json_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    let object = value.as_object()?;
    names.iter().find_map(|name| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

fn json_string<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    json_field(value, names).and_then(Value::as_str)
}

fn json_i64(value: &Value, names: &[&str]) -> Option<i64> {
    let value = json_field(value, names)?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn json_decimal(value: &Value, names: &[&str]) -> Option<(i128, u8)> {
    let value = json_field(value, names)?;
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_number().map(ToString::to_string))
        .and_then(|value| parse_decimal(&value))
}

fn parse_publication_timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.timestamp())
        .ok()
        .or_else(|| {
            value
                .get(..10)
                .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|date| date.and_utc().timestamp())
        })
}

fn sum_fixed(values: &[(i128, u8)]) -> Option<(i128, u8)> {
    let scale = values.iter().map(|(_, scale)| *scale).max()?;
    let mut total = 0_i128;
    for (units, value_scale) in values {
        let factor = 10_i128.checked_pow(u32::from(scale.checked_sub(*value_scale)?))?;
        total = total.checked_add(units.checked_mul(factor)?)?;
    }
    Some((total, scale))
}

fn slug(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(96));
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !output.is_empty() {
                output.push('-');
            }
            separator = false;
            output.push(character);
        } else {
            separator = true;
        }
        if output.len() >= 96 {
            break;
        }
    }
    output.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FETCHED: i64 = 1_798_416_000;

    #[test]
    fn bls_ics_preserves_eastern_release_time_and_importance() {
        let fixture = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART;TZID=America/New_York:20261002T083000\r\nSUMMARY:Employment Situation for September 2026\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let publication = parse_bls_calendar(fixture, FETCHED).expect("BLS fixture parses");
        let event = &publication.economic_events[0];
        assert_eq!(event.importance, EventImportance::High);
        assert_eq!(event.scheduled_unix_seconds, 1_790_944_200);
    }

    #[test]
    fn bea_rows_are_parsed_without_carrying_markup() {
        let fixture = r#"<table><tr class="scheduled-releases-type-press"><td><div class="release-date">October 29</div><small class="text-muted">8:30 AM</small></td><td class="release-title views-field views-field-field-scheduled-releases-type">GDP (Advance Estimate), 3rd Quarter 2026</td></tr></table>"#;
        let publication = parse_bea_calendar(fixture, FETCHED).expect("BEA fixture parses");
        assert_eq!(publication.economic_events.len(), 1);
        assert_eq!(
            publication.economic_events[0].importance,
            EventImportance::High
        );
        assert_eq!(
            publication.economic_events[0].title,
            "GDP (Advance Estimate), 3rd Quarter 2026"
        );
    }

    #[test]
    fn fed_calendar_uses_press_conference_dates_as_decision_time() {
        let fixture = r#"<a href="/monetarypolicy/fomcpresconf20261028.htm">Press Conference</a>"#;
        let publication = parse_fed_calendar(fixture, FETCHED).expect("Fed fixture parses");
        assert_eq!(publication.economic_events.len(), 1);
        assert_eq!(
            publication.economic_events[0].importance,
            EventImportance::High
        );
    }

    #[test]
    fn cftc_required_fields_and_categories_are_exact() {
        let fixture = serde_json::json!([{
            "report_date_as_yyyy_mm_dd": "2026-09-22T00:00:00.000",
            "cftc_contract_market_code": "001602",
            "market_and_exchange_names": "WHEAT-SRW - CHICAGO BOARD OF TRADE",
            "open_interest_all": "316244",
            "prod_merc_positions_long": "37353",
            "prod_merc_positions_short": "85943",
            "swap_positions_long_all": "76373",
            "swap__positions_short_all": "16895",
            "swap__positions_spread_all": "14058",
            "m_money_positions_long_all": "65114",
            "m_money_positions_short_all": "84004",
            "m_money_positions_spread": "38692",
            "other_rept_positions_long": "30693",
            "other_rept_positions_short": "12528",
            "other_rept_positions_spread": "25980"
        }]);
        let publication = parse_cftc(&fixture, FETCHED).expect("CFTC fixture parses");
        assert_eq!(publication.commitments.len(), 4);
        assert_eq!(publication.commitments[2].category, "Managed Money");
    }

    #[test]
    fn noaa_cpc_degree_days_preserve_official_issue_and_population_weighting() {
        let heating = "Product: Daily Heating Degree Days Forecast Based on NDFD Data Issued 0000UTC 20260926\nRegions: CPC::Regions::CensusDivisions\nRegion|20260926|20260927|Total\nCONUS|3|4|7\n";
        let cooling = "Product: Daily Cooling Degree Days Forecast Based on NDFD Data Issued 0000UTC 20260926\nRegions: CPC::Regions::CensusDivisions\nRegion|20260926|20260927|Total\nCONUS|5|6|11\n";
        let metric = parse_noaa_degree_days(
            heating,
            cooling,
            "https://ftp.cpc.ncep.noaa.gov/fixture",
            FETCHED,
        )
        .expect("NOAA fixture parses");
        assert_eq!(metric.heating_degree_days_units, 7);
        assert_eq!(metric.cooling_degree_days_units, 11);
        assert_eq!(metric.scale, 0);
        assert_eq!(metric.region, "CONUS population weighted");
        assert_eq!(metric.provenance.release_unix_seconds, 1_790_380_800);
    }

    #[test]
    fn decimal_parser_never_rounds_provider_values() {
        assert_eq!(parse_decimal("-12.340"), Some((-12_340, 3)));
        assert_eq!(parse_decimal("."), None);
        assert_eq!(parse_decimal("1.1234567890"), None);
    }

    #[test]
    fn eia_schedule_generates_standard_releases_and_honors_holiday_overrides() {
        let fixture = r#"<table><tr><th scope="row">September 25, 2026</th>
            <td>October 1, 2026</td><td>Thursday</td><td>12:00 p.m.</td>
            <td>Holiday</td></tr></table>"#;
        let fetched = Utc
            .with_ymd_and_hms(2026, 9, 27, 12, 0, 0)
            .single()
            .expect("fixture timestamp")
            .timestamp();
        let events = parse_eia_release_schedule(fixture, fetched).expect("schedule parses");
        assert_eq!(events.len(), 13);
        assert_eq!(events[0].id, "eia:wpsr:2026-09-30");
        assert_eq!(
            events[0].scheduled_unix_seconds,
            Utc.with_ymd_and_hms(2026, 10, 1, 16, 0, 0)
                .single()
                .expect("override UTC")
                .timestamp()
        );
        assert_eq!(events[0].provenance.release_unix_seconds, fetched);
    }

    #[test]
    fn wasde_release_keeps_official_known_at_time_and_corn_balance_sheet() {
        let landing = r#"<table><tr><td><a href="/sites/default/release-files/796054/wasde0926.txt"><time datetime="2026-09-11T12:00:00Z">Sep 11 2026</time> txt</a></td></tr></table>"#;
        let (released, url) = parse_wasde_landing(landing).expect("landing parses");
        let report = "header\nCORN\nProduction 14892 17021 16013 15800\nExports 2873 3425 3275 3275\nEnding Stocks 1551 1922 1653 1567\n====";
        let publication =
            parse_wasde_report(report, &url, released, FETCHED).expect("report parses");
        assert_eq!(publication.agriculture.len(), 3);
        assert_eq!(publication.agriculture[0].value_units, 15_800);
        assert_eq!(
            publication.agriculture[0].provenance.release_unix_seconds,
            released
        );
    }

    #[test]
    fn fas_export_sales_select_latest_supported_release_and_sum_latest_week() {
        let commodities = serde_json::json!([
            {"commodityCode": 401, "commodityName": "Corn"},
            {"commodityCode": 801, "commodityName": "Soybeans"}
        ]);
        let releases = serde_json::json!([
            {"commodityCode": 401, "marketYear": 2026, "releaseDate": "2026-09-24T12:30:00Z"},
            {"commodityCode": 801, "marketYear": 2026, "releaseDate": "2026-09-24T12:30:00Z"}
        ]);
        let requests = fas_export_requests(&commodities, &releases).expect("release rows parse");
        assert_eq!(requests.len(), 2);
        let exports = serde_json::json!([
            {"weekEndingDate": "2026-09-17", "weeklyExports": "120.5"},
            {"weekEndingDate": "2026-09-17", "weeklyExports": 79.5},
            {"weekEndingDate": "2026-09-10", "weeklyExports": "999"}
        ]);
        let metric = parse_fas_exports(
            &exports,
            &requests[0],
            "https://api.fas.usda.gov/api/esr/exports/fixture",
            FETCHED,
        )
        .expect("exports parse");
        assert_eq!((metric.value_units, metric.value_scale), (2_000, 1));
        assert_eq!(metric.period, "2026-09-17");
    }

    #[test]
    #[ignore = "requires live official BEA, Federal Reserve, NOAA and CFTC public access"]
    fn official_public_sources_publish_current_validated_shapes() {
        let epoch_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock follows Unix epoch")
            .as_secs();
        let now_unix_seconds = i64::try_from(epoch_seconds).expect("timestamp fits i64");
        for source in [
            ContextSource::UsdaWasde,
            ContextSource::Bea,
            ContextSource::FederalReserve,
            ContextSource::Noaa,
            ContextSource::Cftc,
        ] {
            // Isolate source connections so a slow or closed government endpoint cannot poison a
            // pooled connection before the remaining live-shape checks run.
            let client = OfficialContextFetcher::new(Duration::from_secs(20))
                .expect("official source client builds");
            let publication = client
                .fetch(source, None, now_unix_seconds)
                .unwrap_or_else(|error| panic!("{} failed: {}", source.label(), error.detail));
            publication
                .validate()
                .unwrap_or_else(|error| panic!("{} invalid: {error}", source.label()));
        }
    }
}
