//! Durable workspace validation, migration, and storage.

use super::{
    BTreeMap, BTreeSet, BarPeriod, BarSeriesKey, CACHE_MANIFEST_REVISION, EngineLifetimeMode,
    Envelope, EnvelopeDecoder, HotSeries, HotSetDescriptor, HotSetEntry, HotSetManager,
    HotSetState, InstallProviderInstrument, InstalledInstrument, MAXIMUM_HOT_SERIES, NonZeroU64,
    NonZeroUsize, OpenOptions, PROTOCOL_VERSION, Path, PathBuf,
    RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID, ResourceMode, SeriesCadence, SeriesKey, SystemTime,
    UNIX_EPOCH, WORKSPACE_SCHEMA_REVISION, WorkspaceId, WorkspaceLayoutState, WorkspacePaneKind,
    WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState, Write,
    encode_envelope, envelope, fs, workspace_layout,
};

pub(super) fn default_workspace() -> WorkspaceState {
    let primary = hyperliquid_hot_series(1, unix_seconds());
    WorkspaceState {
        provider: "hyperliquid".to_string(),
        market: "BTC-PERP".to_string(),
        interval_seconds: 60,
        watchlist: vec!["BTC-PERP".to_string()],
        workspace_revision: 0,
        warm_mode_enabled: true,
        resource_mode: ResourceMode::Warm as i32,
        schema_revision: WORKSPACE_SCHEMA_REVISION,
        cache_manifest_revision: CACHE_MANIFEST_REVISION,
        hot_series: vec![primary.clone()],
        lifetime_mode: EngineLifetimeMode::KeepEngineWarm as i32,
        autostart_enabled: false,
        markets_live_permitted: false,
        layout_generation: 1,
        active_workspace_id: 1,
        workspace_tabs: vec![default_workspace_tab(&primary)],
    }
}

fn default_workspace_tab(series: &HotSeries) -> WorkspaceTabState {
    WorkspaceTabState {
        workspace_id: 1,
        label: "Workspace 1".to_string(),
        split_axis: WorkspaceSplitAxis::Horizontal as i32,
        panes: vec![WorkspacePaneState {
            pane_id: 1,
            consumer_id: 1,
            kind: WorkspacePaneKind::Chart as i32,
            instrument: Some(InstallProviderInstrument {
                provider: series.provider.clone(),
                session_generation: series.provider_watermark.max(1),
                selection_generation: 1,
                instrument_id: series.instrument_id.clone(),
                provider_symbol: series.provider_symbol.clone(),
                display_symbol: series.display_symbol.clone(),
                venue_id: series.venue_id.clone(),
                price_scale: series.price_scale,
                quantity_scale: series.quantity_scale,
                entitlement_id: series.entitlement_id.clone(),
            }),
            series: Some(SeriesKey {
                provider: series.provider.clone(),
                instrument_id: series.instrument_id.clone(),
                cadence_value: series.cadence_value,
                definition_revision: series.definition_revision,
                entitlement_id: series.entitlement_id.clone(),
                cadence: series.cadence,
            }),
            viewport_start_unix_nanos: series.viewport_start_unix_nanos,
            viewport_end_unix_nanos: series.viewport_end_unix_nanos,
            size_basis_points: 10_000,
            generation: 1,
        }],
        active_pane_id: 1,
        generation: 1,
        layout: Some(WorkspaceLayoutState {
            pane_id: 1,
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            ratio_basis_points: 0,
            first: None,
            second: None,
        }),
    }
}

pub(super) fn validate_workspace(workspace: &WorkspaceState) -> Result<(), String> {
    const MAXIMUM_MARKET_BYTES: usize = 128;
    const MAXIMUM_WATCHLIST_ITEMS: usize = 256;
    let mut hot_identities = BTreeSet::new();
    let mut workspace_ids = BTreeSet::new();
    let mut pane_ids = BTreeSet::new();
    let mut consumer_ids = BTreeSet::new();

    if workspace.provider.trim().is_empty()
        || workspace.market.trim().is_empty()
        || workspace.market.len() > MAXIMUM_MARKET_BYTES
        || workspace.interval_seconds == 0
        || workspace.watchlist.len() > MAXIMUM_WATCHLIST_ITEMS
        || workspace
            .watchlist
            .iter()
            .any(|market| market.trim().is_empty() || market.len() > MAXIMUM_MARKET_BYTES)
        || workspace.hot_series.len() > MAXIMUM_HOT_SERIES
        || workspace.hot_series.iter().any(|series| {
            series.provider.trim().is_empty()
                || series.market.trim().is_empty()
                || series.market.len() > MAXIMUM_MARKET_BYTES
                || series.interval_seconds == 0
                || series.score == 0
                || !valid_hot_identity(workspace.schema_revision, series, &mut hot_identities)
                || !valid_viewport(series)
                || !valid_coverage(series)
                || workspace.schema_revision >= 3 && !valid_reconstructable_hot_series(series)
        })
    {
        return Err("workspace state is invalid".to_string());
    }
    if !matches!(workspace.schema_revision, 0..=WORKSPACE_SCHEMA_REVISION)
        || workspace.cache_manifest_revision == u32::MAX
    {
        return Err("workspace revision is unsupported".to_string());
    }
    if ResourceMode::try_from(workspace.resource_mode).is_err() {
        return Err("workspace resource mode is invalid".to_string());
    }
    let lifetime_mode = EngineLifetimeMode::try_from(workspace.lifetime_mode)
        .map_err(|_| "workspace lifetime mode is invalid".to_string())?;
    if lifetime_mode == EngineLifetimeMode::KeepMarketsLive && !workspace.markets_live_permitted {
        return Err("markets-live mode requires explicit permission".to_string());
    }
    if workspace.schema_revision >= 4
        && (workspace.layout_generation == 0
            || workspace.workspace_tabs.is_empty()
            || workspace.workspace_tabs.len() > 8
            || workspace.active_workspace_id == 0
            || workspace.workspace_tabs.iter().any(|tab| {
                tab.workspace_id == 0
                    || !workspace_ids.insert(tab.workspace_id)
                    || tab.label.trim().is_empty()
                    || tab.label.len() > 128
                    || WorkspaceSplitAxis::try_from(tab.split_axis).is_err()
                    || tab.panes.is_empty()
                    || tab.panes.len() > 4
                    || tab.active_pane_id == 0
                    || tab.generation == 0
                    || workspace.schema_revision >= 5
                        && !workspace_layout::layout_matches_panes(tab)
                    || tab
                        .panes
                        .iter()
                        .map(|pane| pane.size_basis_points)
                        .sum::<u32>()
                        != 10_000
                    || !tab
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == tab.active_pane_id)
                    || tab.panes.iter().any(|pane| {
                        pane.pane_id == 0
                            || pane.consumer_id == 0
                            || !pane_ids.insert(pane.pane_id)
                            || !consumer_ids.insert(pane.consumer_id)
                            || pane.generation == 0
                            || pane.size_basis_points == 0
                            || WorkspacePaneKind::try_from(pane.kind).is_err()
                            || !valid_workspace_pane(pane)
                    })
            })
            || !workspace_ids.contains(&workspace.active_workspace_id))
    {
        return Err("workspace layout is invalid".to_string());
    }
    Ok(())
}

fn valid_workspace_pane(pane: &WorkspacePaneState) -> bool {
    let viewport_valid = match (pane.viewport_start_unix_nanos, pane.viewport_end_unix_nanos) {
        (None, None) => true,
        (Some(start), Some(end)) => start < end,
        _ => false,
    };
    let Some(instrument) = pane.instrument.as_ref() else {
        return false;
    };
    let Some(series) = pane.series.as_ref() else {
        return false;
    };
    viewport_valid
        && !instrument.provider.trim().is_empty()
        && instrument.provider == series.provider
        && instrument.instrument_id == series.instrument_id
        && instrument.entitlement_id == series.entitlement_id
        && SeriesCadence::try_from(series.cadence)
            .is_ok_and(|cadence| cadence != SeriesCadence::Unspecified)
        && series.cadence_value > 0
        && series.definition_revision > 0
        && instrument.price_scale <= 18
        && instrument.quantity_scale <= 18
}

pub(super) fn sync_layout_hot_series(workspace: &mut WorkspaceState) {
    for hot in &mut workspace.hot_series {
        hot.workspace_ids.clear();
    }
    let mut next_score = workspace
        .hot_series
        .iter()
        .map(|series| series.score)
        .max()
        .unwrap_or(0);
    for tab in &workspace.workspace_tabs {
        for pane in &tab.panes {
            let (Some(instrument), Some(series)) = (&pane.instrument, &pane.series) else {
                continue;
            };
            if let Some(hot) = workspace.hot_series.iter_mut().find(|hot| {
                hot.provider == series.provider
                    && hot.instrument_id == series.instrument_id
                    && hot.entitlement_id == series.entitlement_id
                    && hot.cadence == series.cadence
                    && hot.cadence_value == series.cadence_value
                    && hot.definition_revision == series.definition_revision
            }) {
                if !hot.workspace_ids.contains(&tab.workspace_id) {
                    hot.workspace_ids.push(tab.workspace_id);
                }
                continue;
            }
            next_score = next_score.saturating_add(1).max(1);
            workspace.hot_series.push(HotSeries {
                provider: series.provider.clone(),
                market: instrument.provider_symbol.clone(),
                interval_seconds: series.cadence_value,
                score: next_score,
                last_used_unix_seconds: unix_seconds(),
                provider_watermark: instrument.session_generation,
                series_watermark: 0,
                viewport_start_unix_nanos: pane.viewport_start_unix_nanos,
                viewport_end_unix_nanos: pane.viewport_end_unix_nanos,
                account_id: provider_account_id(&series.provider).to_string(),
                instrument_id: series.instrument_id.clone(),
                entitlement_id: series.entitlement_id.clone(),
                cadence: series.cadence,
                cadence_value: series.cadence_value,
                definition_revision: series.definition_revision,
                pinned: false,
                workspace_ids: vec![tab.workspace_id],
                coverage_start_unix_nanos: None,
                coverage_end_unix_nanos: None,
                provider_symbol: instrument.provider_symbol.clone(),
                venue_id: instrument.venue_id.clone(),
                display_symbol: instrument.display_symbol.clone(),
                price_scale: instrument.price_scale,
                quantity_scale: instrument.quantity_scale,
            });
        }
    }
    workspace.hot_series.sort_unstable_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.score.cmp(&left.score))
    });
    workspace.hot_series.truncate(MAXIMUM_HOT_SERIES);
}
fn provider_account_id(provider: &str) -> &str {
    if provider == "rithmic" {
        RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID
    } else if provider == "hyperliquid" {
        crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID
    } else {
        "provider-account-unavailable"
    }
}

fn valid_viewport(series: &HotSeries) -> bool {
    match (
        series.viewport_start_unix_nanos,
        series.viewport_end_unix_nanos,
    ) {
        (None, None) => true,
        (Some(start), Some(end)) => start < end,
        _ => false,
    }
}

fn valid_coverage(series: &HotSeries) -> bool {
    match (
        series.coverage_start_unix_nanos,
        series.coverage_end_unix_nanos,
    ) {
        (None, None) => true,
        (Some(start), Some(end)) => start < end,
        _ => false,
    }
}

fn valid_hot_identity(
    schema_revision: u32,
    series: &HotSeries,
    identities: &mut BTreeSet<(String, String, String, i32, u32, u32)>,
) -> bool {
    if schema_revision < 3 {
        return identities.insert((
            series.provider.clone(),
            series.market.clone(),
            String::new(),
            SeriesCadence::FixedSeconds as i32,
            series.interval_seconds,
            0,
        ));
    }
    identities.insert((
        series.provider.clone(),
        series.instrument_id.clone(),
        series.entitlement_id.clone(),
        series.cadence,
        series.cadence_value,
        series.definition_revision,
    ))
}

fn valid_reconstructable_hot_series(series: &HotSeries) -> bool {
    !series.account_id.trim().is_empty()
        && !series.instrument_id.trim().is_empty()
        && !series.entitlement_id.trim().is_empty()
        && !series.provider_symbol.trim().is_empty()
        && !series.venue_id.trim().is_empty()
        && !series.display_symbol.trim().is_empty()
        && SeriesCadence::try_from(series.cadence)
            .is_ok_and(|cadence| cadence != SeriesCadence::Unspecified && series.cadence_value > 0)
        && series.definition_revision > 0
        && !series.workspace_ids.is_empty()
        && series.workspace_ids.len() <= 256
        && series
            .workspace_ids
            .iter()
            .all(|workspace_id| *workspace_id > 0)
        && series.price_scale <= 18
        && series.quantity_scale <= 18
}
pub(super) fn migrate_workspace(workspace: &mut WorkspaceState) -> bool {
    // Both live providers survive migration untouched; anything else was
    // never resolvable and restarts from the Hyperliquid default.
    if workspace.provider != "rithmic" && workspace.provider != "hyperliquid"
        || workspace
            .hot_series
            .iter()
            .any(|series| series.provider != "rithmic" && series.provider != "hyperliquid")
    {
        *workspace = default_workspace();
        return true;
    }
    let mut migrated = false;
    if workspace.schema_revision < 2 {
        workspace.lifetime_mode = if workspace.warm_mode_enabled {
            EngineLifetimeMode::KeepEngineWarm as i32
        } else {
            EngineLifetimeMode::ExitCompletely as i32
        };
        workspace.autostart_enabled = false;
        workspace.markets_live_permitted = false;
        migrated = true;
    }
    if workspace.schema_revision < 4 {
        let primary = workspace.hot_series.first().cloned().unwrap_or_else(|| {
            pending_hot_series(
                &workspace.provider,
                &workspace.market,
                workspace.interval_seconds,
            )
        });
        workspace.layout_generation = 1;
        workspace.active_workspace_id = 1;
        workspace.workspace_tabs = vec![default_workspace_tab(&primary)];
        migrated = true;
    }
    if workspace.schema_revision < 5 {
        workspace_layout::add_native_layouts(&mut workspace.workspace_tabs);
        migrated = true;
    }
    workspace.schema_revision = WORKSPACE_SCHEMA_REVISION;
    if workspace.cache_manifest_revision == 0 {
        workspace.cache_manifest_revision = CACHE_MANIFEST_REVISION;
        migrated = true;
    }
    if workspace.hot_series.is_empty() {
        workspace.hot_series.push(pending_hot_series(
            &workspace.provider,
            &workspace.market,
            workspace.interval_seconds,
        ));
        migrated = true;
    }
    migrated
}

/// Builds one unresolved hot-series entry for a freshly selected market.
///
/// The entry carries the selection identity with pending instrument metadata;
/// catalog resolution replaces it with the installed instrument before any
/// demand is accepted.
fn pending_hot_series(provider: &str, market: &str, interval_seconds: u32) -> HotSeries {
    if provider == "hyperliquid" {
        hyperliquid_hot_series(1, unix_seconds())
    } else {
        rithmic_hot_series(market, interval_seconds, 1, unix_seconds())
    }
}
pub(super) fn touch_hot_series(workspace: &mut WorkspaceState) {
    let identity = (
        &workspace.provider,
        &workspace.market,
        workspace.interval_seconds,
    );
    let now = unix_seconds();
    let next_score = workspace
        .hot_series
        .iter()
        .map(|series| series.score)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    if let Some(series) = workspace.hot_series.iter_mut().find(|series| {
        series.provider == *identity.0
            && series.market == *identity.1
            && series.interval_seconds == identity.2
    }) {
        series.score = next_score;
        series.last_used_unix_seconds = now;
    } else if workspace.provider == "rithmic" {
        workspace.hot_series.push(rithmic_hot_series(
            &workspace.market,
            workspace.interval_seconds,
            next_score,
            now,
        ));
    } else if workspace.provider == "hyperliquid" {
        workspace
            .hot_series
            .push(hyperliquid_hot_series(next_score, now));
    }
    workspace.hot_series.sort_unstable_by(|left, right| {
        right
            .last_used_unix_seconds
            .cmp(&left.last_used_unix_seconds)
            .then_with(|| right.score.cmp(&left.score))
    });
    workspace.hot_series.truncate(MAXIMUM_HOT_SERIES);
}
fn rithmic_hot_series(
    market: &str,
    interval_seconds: u32,
    score: u32,
    last_used_unix_seconds: u64,
) -> HotSeries {
    let symbol = market.trim().to_ascii_uppercase();
    HotSeries {
        provider: "rithmic".to_string(),
        market: symbol.clone(),
        interval_seconds,
        score,
        last_used_unix_seconds,
        provider_watermark: 0,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: RITHMIC_TEST_MARKET_DATA_ACCOUNT_ID.to_string(),
        instrument_id: format!("rithmic-pending:{symbol}"),
        entitlement_id: format!("rithmic-test:pending:{symbol}"),
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: interval_seconds,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: symbol.clone(),
        venue_id: "CME".to_string(),
        display_symbol: symbol,
        price_scale: 2,
        quantity_scale: 0,
    }
}

/// Pending fresh-install entry: Hyperliquid BTC perpetual, one-minute
/// candles, resolved against the public catalog before demand is accepted.
fn hyperliquid_hot_series(score: u32, last_used_unix_seconds: u64) -> HotSeries {
    HotSeries {
        provider: "hyperliquid".to_string(),
        market: "BTC-PERP".to_string(),
        interval_seconds: 60,
        score,
        last_used_unix_seconds,
        provider_watermark: 0,
        series_watermark: 0,
        viewport_start_unix_nanos: None,
        viewport_end_unix_nanos: None,
        account_id: crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ACCOUNT_ID.to_string(),
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        entitlement_id: crate::hyperliquid_realtime::HYPERLIQUID_PUBLIC_ENTITLEMENT_ID.to_string(),
        cadence: SeriesCadence::FixedSeconds as i32,
        cadence_value: 60,
        definition_revision: 1,
        pinned: false,
        workspace_ids: vec![1],
        coverage_start_unix_nanos: None,
        coverage_end_unix_nanos: None,
        provider_symbol: "BTC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        display_symbol: "BTC-PERP".to_string(),
        price_scale: 8,
        quantity_scale: 8,
    }
}

pub(super) fn empty_hot_set_state() -> HotSetState {
    HotSetState {
        manager: HotSetManager::new(
            NonZeroUsize::new(MAXIMUM_HOT_SERIES).unwrap_or(NonZeroUsize::MIN),
        ),
        consumers: BTreeMap::new(),
        instruments: BTreeMap::new(),
    }
}

pub(super) fn hot_set_state(workspace: &WorkspaceState) -> Result<HotSetState, String> {
    let mut state = empty_hot_set_state();
    let entries = workspace
        .hot_series
        .iter()
        .map(protocol_hot_entry)
        .collect::<Result<Vec<_>, _>>()?;
    state
        .manager
        .restore(entries)
        .map_err(|error| error.to_string())?;
    for series in &workspace.hot_series {
        let price_scale = u8::try_from(series.price_scale)
            .map_err(|_| "hot-set price precision is invalid".to_string())?;
        let quantity_scale = u8::try_from(series.quantity_scale)
            .map_err(|_| "hot-set quantity precision is invalid".to_string())?;
        state.instruments.insert(
            (series.provider.clone(), series.instrument_id.clone()),
            InstalledInstrument {
                account_id: series.account_id.clone(),
                provider_symbol: series.provider_symbol.clone(),
                venue_id: series.venue_id.clone(),
                display_symbol: series.display_symbol.clone(),
                price_scale,
                quantity_scale,
                entitlement_id: series.entitlement_id.clone(),
            },
        );
    }
    Ok(state)
}

pub(super) fn protocol_hot_entry(series: &HotSeries) -> Result<HotSetEntry, String> {
    let canonical = canonical_series(&SeriesKey {
        provider: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        cadence_value: series.cadence_value,
        definition_revision: series.definition_revision,
        entitlement_id: series.entitlement_id.clone(),
        cadence: series.cadence,
    })?;
    let workspaces = series
        .workspace_ids
        .iter()
        .map(|workspace_id| {
            NonZeroU64::new(*workspace_id)
                .map(WorkspaceId)
                .ok_or_else(|| "hot-set workspace identity is invalid".to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let price_scale = u8::try_from(series.price_scale)
        .map_err(|_| "hot-set price precision is invalid".to_string())?;
    let quantity_scale = u8::try_from(series.quantity_scale)
        .map_err(|_| "hot-set quantity precision is invalid".to_string())?;
    Ok(HotSetEntry {
        descriptor: HotSetDescriptor {
            series: canonical,
            account_id: series.account_id.clone(),
            provider_symbol: series.provider_symbol.clone(),
            venue_id: series.venue_id.clone(),
            display_symbol: series.display_symbol.clone(),
            price_scale,
            quantity_scale,
        },
        workspaces,
        pinned: series.pinned,
        score: u64::from(series.score),
        last_used_unix_seconds: series.last_used_unix_seconds,
        provider_watermark: series.provider_watermark,
        series_watermark: series.series_watermark,
        viewport: series
            .viewport_start_unix_nanos
            .zip(series.viewport_end_unix_nanos),
        coverage: series
            .coverage_start_unix_nanos
            .zip(series.coverage_end_unix_nanos),
    })
}

pub(super) fn hot_entry_to_protocol(entry: HotSetEntry) -> HotSeries {
    let (cadence, cadence_value) = protocol_period(entry.descriptor.series.period);
    HotSeries {
        provider: entry.descriptor.series.provider_id,
        market: entry.descriptor.display_symbol.clone(),
        interval_seconds: cadence_value,
        score: u32::try_from(entry.score).unwrap_or(u32::MAX),
        last_used_unix_seconds: entry.last_used_unix_seconds,
        provider_watermark: entry.provider_watermark,
        series_watermark: entry.series_watermark,
        viewport_start_unix_nanos: entry.viewport.map(|viewport| viewport.0),
        viewport_end_unix_nanos: entry.viewport.map(|viewport| viewport.1),
        account_id: entry.descriptor.account_id,
        instrument_id: entry.descriptor.series.instrument_id,
        entitlement_id: entry.descriptor.series.entitlement_id,
        cadence: cadence as i32,
        cadence_value,
        definition_revision: entry.descriptor.series.definition_version,
        pinned: entry.pinned,
        workspace_ids: entry
            .workspaces
            .into_iter()
            .map(|workspace| workspace.0.get())
            .collect(),
        coverage_start_unix_nanos: entry.coverage.map(|coverage| coverage.0),
        coverage_end_unix_nanos: entry.coverage.map(|coverage| coverage.1),
        provider_symbol: entry.descriptor.provider_symbol,
        venue_id: entry.descriptor.venue_id,
        display_symbol: entry.descriptor.display_symbol,
        price_scale: u32::from(entry.descriptor.price_scale),
        quantity_scale: u32::from(entry.descriptor.quantity_scale),
    }
}

pub(super) fn canonical_series(series: &SeriesKey) -> Result<BarSeriesKey, String> {
    if series.provider.trim().is_empty()
        || series.instrument_id.trim().is_empty()
        || series.entitlement_id.trim().is_empty()
        || series.definition_revision == 0
    {
        return Err("hot-set series identity is invalid".to_string());
    }
    let period = match SeriesCadence::try_from(series.cadence)
        .map_err(|_| "hot-set series cadence is invalid".to_string())?
    {
        SeriesCadence::FixedSeconds => BarPeriod::time(series.cadence_value),
        SeriesCadence::Trades => BarPeriod::tick(series.cadence_value),
        SeriesCadence::SessionDays => BarPeriod::session(series.cadence_value),
        SeriesCadence::CalendarWeeks => BarPeriod::week(series.cadence_value),
        SeriesCadence::CalendarMonths => BarPeriod::month(series.cadence_value),
        SeriesCadence::Unspecified => {
            Err(axiusflow_market_data::MarketDataValidationError::InvalidPeriod)
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(BarSeriesKey {
        provider_id: series.provider.clone(),
        instrument_id: series.instrument_id.clone(),
        entitlement_id: series.entitlement_id.clone(),
        period,
        definition_version: series.definition_revision,
    })
}

const fn protocol_period(period: BarPeriod) -> (SeriesCadence, u32) {
    match period {
        BarPeriod::Time { seconds } => (SeriesCadence::FixedSeconds, seconds),
        BarPeriod::Tick { trades } => (SeriesCadence::Trades, trades),
        BarPeriod::Session { days } => (SeriesCadence::SessionDays, days),
        BarPeriod::Week { weeks } => (SeriesCadence::CalendarWeeks, weeks),
        BarPeriod::Month { months } => (SeriesCadence::CalendarMonths, months),
    }
}

pub(super) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn workspace_filename(revision: u64) -> String {
    format!("workspace-{revision:020}.frame")
}

fn hot_set_filename(revision: u32) -> String {
    format!("hot-set-{revision:010}.frame")
}

pub(super) fn persist_workspace(root: &Path, workspace: &WorkspaceState) -> Result<(), String> {
    validate_workspace(workspace)?;
    let path = root.join(workspace_filename(workspace.workspace_revision));
    if path.exists() {
        return Ok(());
    }
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::WorkspaceState(workspace.clone())),
    })
    .map_err(|_| "workspace state could not be encoded".to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(redacted_workspace_error)?;
    file.write_all(&bytes).map_err(redacted_workspace_error)?;
    file.sync_all().map_err(redacted_workspace_error)
}

pub(super) fn load_latest_workspace(root: &Path) -> Result<Option<WorkspaceState>, String> {
    let mut candidates = fs::read_dir(root)
        .map_err(redacted_workspace_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let revision = name
                .strip_prefix("workspace-")?
                .strip_suffix(".frame")?
                .parse::<u64>()
                .ok()?;
            Some((revision, entry.path()))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    for (revision, path) in candidates {
        match decode_workspace_file(&path, revision) {
            Ok(workspace) => return Ok(Some(workspace)),
            Err(()) => quarantine_workspace_file(&path)?,
        }
    }
    Ok(None)
}

pub(super) fn persist_hot_set(root: &Path, workspace: &WorkspaceState) -> Result<(), String> {
    validate_workspace(workspace)?;
    let path = root.join(hot_set_filename(workspace.cache_manifest_revision));
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::WorkspaceState(workspace.clone())),
    })
    .map_err(|_| "workspace hot set could not be encoded".to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(redacted_workspace_error)?;
    file.write_all(&bytes).map_err(redacted_workspace_error)?;
    file.sync_all().map_err(redacted_workspace_error)?;
    for (_, stale) in hot_set_files(root)?.into_iter().skip(2) {
        fs::remove_file(stale).map_err(redacted_workspace_error)?;
    }
    Ok(())
}

pub(super) fn load_latest_hot_set(root: &Path) -> Result<Option<WorkspaceState>, String> {
    for (revision, path) in hot_set_files(root)? {
        match decode_hot_set_file(&path, revision) {
            Ok(workspace) => return Ok(Some(workspace)),
            Err(()) => quarantine_workspace_file(&path)?,
        }
    }
    Ok(None)
}

fn hot_set_files(root: &Path) -> Result<Vec<(u32, PathBuf)>, String> {
    let mut files = fs::read_dir(root)
        .map_err(redacted_workspace_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let revision = name
                .strip_prefix("hot-set-")?
                .strip_suffix(".frame")?
                .parse::<u32>()
                .ok()?;
            Some((revision, entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_unstable_by_key(|(revision, _)| std::cmp::Reverse(*revision));
    Ok(files)
}

fn decode_hot_set_file(path: &Path, revision: u32) -> Result<WorkspaceState, ()> {
    let workspace = decode_workspace_payload(path)?;
    if workspace.cache_manifest_revision != revision || validate_workspace(&workspace).is_err() {
        return Err(());
    }
    Ok(workspace)
}

fn decode_workspace_file(path: &Path, revision: u64) -> Result<WorkspaceState, ()> {
    let workspace = decode_workspace_payload(path)?;
    if workspace.workspace_revision != revision || validate_workspace(&workspace).is_err() {
        return Err(());
    }
    Ok(workspace)
}

fn decode_workspace_payload(path: &Path) -> Result<WorkspaceState, ()> {
    let bytes = fs::read(path).map_err(|_| ())?;
    let mut decoder = EnvelopeDecoder::try_new().map_err(|_| ())?;
    let mut envelopes = decoder.push(&bytes).map_err(|_| ())?;
    if envelopes.len() != 1 {
        return Err(());
    }
    let Some(envelope::Payload::WorkspaceState(workspace)) =
        envelopes.pop().and_then(|item| item.payload)
    else {
        return Err(());
    };
    Ok(workspace)
}

fn quarantine_workspace_file(path: &Path) -> Result<(), String> {
    for suffix in 0_u16..=u16::MAX {
        let quarantine = path.with_extension(format!("corrupt-{suffix}"));
        if !quarantine.exists() {
            return fs::rename(path, quarantine).map_err(redacted_workspace_error);
        }
    }
    Err("corrupt workspace state could not be quarantined".to_string())
}

pub(super) fn redacted_workspace_error<E>(_error: E) -> String {
    "engine workspace storage is unavailable".to_string()
}
