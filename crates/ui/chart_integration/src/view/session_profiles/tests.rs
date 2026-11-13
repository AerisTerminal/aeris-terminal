use super::*;
use aeris_application::ReplaySnapshot;

const HALF_HOUR: i64 = 1_800;
const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// 2024-03-06 00:00 UTC. United States daylight saving time starts on 2024-03-10.
const MARCH_6_2024: i64 = 1_709_683_200;

/// A calendar answering from a fixed list, as the market calendar answers for one instrument.
struct FixedCalendar(Vec<ChartTradingSession>);

impl TradingSessionCalendar for FixedCalendar {
    fn sessions(
        &self,
        from_unix_seconds: i64,
        to_unix_seconds: i64,
        limit: usize,
    ) -> Vec<ChartTradingSession> {
        let mut sessions = self
            .0
            .iter()
            .copied()
            .filter(|session| {
                session.close_unix_seconds > from_unix_seconds
                    && session.open_unix_seconds < to_unix_seconds
            })
            .collect::<Vec<_>>();
        let excess = sessions.len().saturating_sub(limit);
        sessions.drain(..excess);
        sessions
    }
}

fn session(open_unix_seconds: i64, close_unix_seconds: i64) -> ChartTradingSession {
    ChartTradingSession {
        open_unix_seconds,
        close_unix_seconds,
    }
}

/// Trading days of a futures contract trading 17:00 to 16:00 Chicago time: 23:00 to 22:00 UTC
/// before the daylight saving change and 22:00 to 21:00 UTC after it.
fn futures_sessions() -> Vec<ChartTradingSession> {
    vec![
        session(MARCH_6_2024 + 23 * HOUR, MARCH_6_2024 + DAY + 22 * HOUR),
        session(
            MARCH_6_2024 + DAY + 23 * HOUR,
            MARCH_6_2024 + 2 * DAY + 22 * HOUR,
        ),
        session(
            MARCH_6_2024 + 4 * DAY + 22 * HOUR,
            MARCH_6_2024 + 5 * DAY + 21 * HOUR,
        ),
        session(
            MARCH_6_2024 + 5 * DAY + 22 * HOUR,
            MARCH_6_2024 + 6 * DAY + 21 * HOUR,
        ),
        session(
            MARCH_6_2024 + 6 * DAY + 22 * HOUR,
            MARCH_6_2024 + 7 * DAY + 21 * HOUR,
        ),
    ]
}

/// A market open around the clock: every UTC day, weekends included, is one trading day.
fn utc_day_sessions(first_day: i64, days: i64) -> Vec<ChartTradingSession> {
    (0..days)
        .map(|day| {
            let open = first_day + day * DAY;
            session(open, open + DAY)
        })
        .collect()
}

fn chart_with_bars(start: i64, interval: i64, count: usize) -> AerisChartView {
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: count })
        .expect("fixture snapshot");
    let bars = baseline
        .bars()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut bar = *item.value();
            let time = start + i64::try_from(index).expect("bar index fits") * interval;
            bar.exchange_timestamp_seconds = time;
            bar.exchange_timestamp_unix_nanos = time * 1_000_000_000;
            bar
        })
        .collect();
    let mut definition = baseline.bar_definition().clone();
    definition.definition_id = format!("fixture:{interval}s:unadjusted:v1");
    definition.interval_seconds = u32::try_from(interval).expect("interval fits");
    let replay = ReplaySnapshot::try_new(
        baseline.instrument().clone(),
        baseline.provenance(),
        definition,
        bars,
    )
    .expect("fixture snapshot validates");
    AerisChartView::with_replay(&replay)
}

/// A fixture time in the chart's UTC seconds; every fixture time fits 32 bits.
fn utc(seconds: i64) -> f64 {
    f64::from(u32::try_from(seconds).expect("fixture times fit 32 bits"))
}

fn install_calendar(chart: &mut AerisChartView, sessions: Vec<ChartTradingSession>) {
    chart.set_trading_session_calendar(Some(Rc::new(FixedCalendar(sessions))));
}

fn boundary_spans(boundaries: &[ResampleBoundary]) -> Vec<(i64, i64, u64)> {
    boundaries
        .iter()
        .map(|boundary| (boundary.start_time, boundary.end_time, boundary.session_id))
        .collect()
}

fn refresh_at(chart: &AerisChartView, kind: SessionProfileKind) -> Option<f64> {
    chart.session_profiles.get(kind)?.refresh_at
}

#[test]
fn tpo_sessions_follow_the_calendar_across_a_daylight_saving_change() {
    // Half-hour bars from Wednesday 22:00 UTC through Tuesday 17:30 UTC, weekend included.
    let mut chart = chart_with_bars(MARCH_6_2024 + 22 * HOUR, HALF_HOUR, 280);
    install_calendar(&mut chart, futures_sessions());
    chart
        .add_indicator(ChartIndicator::Tpo)
        .expect("TPO is added");
    assert!(chart.session_profile_drawn(SessionProfileKind::Tpo));

    let sessions = futures_sessions();
    let boundaries = chart
        .drawn_session_boundaries(SessionProfileKind::Tpo)
        .expect("TPO boundaries");
    // The session that opens after the newest bar is not drawn; the newest drawn session is 0.
    assert_eq!(
        boundary_spans(&boundaries),
        sessions[..4]
            .iter()
            .zip([3, 2, 1, 0])
            .map(|(session, id)| (session.open_unix_seconds, session.close_unix_seconds, id))
            .collect::<Vec<_>>()
    );
    // Across the change the open moves from 23:00 to 22:00 UTC: a weekend minus one hour.
    assert_eq!(
        boundaries[2].start_time - boundaries[1].start_time,
        3 * DAY - HOUR
    );
    assert_eq!(boundaries[2].end_time - boundaries[2].start_time, 23 * HOUR);
    assert_eq!(
        refresh_at(&chart, SessionProfileKind::Tpo),
        Some(utc(MARCH_6_2024 + 6 * DAY + 22 * HOUR)),
        "a bar from the next open redraws the TPO"
    );

    let profiles = chart
        .drawn_tpo_profiles()
        .expect("Aeris Charts draws the TPO");
    assert_eq!(profiles.len(), 4);
    for (profile, boundary) in profiles.iter().zip(&boundaries) {
        assert_eq!(
            profile.start_timestamp_micros,
            boundary.start_time * 1_000_000
        );
        let periods_per_session = (boundary.end_time - boundary.start_time) / HALF_HOUR;
        assert!(
            profile
                .rows
                .iter()
                .flat_map(|row| &row.periods)
                .all(|&period| i64::from(period) < periods_per_session),
            "every letter falls inside its own trading day"
        );
        let poc = profile.poc.expect("point of control");
        let (low, high) = (
            profile.value_area_low.expect("value area low"),
            profile.value_area_high.expect("value area high"),
        );
        assert!(low <= poc && poc <= high);
        assert!(profile.initial_balance_low.is_some() && profile.initial_balance_high.is_some());
    }
    // The newest session is still trading: its last letter is the half hour from 17:30 UTC,
    // 19.5 hours after its 22:00 UTC open.
    let newest_periods = profiles[3]
        .rows
        .iter()
        .flat_map(|row| row.periods.iter().copied())
        .max();
    assert_eq!(newest_periods, Some(39));
}

#[test]
fn around_the_clock_sessions_split_at_every_utc_midnight_including_weekends() {
    // Friday 2024-03-08 00:00 UTC, hourly bars through Monday 11:00 UTC.
    let friday = MARCH_6_2024 + 2 * DAY;
    let mut chart = chart_with_bars(friday, HOUR, 84);
    install_calendar(&mut chart, utc_day_sessions(friday, 6));
    chart
        .add_indicator(ChartIndicator::Tpo)
        .expect("TPO is added");
    assert!(
        chart.set_session_profile_settings(SessionProfileSettings::Tpo(TpoSettings {
            period_minutes: 60,
            ..TpoSettings::default()
        }))
    );

    let boundaries = chart
        .drawn_session_boundaries(SessionProfileKind::Tpo)
        .expect("TPO boundaries");
    assert_eq!(
        boundary_spans(&boundaries),
        (0..4)
            .map(|day| (
                friday + day * DAY,
                friday + (day + 1) * DAY,
                u64::try_from(3 - day).expect("id")
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        refresh_at(&chart, SessionProfileKind::Tpo),
        Some(utc(friday + 4 * DAY))
    );
    let profiles = chart
        .drawn_tpo_profiles()
        .expect("Aeris Charts draws the TPO");
    let last_periods = profiles
        .iter()
        .map(|profile| {
            profile
                .rows
                .iter()
                .flat_map(|row| row.periods.iter().copied())
                .max()
        })
        .collect::<Vec<_>>();
    assert_eq!(last_periods, vec![Some(23), Some(23), Some(23), Some(11)]);
}

#[test]
fn composite_volume_profiles_end_with_the_newest_session() {
    let monday = MARCH_6_2024 - 2 * DAY;
    let mut chart = chart_with_bars(monday, HOUR, 5 * 24);
    install_calendar(&mut chart, utc_day_sessions(monday, 5));
    chart
        .add_indicator(ChartIndicator::SessionVolumeProfile)
        .expect("session volume profile is added");
    let settings = SessionVolumeProfileSettings {
        sessions_per_profile: 2,
        row_count: 24,
        show_developing: true,
        extend_naked_levels: true,
        ..SessionVolumeProfileSettings::default()
    };
    assert!(chart.set_session_profile_settings(SessionProfileSettings::Volume(settings)));
    assert_eq!(
        chart.session_profile_settings(SessionProfileKind::Volume),
        Some(SessionProfileSettings::Volume(settings))
    );

    let boundaries = chart
        .drawn_session_boundaries(SessionProfileKind::Volume)
        .expect("profile boundaries");
    assert_eq!(
        boundaries
            .iter()
            .map(|boundary| boundary.session_id)
            .collect::<Vec<_>>(),
        vec![2, 1, 1, 0, 0],
        "the oldest session alone forms the partial composite"
    );
    let profiles = chart
        .drawn_session_volume_profiles()
        .expect("Aeris Charts draws the profiles");
    assert_eq!(profiles.len(), 3);
    for profile in &profiles {
        assert!(profile.candle_approximation, "candle volume is spread");
        assert!(profile.total_volume > 0.0);
        assert!(profile.poc.is_some() && profile.value_area_low.is_some());
        assert!(profile.rows.len() <= 24);
    }
}

#[test]
fn session_profiles_follow_chart_type_visibility_calendar_and_settings() {
    let mut chart = chart_with_bars(MARCH_6_2024 + 22 * HOUR, HALF_HOUR, 286);
    chart
        .add_indicator(ChartIndicator::Tpo)
        .expect("TPO is added");
    assert!(
        chart.add_indicator(ChartIndicator::Tpo).is_err(),
        "one TPO per chart"
    );
    assert!(
        !chart.session_profile_drawn(SessionProfileKind::Tpo),
        "nothing is drawn before the calendar is known"
    );
    assert!(chart.legend_rows().iter().any(|row| row.item
        == LegendItem::SessionProfile(SessionProfileKind::Tpo)
        && row.settings_available));
    install_calendar(&mut chart, futures_sessions());
    assert!(chart.session_profile_drawn(SessionProfileKind::Tpo));

    chart.set_chart_type(ChartType::Line);
    assert!(!chart.session_profile_drawn(SessionProfileKind::Tpo));
    chart.set_chart_type(ChartType::HeikinAshi);
    assert!(chart.session_profile_drawn(SessionProfileKind::Tpo));

    let item = LegendItem::SessionProfile(SessionProfileKind::Tpo);
    assert!(chart.set_legend_item_visible(item, false));
    assert!(!chart.session_profile_drawn(SessionProfileKind::Tpo));
    assert!(chart.set_legend_item_visible(item, true));
    assert!(chart.session_profile_drawn(SessionProfileKind::Tpo));

    let invalid = TpoSettings {
        period_minutes: 3,
        ..TpoSettings::default()
    };
    assert!(!chart.set_session_profile_settings(SessionProfileSettings::Tpo(invalid)));
    assert!(
        !chart.set_session_profile_settings(SessionProfileSettings::default_for(
            SessionProfileKind::Volume
        )),
        "a profile that is not on the chart is not configured"
    );
    let blocks = TpoSettings {
        cells: TpoCells::Blocks,
        value_area_percent: 80,
        initial_balance_periods: 4,
        ..TpoSettings::default()
    };
    assert!(chart.set_session_profile_settings(SessionProfileSettings::Tpo(blocks)));
    assert!(!chart.set_session_profile_settings(SessionProfileSettings::Tpo(blocks)));

    install_calendar(
        &mut chart,
        vec![
            session(MARCH_6_2024, MARCH_6_2024 + 2 * DAY),
            session(MARCH_6_2024 + DAY, MARCH_6_2024 + 3 * DAY),
        ],
    );
    assert!(
        !chart.session_profile_drawn(SessionProfileKind::Tpo),
        "overlapping trading days are refused"
    );
    install_calendar(&mut chart, futures_sessions());
    assert!(chart.session_profile_drawn(SessionProfileKind::Tpo));

    assert!(chart.remove_legend_indicator(item));
    assert!(!chart.has_session_profiles());
    assert_eq!(chart.indicator_states(), Vec::new());
}

#[test]
fn session_profile_settings_round_trip_through_indicator_states() {
    let mut chart = chart_with_bars(MARCH_6_2024 + 22 * HOUR, HALF_HOUR, 286);
    chart
        .add_indicator(ChartIndicator::Tpo)
        .expect("TPO is added");
    chart
        .add_indicator(ChartIndicator::SessionVolumeProfile)
        .expect("session volume profile is added");
    let tpo = SessionProfileSettings::Tpo(TpoSettings {
        cells: TpoCells::Blocks,
        period_minutes: 15,
        value_area_percent: 68,
        initial_balance_periods: 4,
    });
    let volume = SessionProfileSettings::Volume(SessionVolumeProfileSettings {
        sessions_per_profile: 5,
        row_count: 96,
        value_area_percent: 75,
        show_developing: true,
        extend_naked_levels: true,
    });
    assert!(chart.set_session_profile_settings(tpo));
    assert!(chart.set_session_profile_settings(volume));
    assert!(chart.set_legend_item_visible(
        LegendItem::SessionProfile(SessionProfileKind::Volume),
        false
    ));
    let states = chart.indicator_states();
    assert_eq!(
        states,
        vec![
            ChartIndicatorState {
                indicator: ChartIndicator::Tpo,
                visible: true,
                volume: None,
                session_profile: Some(tpo),
            },
            ChartIndicatorState {
                indicator: ChartIndicator::SessionVolumeProfile,
                visible: false,
                volume: None,
                session_profile: Some(volume),
            },
        ]
    );

    let mut restored = chart_with_bars(MARCH_6_2024 + 22 * HOUR, HALF_HOUR, 286);
    install_calendar(&mut restored, futures_sessions());
    restored
        .restore_indicator_states(&states)
        .expect("session profiles restore");
    assert_eq!(restored.indicator_states(), states);
    assert!(restored.session_profile_drawn(SessionProfileKind::Tpo));
    assert!(!restored.session_profile_drawn(SessionProfileKind::Volume));

    // A state written without settings, or with settings of another kind, opens on defaults.
    let mut defaulted = chart_with_bars(MARCH_6_2024 + 22 * HOUR, HALF_HOUR, 286);
    defaulted
        .restore_indicator_states(&[ChartIndicatorState {
            indicator: ChartIndicator::Tpo,
            visible: true,
            volume: None,
            session_profile: Some(volume),
        }])
        .expect("TPO restores");
    assert_eq!(
        defaulted.session_profile_settings(SessionProfileKind::Tpo),
        Some(SessionProfileSettings::default_for(SessionProfileKind::Tpo))
    );
}
