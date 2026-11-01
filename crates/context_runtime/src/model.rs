//! Canonical, provider-neutral context data retained by the context owner.

use serde::{Deserialize, Serialize};

/// Maximum items retained for any one context-data collection.
pub const MAXIMUM_CONTEXT_ITEMS: usize = 4_096;

/// Official public-data publishers supported by the context owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    Bls,
    Bea,
    FederalReserve,
    Eia,
    Noaa,
    Cftc,
    Usda,
    UsdaWasde,
    UsdaFas,
    Fred,
}

impl ContextSource {
    pub const ALL: [Self; 10] = [
        Self::Bls,
        Self::Bea,
        Self::FederalReserve,
        Self::Eia,
        Self::Noaa,
        Self::Cftc,
        Self::Usda,
        Self::UsdaWasde,
        Self::UsdaFas,
        Self::Fred,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bls => "BLS",
            Self::Bea => "BEA",
            Self::FederalReserve => "Federal Reserve",
            Self::Eia => "EIA",
            Self::Noaa => "NOAA",
            Self::Cftc => "CFTC",
            Self::Usda => "USDA NASS",
            Self::UsdaWasde => "USDA WASDE",
            Self::UsdaFas => "USDA FAS",
            Self::Fred => "FRED",
        }
    }

    #[must_use]
    pub const fn credential_key(self) -> Option<&'static str> {
        match self {
            Self::Eia => Some("eia_api_key"),
            Self::Usda => Some("usda_api_key"),
            Self::UsdaFas => Some("data_gov_api_key"),
            Self::Fred => Some("fred_api_key"),
            Self::Bls
            | Self::Bea
            | Self::FederalReserve
            | Self::Noaa
            | Self::Cftc
            | Self::UsdaWasde => None,
        }
    }
}

/// Dataset families displayed by the desktop.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDataset {
    EconomicCalendar,
    Energy,
    Weather,
    CommitmentsOfTraders,
    Agriculture,
    Macro,
}

/// Availability never silently substitutes made-up or stale values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Pending,
    Available,
    MissingCredential,
    Unavailable,
}

/// Current health for one official source. Error text is bounded and contains no credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextSourceStatus {
    pub source: ContextSource,
    pub availability: SourceAvailability,
    pub last_attempt_unix_seconds: Option<i64>,
    pub last_success_unix_seconds: Option<i64>,
    pub next_refresh_unix_seconds: Option<i64>,
    pub detail: Option<String>,
}

/// Provenance attached to every public value so historical views cannot look ahead.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextProvenance {
    pub source: ContextSource,
    pub source_url: String,
    pub release_unix_seconds: i64,
    pub fetched_unix_seconds: i64,
}

/// Market relevance used for countdown presentation and risk rules.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventImportance {
    Low,
    Medium,
    High,
}

/// Releases whose title an Aeris adapter writes rather than the publisher.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EconomicEventKind {
    /// The title is the publisher's own release name.
    #[default]
    Published,
    FomcRateDecision,
    EiaWeeklyPetroleumStatusReport,
}

/// One scheduled official economic or policy release.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EconomicEvent {
    pub id: String,
    /// The English title; surfaces localize it from `kind` unless the publisher wrote it.
    pub title: String,
    #[serde(default)]
    pub kind: EconomicEventKind,
    pub scheduled_unix_seconds: i64,
    pub importance: EventImportance,
    pub provenance: ContextProvenance,
}

/// What a statistic measures, for statistics whose label and unit an Aeris adapter writes.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMetricKind {
    /// The label and unit are the publisher's own text.
    #[default]
    Published,
    CommercialCrudeStocks,
    Lower48NaturalGasStorage,
    TenYearTreasuryYield,
    TradeWeightedDollarIndex,
    ConsumerPriceIndex,
    NonfarmPayrolls,
    WasdeCornProduction,
    WasdeCornExports,
    WasdeCornEndingStocks,
    /// Weekly export sales of a commodity the publisher names.
    WeeklyExportSales {
        commodity: String,
    },
}

/// Fixed-point public statistic. `units` carries the exact decimal provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextMetric {
    pub id: String,
    #[serde(default)]
    pub kind: ContextMetricKind,
    /// The English or publisher label; surfaces localize Aeris-written labels from `kind`.
    pub label: String,
    pub period: String,
    pub value_units: i128,
    pub value_scale: u8,
    pub unit: String,
    pub expected_units: Option<i128>,
    pub five_year_min_units: Option<i128>,
    pub five_year_max_units: Option<i128>,
    pub provenance: ContextProvenance,
}

/// Regions a degree-day projection can cover.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegreeDayRegion {
    /// The contiguous United States, weighted by population.
    ConusPopulationWeighted,
}

/// Heating/cooling degree-day projection derived only from released NOAA observations/forecasts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DegreeDayMetric {
    pub region: DegreeDayRegion,
    pub period: String,
    pub heating_degree_days_units: i64,
    pub cooling_degree_days_units: i64,
    pub scale: u8,
    pub provenance: ContextProvenance,
}

/// CFTC disaggregated Commitments of Traders categories.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CotTraderCategory {
    ProducerMerchant,
    SwapDealer,
    ManagedMoney,
    OtherReportable,
}

/// One CFTC positioning row with explicit trader-category provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CotPosition {
    pub market_code: String,
    pub market_name: String,
    pub report_date_unix_seconds: i64,
    pub category: CotTraderCategory,
    pub long_contracts: i64,
    pub short_contracts: i64,
    pub spreading_contracts: Option<i64>,
    pub open_interest: i64,
    pub provenance: ContextProvenance,
}

/// Immutable bounded view published by the single context owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextSnapshot {
    pub revision: u64,
    pub published_unix_seconds: i64,
    pub source_statuses: Vec<ContextSourceStatus>,
    pub economic_events: Vec<EconomicEvent>,
    pub energy: Vec<ContextMetric>,
    pub weather: Vec<DegreeDayMetric>,
    pub commitments: Vec<CotPosition>,
    pub agriculture: Vec<ContextMetric>,
    pub macro_observations: Vec<ContextMetric>,
}

impl ContextSnapshot {
    #[must_use]
    pub fn empty(now_unix_seconds: i64) -> Self {
        Self {
            revision: 0,
            published_unix_seconds: now_unix_seconds,
            source_statuses: ContextSource::ALL
                .into_iter()
                .map(|source| ContextSourceStatus {
                    source,
                    availability: SourceAvailability::Pending,
                    last_attempt_unix_seconds: None,
                    last_success_unix_seconds: None,
                    next_refresh_unix_seconds: None,
                    detail: None,
                })
                .collect(),
            economic_events: Vec::new(),
            energy: Vec::new(),
            weather: Vec::new(),
            commitments: Vec::new(),
            agriculture: Vec::new(),
            macro_observations: Vec::new(),
        }
    }
}

/// One validated result from an official source adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePublication {
    pub source: ContextSource,
    pub economic_events: Vec<EconomicEvent>,
    pub energy: Vec<ContextMetric>,
    pub weather: Vec<DegreeDayMetric>,
    pub commitments: Vec<CotPosition>,
    pub agriculture: Vec<ContextMetric>,
    pub macro_observations: Vec<ContextMetric>,
}

impl SourcePublication {
    #[must_use]
    pub const fn empty(source: ContextSource) -> Self {
        Self {
            source,
            economic_events: Vec::new(),
            energy: Vec::new(),
            weather: Vec::new(),
            commitments: Vec::new(),
            agriculture: Vec::new(),
            macro_observations: Vec::new(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        for provenance in self
            .economic_events
            .iter()
            .map(|value| &value.provenance)
            .chain(self.energy.iter().map(|value| &value.provenance))
            .chain(self.weather.iter().map(|value| &value.provenance))
            .chain(self.commitments.iter().map(|value| &value.provenance))
            .chain(self.agriculture.iter().map(|value| &value.provenance))
            .chain(
                self.macro_observations
                    .iter()
                    .map(|value| &value.provenance),
            )
        {
            if provenance.source != self.source
                || provenance.release_unix_seconds <= 0
                || provenance.fetched_unix_seconds <= 0
                || provenance.release_unix_seconds > provenance.fetched_unix_seconds
                || provenance.source_url.len() > 2_048
                || !provenance.source_url.starts_with("https://")
            {
                return Err("context publication provenance is invalid".to_string());
            }
        }
        let item_count = self.economic_events.len()
            + self.energy.len()
            + self.weather.len()
            + self.commitments.len()
            + self.agriculture.len()
            + self.macro_observations.len();
        if item_count > MAXIMUM_CONTEXT_ITEMS {
            return Err("context publication exceeds its item limit".to_string());
        }
        Ok(())
    }
}
