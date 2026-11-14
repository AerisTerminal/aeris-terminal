//! Session-anchored market profiles: the TPO (market profile) chart type and the session or
//! composite volume profile.
//!
//! Aeris Charts computes, bounds and draws both from the product price bars. The TPO is Aeris
//! Charts' TPO series, which replaces the candles while the chart type is TPO; the session volume
//! profile is a periodic profile presentation over the bars. The host supplies only the trading
//! days of the instrument's calendar, a row size on the instrument's price grid and the theme
//! colors. Neither enters the chart's undo history; a settings, theme, chart type or session
//! change replaces what is drawn.

use super::*;
use crate::engine_bridge::ProductPriceBars;
use aeris_charts_engine::{
    MAX_PROFILE_PERIODS, NativePrimitiveId, PeriodicProfilePresentationOptions,
    PeriodicProfilePresentationRequest, ProfileDisplayMode, ProfileError, ProfileSource,
    ResampleBoundary, TpoCellMode, TpoRequest, TpoSeriesOptions,
};
use std::ops::RangeInclusive;
use std::rc::Rc;

/// Trading days a TPO chart covers at most, newest first.
const MAXIMUM_TPO_SESSIONS: usize = 120;
/// Session or composite volume profiles drawn at most, newest first.
const MAXIMUM_SESSION_VOLUME_PROFILES: usize = 200;
/// Rows the widest TPO session spans before each row widens to several ticks.
const TPO_TARGET_ROWS: f64 = 80.0;
/// How far past the newest bar the calendar is read, so the next session open is known across
/// a weekend or a holiday.
const LOOKAHEAD_SECONDS: i64 = 8 * 86_400;
const LOOKAHEAD_SESSIONS: usize = 16;
/// Rows Aeris Charts reserves across every periodic profile on a chart (its crate-private
/// `MAX_PERIODIC_PRESENTATION_ROWS`); a refused request falls back to fewer sessions.
const PERIODIC_PROFILE_ROW_BUDGET: usize = 32_768;
/// The product price series, which the TPO chart type turns into Aeris Charts' TPO series.
const PRICE_SERIES: u32 = 0;

/// One trading day of the chart instrument, `[open, close)` in UTC seconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartTradingSession {
    pub open_unix_seconds: i64,
    pub close_unix_seconds: i64,
}

/// The chart instrument's trading-day calendar, supplied by the host from its provider-neutral
/// market calendar. Session profiles start a new profile at each trading day it returns.
pub trait TradingSessionCalendar {
    /// Trading days overlapping `[from_unix_seconds, to_unix_seconds)`, oldest first and
    /// non-overlapping, at most the newest `limit`.
    fn sessions(
        &self,
        from_unix_seconds: i64,
        to_unix_seconds: i64,
        limit: usize,
    ) -> Vec<ChartTradingSession>;
}

/// Which session profile indicator an item names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionProfileKind {
    /// Volume at price per session, or per composite of several sessions.
    Volume,
}

impl SessionProfileKind {
    /// The indicator catalog entry of this profile.
    #[must_use]
    pub const fn indicator(self) -> ChartIndicator {
        match self {
            Self::Volume => ChartIndicator::SessionVolumeProfile,
        }
    }

    /// The session profile an indicator is, if it is one.
    #[must_use]
    pub const fn of(indicator: ChartIndicator) -> Option<Self> {
        match indicator {
            ChartIndicator::SessionVolumeProfile => Some(Self::Volume),
            _ => None,
        }
    }

    const fn slot(self) -> usize {
        match self {
            Self::Volume => 0,
        }
    }

    fn legend_title(self) -> &'static str {
        match self {
            Self::Volume => tr!(chart_ui::LEGEND_SESSION_VOLUME_PROFILE),
        }
    }
}

/// How TPO prints are drawn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TpoCells {
    /// One letter per period, `A` for the first.
    #[default]
    Letters,
    /// One solid block per period.
    Blocks,
}

/// Durable TPO chart settings. Rows follow the instrument's price grid, widened only as far as
/// a session's range needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TpoSettings {
    pub cells: TpoCells,
    /// Minutes each letter covers.
    pub period_minutes: u16,
    pub value_area_percent: u8,
    /// Periods from the session open that form the initial balance.
    pub initial_balance_periods: u8,
}

impl TpoSettings {
    /// Period lengths the settings accept, in minutes. A 24-hour session of the shortest
    /// period stays inside Aeris Charts' 1,024 periods per profile.
    pub const PERIOD_MINUTES: RangeInclusive<u16> = 5..=240;
    pub const VALUE_AREA_PERCENTS: RangeInclusive<u8> = 50..=95;
    pub const INITIAL_BALANCE_PERIODS: RangeInclusive<u8> = 1..=12;

    /// Whether every bounded value is inside its accepted range.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        Self::PERIOD_MINUTES.contains(&self.period_minutes)
            && Self::VALUE_AREA_PERCENTS.contains(&self.value_area_percent)
            && Self::INITIAL_BALANCE_PERIODS.contains(&self.initial_balance_periods)
    }
}

impl Default for TpoSettings {
    fn default() -> Self {
        Self {
            cells: TpoCells::Letters,
            period_minutes: 30,
            value_area_percent: 70,
            initial_balance_periods: 2,
        }
    }
}

/// Durable session volume profile settings. Volume is spread over each bar's range, so the
/// profile approximates the traded volume at each price.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionVolumeProfileSettings {
    /// Trading days merged into one profile: 1 draws a profile per session, more draws
    /// composites, aligned so the newest composite ends with the newest session.
    pub sessions_per_profile: u8,
    pub row_count: u16,
    pub value_area_percent: u8,
    /// The developing point of control and value area through each profile.
    pub show_developing: bool,
    /// Points of control and value-area edges extended until later prices touch them.
    pub extend_naked_levels: bool,
}

impl SessionVolumeProfileSettings {
    pub const SESSIONS_PER_PROFILE: RangeInclusive<u8> = 1..=20;
    pub const ROW_COUNTS: RangeInclusive<u16> = 12..=240;
    pub const VALUE_AREA_PERCENTS: RangeInclusive<u8> = 50..=95;

    /// Whether every bounded value is inside its accepted range.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        Self::SESSIONS_PER_PROFILE.contains(&self.sessions_per_profile)
            && Self::ROW_COUNTS.contains(&self.row_count)
            && Self::VALUE_AREA_PERCENTS.contains(&self.value_area_percent)
    }

    /// Trading days drawn at most: bounded by the profile count and by the rows Aeris Charts
    /// reserves for periodic profiles.
    fn session_limit(self) -> usize {
        let per_profile = usize::from(self.sessions_per_profile);
        let profiles = MAXIMUM_SESSION_VOLUME_PROFILES
            .min(PERIODIC_PROFILE_ROW_BUDGET / usize::from(self.row_count).max(1));
        profiles
            .saturating_mul(per_profile)
            .min(MAX_PROFILE_PERIODS)
    }
}

impl Default for SessionVolumeProfileSettings {
    fn default() -> Self {
        Self {
            sessions_per_profile: 1,
            row_count: 48,
            value_area_percent: 70,
            show_developing: false,
            extend_naked_levels: false,
        }
    }
}

/// The settings of one session profile indicator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionProfileSettings {
    Volume(SessionVolumeProfileSettings),
}

impl SessionProfileSettings {
    /// The default settings of a profile kind.
    #[must_use]
    pub fn default_for(kind: SessionProfileKind) -> Self {
        match kind {
            SessionProfileKind::Volume => Self::Volume(SessionVolumeProfileSettings::default()),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> SessionProfileKind {
        match self {
            Self::Volume(_) => SessionProfileKind::Volume,
        }
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Volume(settings) => settings.is_valid(),
        }
    }

    fn session_limit(self) -> usize {
        match self {
            Self::Volume(settings) => settings.session_limit(),
        }
    }
}

#[derive(Clone, Debug)]
struct DrawnProfile {
    primitive: NativePrimitiveId,
    /// Aeris Charts has no getter for a drawn primitive's state, so tests read the presentation
    /// back by querying the request it was drawn from.
    #[cfg(test)]
    request: PeriodicProfilePresentationRequest,
}

#[derive(Clone, Debug)]
struct SessionProfile {
    settings: SessionProfileSettings,
    visible: bool,
    drawn: Option<DrawnProfile>,
    /// A bar at or after this time (UTC seconds) opens a session the drawn profile lacks.
    refresh_at: Option<f64>,
}

/// The TPO chart type's settings and what Aeris Charts' TPO series was last given.
#[derive(Clone, Debug, Default)]
struct TpoChart {
    settings: TpoSettings,
    /// Whether the TPO series has at least one session to draw.
    drawn: bool,
    /// A bar at or after this time (UTC seconds) opens a session the TPO series lacks.
    refresh_at: Option<f64>,
}

/// The chart's session profiles, its TPO chart settings and the calendar they are drawn from.
#[derive(Default)]
pub(super) struct SessionProfiles {
    calendar: Option<Rc<dyn TradingSessionCalendar>>,
    profiles: [Option<SessionProfile>; 1],
    tpo: TpoChart,
}

impl SessionProfiles {
    const fn get(&self, kind: SessionProfileKind) -> Option<&SessionProfile> {
        self.profiles[kind.slot()].as_ref()
    }

    const fn get_mut(&mut self, kind: SessionProfileKind) -> Option<&mut SessionProfile> {
        self.profiles[kind.slot()].as_mut()
    }
}

const SESSION_PROFILE_KINDS: [SessionProfileKind; 1] = [SessionProfileKind::Volume];

impl AerisChartView {
    /// Installs the chart instrument's trading-day calendar, or removes it, and redraws the
    /// session profiles and the TPO from it.
    pub fn set_trading_session_calendar(
        &mut self,
        calendar: Option<Rc<dyn TradingSessionCalendar>>,
    ) {
        self.session_profiles.calendar = calendar;
        self.redraw_session_profiles();
    }

    /// The TPO chart type's settings. They are kept while another chart type shows.
    #[must_use]
    pub const fn tpo_settings(&self) -> TpoSettings {
        self.session_profiles.tpo.settings
    }

    /// Replaces the TPO chart type's settings. Returns whether the chart changed: `false` when
    /// the settings are unchanged or out of range.
    pub fn set_tpo_settings(&mut self, settings: TpoSettings) -> bool {
        if !settings.is_valid() || self.session_profiles.tpo.settings == settings {
            return false;
        }
        self.session_profiles.tpo.settings = settings;
        self.draw_tpo_series();
        self.mark_user_state_changed();
        true
    }

    /// Whether Aeris Charts draws a TPO now: the chart type is TPO and the calendar has a
    /// trading day under the bars.
    #[must_use]
    pub const fn tpo_drawn(&self) -> bool {
        self.session_profiles.tpo.drawn
    }

    /// The settings of a session profile on the chart.
    #[must_use]
    pub fn session_profile_settings(
        &self,
        kind: SessionProfileKind,
    ) -> Option<SessionProfileSettings> {
        self.session_profiles
            .get(kind)
            .map(|profile| profile.settings)
    }

    /// Replaces the settings of a session profile on the chart, keeping whether it shows.
    /// Returns whether the chart changed: `false` when that profile is not on the chart or the
    /// settings are unchanged or out of range.
    pub fn set_session_profile_settings(&mut self, settings: SessionProfileSettings) -> bool {
        if !settings.is_valid() {
            return false;
        }
        let kind = settings.kind();
        let Some(profile) = self.session_profiles.get_mut(kind) else {
            return false;
        };
        if profile.settings == settings {
            return false;
        }
        profile.settings = settings;
        self.draw_session_profile(kind);
        self.mark_user_state_changed();
        true
    }

    /// Whether Aeris Charts draws the session profile now: it is on the chart and shown, the
    /// chart type draws OHLC bars, and the calendar has a trading day under the bars.
    #[must_use]
    pub fn session_profile_drawn(&self, kind: SessionProfileKind) -> bool {
        self.session_profiles
            .get(kind)
            .is_some_and(|profile| profile.drawn.is_some())
    }

    pub(super) fn add_session_profile(
        &mut self,
        settings: SessionProfileSettings,
        visible: bool,
    ) -> Result<(), ChartIndicatorError> {
        let kind = settings.kind();
        if !settings.is_valid() || self.session_profiles.get(kind).is_some() {
            return Err(ChartIndicatorError::CreationRejected(kind.indicator()));
        }
        self.session_profiles.profiles[kind.slot()] = Some(SessionProfile {
            settings,
            visible,
            drawn: None,
            refresh_at: None,
        });
        self.draw_session_profile(kind);
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        Ok(())
    }

    pub(super) fn session_profile_states(&self) -> impl Iterator<Item = ChartIndicatorState> + '_ {
        SESSION_PROFILE_KINDS.into_iter().filter_map(|kind| {
            self.session_profiles
                .get(kind)
                .map(|profile| ChartIndicatorState {
                    indicator: kind.indicator(),
                    visible: profile.visible,
                    volume: None,
                    session_profile: Some(profile.settings),
                })
        })
    }

    /// The legend rows of the session profiles. They follow the durable settings rather than
    /// the drawn presentation, so a profile with no session to draw can still be edited.
    pub(super) fn session_profile_legend_rows(
        &self,
    ) -> impl Iterator<Item = (LegendItem, &'static str, bool)> + '_ {
        SESSION_PROFILE_KINDS.into_iter().filter_map(|kind| {
            self.session_profiles.get(kind).map(|profile| {
                (
                    LegendItem::SessionProfile(kind),
                    kind.legend_title(),
                    profile.visible,
                )
            })
        })
    }

    pub(super) fn has_session_profiles(&self) -> bool {
        self.session_profiles.profiles.iter().any(Option::is_some)
    }

    pub(super) fn set_session_profile_visible(
        &mut self,
        kind: SessionProfileKind,
        visible: bool,
    ) -> bool {
        let Some(profile) = self.session_profiles.get_mut(kind) else {
            return false;
        };
        if profile.visible == visible {
            return false;
        }
        profile.visible = visible;
        self.draw_session_profile(kind);
        self.mark_user_state_changed();
        true
    }

    pub(super) fn remove_session_profile(&mut self, kind: SessionProfileKind) -> bool {
        let Some(profile) = self.session_profiles.profiles[kind.slot()].take() else {
            return false;
        };
        if let Some(drawn) = profile.drawn {
            self.remove_drawn_session_profile(&drawn);
        }
        self.invalidate_series_layout();
        self.mark_user_state_changed();
        true
    }

    pub(super) fn remove_every_session_profile(&mut self) -> bool {
        let mut removed = false;
        for kind in SESSION_PROFILE_KINDS {
            removed |= self.remove_session_profile(kind);
        }
        removed
    }

    /// Redraws the TPO and every session profile from the current bars, chart type, calendar
    /// and theme.
    pub(super) fn redraw_session_profiles(&mut self) {
        self.draw_tpo_series();
        for kind in SESSION_PROFILE_KINDS {
            self.draw_session_profile(kind);
        }
    }

    /// Redraws what the appended bars have carried into a session it does not have yet.
    pub(super) fn refresh_session_profiles_after_append(&mut self) {
        let Some(newest) = self.product_bars.last_time() else {
            return;
        };
        let opened = |refresh_at: Option<f64>| refresh_at.is_some_and(|at| newest >= at);
        if opened(self.session_profiles.tpo.refresh_at) {
            self.draw_tpo_series();
        }
        for kind in SESSION_PROFILE_KINDS {
            if opened(
                self.session_profiles
                    .get(kind)
                    .and_then(|profile| profile.refresh_at),
            ) {
                self.draw_session_profile(kind);
            }
        }
    }

    /// Gives Aeris Charts' TPO series its rules, colors and the instrument's trading days.
    /// Only the TPO chart type has that series; another type draws nothing here.
    fn draw_tpo_series(&mut self) {
        let tpo = &mut self.session_profiles.tpo;
        tpo.drawn = false;
        tpo.refresh_at = None;
        let settings = tpo.settings;
        if self.chart_type != ChartType::Tpo {
            return;
        }
        let projected = self.session_profiles.calendar.clone().and_then(|calendar| {
            project_sessions(calendar.as_ref(), &self.product_bars, MAXIMUM_TPO_SESSIONS)
        });
        let sessions = projected
            .as_ref()
            .map_or(&[][..], |projected| &projected.sessions[..]);
        let boundaries = tpo_row_size(self.price_tick_size(), sessions, &self.product_bars)
            .map_or_else(Vec::new, |row_size| {
                self.configure_tpo_series(settings, row_size, sessions)
            });
        let drawn = !boundaries.is_empty();
        if let Err(error) = self.engine.set_tpo_sessions(PRICE_SERIES, boundaries) {
            diagnostic!("Aeris Charts refused the TPO sessions: {error}");
        }
        let tpo = &mut self.session_profiles.tpo;
        tpo.drawn = drawn;
        tpo.refresh_at = projected.and_then(|projected| projected.next_open);
        self.invalidate_series_frame();
    }

    /// Configures the TPO series and returns the boundaries of the newest sessions that fit
    /// Aeris Charts' profile bounds; dense history keeps the newest half until they fit.
    fn configure_tpo_series(
        &mut self,
        settings: TpoSettings,
        row_size: f64,
        sessions: &[ChartTradingSession],
    ) -> Vec<ResampleBoundary> {
        let options = tpo_series_options(settings, row_size, &self.platform);
        if let Err(error) = self.engine.configure_tpo_series(PRICE_SERIES, options) {
            diagnostic!("Aeris Charts refused the TPO settings: {error}");
            return Vec::new();
        }
        let mut count = sessions.len();
        while count > 0 {
            let boundaries = session_boundaries(&sessions[sessions.len() - count..], 1);
            let request = TpoRequest {
                price_series: PRICE_SERIES,
                boundaries,
                period_seconds: u32::from(settings.period_minutes) * 60,
                tick_size: row_size,
                value_area_percent: f64::from(settings.value_area_percent),
                initial_balance_periods: u16::from(settings.initial_balance_periods),
            };
            match self.engine.tpo_profiles(&request) {
                Ok(_) => return request.boundaries,
                Err(ProfileError::LimitExceeded) => count /= 2,
                Err(error) => {
                    diagnostic!("Aeris Charts refused the TPO sessions: {error}");
                    return Vec::new();
                }
            }
        }
        Vec::new()
    }

    fn remove_drawn_session_profile(&mut self, drawn: &DrawnProfile) {
        let removed = self.engine.remove_native_primitive(drawn.primitive);
        debug_assert!(
            removed,
            "a drawn session profile stays on the product price series"
        );
        self.invalidate_series_frame();
    }

    fn draw_session_profile(&mut self, kind: SessionProfileKind) {
        let Some(profile) = self.session_profiles.get_mut(kind) else {
            return;
        };
        let previous = profile.drawn.take();
        profile.refresh_at = None;
        let (settings, visible) = (profile.settings, profile.visible);
        if let Some(previous) = previous {
            self.remove_drawn_session_profile(&previous);
        }
        // Aeris Charts reads the price series' own OHLC columns, which line, area and footprint
        // presentations do not carry.
        if !visible || !self.chart_type.draws_ohlc_bars() {
            return;
        }
        let Some(calendar) = self.session_profiles.calendar.clone() else {
            return;
        };
        let Some(projected) = project_sessions(
            calendar.as_ref(),
            &self.product_bars,
            settings.session_limit(),
        ) else {
            return;
        };
        let drawn = if projected.sessions.is_empty() {
            None
        } else {
            match settings {
                SessionProfileSettings::Volume(settings) => {
                    self.install_session_volume_profile(settings, &projected.sessions)
                }
            }
        };
        if let Some(profile) = self.session_profiles.get_mut(kind) {
            profile.drawn = drawn;
            profile.refresh_at = projected.next_open;
        }
        self.invalidate_series_frame();
    }

    fn install_session_volume_profile(
        &mut self,
        settings: SessionVolumeProfileSettings,
        sessions: &[ChartTradingSession],
    ) -> Option<DrawnProfile> {
        let options = session_volume_profile_options(settings, &self.platform);
        let per_profile = usize::from(settings.sessions_per_profile);
        let mut count = sessions.len();
        while count > 0 {
            let request = PeriodicProfilePresentationRequest {
                source: ProfileSource::Candles {
                    price_series: PRICE_SERIES,
                    volume_series: self.product_series.volume,
                },
                boundaries: session_boundaries(&sessions[sessions.len() - count..], per_profile),
                tick_size: self.price_tick_size(),
                row_count: usize::from(settings.row_count),
                value_area_percent: f64::from(settings.value_area_percent),
            };
            #[cfg(test)]
            let drawn_request = request.clone();
            match self.engine.add_periodic_profile_presentation(
                PRICE_SERIES,
                request,
                options.clone(),
            ) {
                Ok(primitive) => {
                    return Some(DrawnProfile {
                        primitive,
                        #[cfg(test)]
                        request: drawn_request,
                    });
                }
                Err(ProfileError::LimitExceeded) => count /= 2,
                Err(error) => {
                    diagnostic!("Aeris Charts refused the session volume profile: {error}");
                    return None;
                }
            }
        }
        None
    }

    /// The profiles Aeris Charts' TPO series draws.
    #[cfg(test)]
    pub(super) fn drawn_tpo_profiles(&self) -> Option<Vec<aeris_charts_engine::TpoSnapshot>> {
        self.engine.tpo_series_profiles(PRICE_SERIES)
    }

    /// The session boundaries the drawn session volume profile was installed with.
    #[cfg(test)]
    pub(super) fn drawn_session_boundaries(
        &self,
        kind: SessionProfileKind,
    ) -> Option<Vec<ResampleBoundary>> {
        let drawn = self.session_profiles.get(kind)?.drawn.as_ref()?;
        Some(drawn.request.boundaries.clone())
    }

    /// The session volume profiles Aeris Charts draws, read back through their request.
    #[cfg(test)]
    pub(super) fn drawn_session_volume_profiles(
        &self,
    ) -> Option<Vec<aeris_charts_engine::ProfileSnapshot>> {
        let request = &self
            .session_profiles
            .get(SessionProfileKind::Volume)?
            .drawn
            .as_ref()?
            .request;
        self.engine
            .periodic_volume_profiles(
                request.source,
                &request.boundaries,
                request.tick_size,
                request.row_count,
                request.value_area_percent,
            )
            .ok()
    }

    #[cfg(test)]
    pub(super) fn tpo_refresh_at(&self) -> Option<f64> {
        self.session_profiles.tpo.refresh_at
    }
}

/// The calendar's trading days under the bars, at most the newest `limit`, and the next open
/// after the newest bar.
struct ProjectedSessions {
    sessions: Vec<ChartTradingSession>,
    next_open: Option<f64>,
}

fn project_sessions(
    calendar: &dyn TradingSessionCalendar,
    bars: &ProductPriceBars,
    limit: usize,
) -> Option<ProjectedSessions> {
    let first = bars.first_time()?.floor().to_i64()?;
    let newest = bars.last_time()?.floor().to_i64()?;
    let mut sessions = calendar.sessions(
        first,
        newest.saturating_add(LOOKAHEAD_SECONDS),
        limit.saturating_add(LOOKAHEAD_SESSIONS),
    );
    sessions.retain(|session| session.open_unix_seconds < session.close_unix_seconds);
    if sessions
        .windows(2)
        .any(|pair| pair[0].close_unix_seconds > pair[1].open_unix_seconds)
    {
        diagnostic!("trading calendar returned overlapping sessions; session profiles skipped");
        return None;
    }
    let next_open = sessions
        .iter()
        .find(|session| session.open_unix_seconds > newest)
        .and_then(|session| session.open_unix_seconds.to_f64());
    sessions.retain(|session| session.open_unix_seconds <= newest);
    let excess = sessions.len().saturating_sub(limit);
    sessions.drain(..excess);
    Some(ProjectedSessions {
        sessions,
        next_open,
    })
}

/// Boundaries of the sessions, grouped into profiles of `per_profile` sessions so the newest
/// group ends with the newest session.
fn session_boundaries(
    sessions: &[ChartTradingSession],
    per_profile: usize,
) -> Vec<ResampleBoundary> {
    let per_profile = per_profile.max(1);
    let newest = sessions.len().saturating_sub(1);
    sessions
        .iter()
        .enumerate()
        .map(|(index, session)| ResampleBoundary {
            start_time: session.open_unix_seconds,
            end_time: session.close_unix_seconds,
            session_id: u64::try_from((newest - index) / per_profile).unwrap_or(u64::MAX),
        })
        .collect()
}

/// A TPO row on the instrument's price grid, as many ticks wide as keeps the widest session
/// near [`TPO_TARGET_ROWS`] rows. `None` without a session or a valid grid.
fn tpo_row_size(
    tick: f64,
    sessions: &[ChartTradingSession],
    bars: &ProductPriceBars,
) -> Option<f64> {
    if sessions.is_empty() || !tick.is_finite() || tick <= 0.0 {
        return None;
    }
    let widest = sessions
        .iter()
        .filter_map(|session| {
            bars.price_range(
                session.open_unix_seconds.to_f64()?,
                session.close_unix_seconds.to_f64()?,
            )
        })
        .map(|(low, high)| high - low)
        .fold(0.0_f64, f64::max);
    let row_size = tick * (widest / tick / TPO_TARGET_ROWS).ceil().max(1.0);
    (row_size.is_finite() && row_size > 0.0).then_some(row_size)
}

fn tpo_series_options(
    settings: TpoSettings,
    row_size: f64,
    theme: &AerisTheme,
) -> TpoSeriesOptions {
    let colors = theme.colors;
    TpoSeriesOptions {
        period_seconds: u32::from(settings.period_minutes) * 60,
        tick_size: row_size,
        value_area_percent: f64::from(settings.value_area_percent),
        initial_balance_periods: u16::from(settings.initial_balance_periods),
        mode: match settings.cells {
            TpoCells::Letters => TpoCellMode::Letters,
            TpoCells::Blocks => TpoCellMode::Blocks,
        },
        color: colors.text_secondary.css_rgba(),
        value_area_color: colors.primary.css_rgba(),
        single_print_color: colors.negative.css_rgba(),
        poc_color: colors.warning.css_rgba(),
        initial_balance_color: colors.purple.css_rgba(),
    }
}

fn session_volume_profile_options(
    settings: SessionVolumeProfileSettings,
    theme: &AerisTheme,
) -> PeriodicProfilePresentationOptions {
    let colors = theme.colors;
    PeriodicProfilePresentationOptions {
        // Candle volume has no aggressor side, so the profile shows total volume.
        mode: ProfileDisplayMode::Total,
        bid_color: colors.bearish.css_rgba(),
        ask_color: colors.bullish.css_rgba(),
        unknown_color: colors.text_muted.css_rgba(),
        poc_color: colors.warning.css_rgba(),
        value_area_color: colors.primary.css_rgba(),
        extend_naked_levels: settings.extend_naked_levels,
        show_developing: settings.show_developing,
        ..PeriodicProfilePresentationOptions::default()
    }
}

#[cfg(test)]
mod tests;
