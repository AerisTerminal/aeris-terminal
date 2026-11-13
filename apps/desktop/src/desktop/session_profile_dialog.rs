//! Settings dialog of the session profiles: the TPO (market profile) and the session or
//! composite volume profile. Every choice is inside the bounds the chart accepts, so each edit
//! previews at once through the chart's session profile owner; Apply keeps what the chart shows
//! and Cancel restores the settings the dialog opened with.

use super::big_trades_dialog::{
    DropdownChoice, SettingDropdown, dropdown_choices, setting_row, settings_group, settings_groups,
};
use super::*;
use aeris_localization::{Text, chart_menu, common, tr};

const TPO_CELL_CHOICES: [(Text, TpoCells); 2] = [
    (chart_menu::TPO_LETTERS, TpoCells::Letters),
    (chart_menu::TPO_BLOCKS, TpoCells::Blocks),
];
const TPO_PERIOD_MINUTES: [u16; 6] = [5, 10, 15, 30, 60, 120];
const TPO_INITIAL_BALANCE_PERIODS: [u8; 4] = [1, 2, 3, 4];
const VALUE_AREA_PERCENTS: [u8; 6] = [60, 65, 68, 70, 75, 80];
const SESSIONS_PER_PROFILE: [u8; 6] = [1, 2, 3, 5, 10, 20];
const PROFILE_ROW_COUNTS: [u16; 6] = [24, 48, 72, 96, 120, 240];

fn profile_title(kind: SessionProfileKind) -> &'static str {
    match kind {
        SessionProfileKind::Tpo => tr!(chart_menu::TPO_TITLE),
        SessionProfileKind::Volume => tr!(chart_menu::SESSION_VOLUME_PROFILE_TITLE),
    }
}

fn profile_removed_message(kind: SessionProfileKind) -> &'static str {
    match kind {
        SessionProfileKind::Tpo => tr!(chart_menu::TPO_REMOVED),
        SessionProfileKind::Volume => tr!(chart_menu::SESSION_VOLUME_PROFILE_REMOVED),
    }
}

/// Settings of one session profile being edited, previewed on the chart as they change.
pub(super) struct SessionProfileDialogState {
    /// The chart's settings when the dialog opened; Cancel restores them.
    original: SessionProfileSettings,
    draft: SessionProfileSettings,
    /// The setting whose dropdown is open.
    pub(super) menu: Option<String>,
    message: Option<&'static str>,
}

impl WorkspaceSurface {
    const fn session_profile_dialog(&self) -> Option<&SessionProfileDialogState> {
        match &self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::SessionProfile(dialog)) => Some(dialog),
            _ => None,
        }
    }

    const fn session_profile_dialog_mut(&mut self) -> Option<&mut SessionProfileDialogState> {
        match &mut self.indicator_settings_dialog {
            Some(IndicatorSettingsDialog::SessionProfile(dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// Opens one session profile's settings; every edit previews on the chart.
    pub(super) fn open_session_profile_dialog(
        &mut self,
        kind: SessionProfileKind,
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self
            .chart
            .as_ref()
            .and_then(|chart| chart.read(cx).session_profile_settings(kind))
        else {
            Self::post_indicator_failure(
                "session_profile_settings",
                tr!(chart_menu::STUDY_UNAVAILABLE),
                profile_removed_message(kind),
                cx,
            );
            return;
        };
        self.open_indicator_settings_dialog(
            IndicatorSettingsDialog::SessionProfile(SessionProfileDialogState {
                original: settings,
                draft: settings,
                menu: None,
                message: None,
            }),
            cx,
        );
    }

    /// Cancel: closes the dialog and restores the settings it opened with.
    pub(super) fn close_session_profile_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(original) = self.session_profile_dialog().map(|dialog| dialog.original) else {
            return;
        };
        self.indicator_settings_dialog = None;
        // A profile removed while the dialog was open has nothing left to restore.
        if let Err(error) = self.apply_session_profile_settings(original, cx) {
            aeris_observability::diagnostic!("Session profile dialog restore skipped: {error}");
        }
        cx.notify();
    }

    /// Hands `settings` to the chart's session profile owner, or says why it cannot hold them.
    fn apply_session_profile_settings(
        &mut self,
        settings: SessionProfileSettings,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let Some(chart) = &self.chart else {
            return Err(tr!(chart_menu::CHART_UNAVAILABLE));
        };
        let kind = settings.kind();
        let changed = chart.update(cx, |chart, chart_cx| {
            match chart.session_profile_settings(kind) {
                None => Err(profile_removed_message(kind)),
                Some(current) if current == settings => Ok(false),
                Some(_) if chart.set_session_profile_settings(settings) => {
                    chart_cx.notify();
                    Ok(true)
                }
                Some(_) => Err(tr!(chart_menu::CHART_APPLY_FAILED)),
            }
        })?;
        if changed {
            self.persistence.mark_dirty();
        }
        Ok(())
    }

    fn preview_session_profile_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.session_profile_dialog().map(|dialog| dialog.draft) else {
            return;
        };
        let result = self.apply_session_profile_settings(draft, cx);
        if let Some(dialog) = self.session_profile_dialog_mut() {
            dialog.message = result.err();
        }
        cx.notify();
    }

    fn edit_session_profile_draft(
        &mut self,
        edit: impl FnOnce(&mut SessionProfileSettings),
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.session_profile_dialog_mut() else {
            return;
        };
        let before = dialog.draft;
        edit(&mut dialog.draft);
        if dialog.draft != before {
            self.preview_session_profile_dialog(cx);
        }
    }

    fn edit_tpo_draft(&mut self, edit: impl FnOnce(&mut TpoSettings), cx: &mut Context<Self>) {
        self.edit_session_profile_draft(
            |draft| {
                if let SessionProfileSettings::Tpo(tpo) = draft {
                    edit(tpo);
                }
            },
            cx,
        );
    }

    fn edit_volume_profile_draft(
        &mut self,
        edit: impl FnOnce(&mut SessionVolumeProfileSettings),
        cx: &mut Context<Self>,
    ) {
        self.edit_session_profile_draft(
            |draft| {
                if let SessionProfileSettings::Volume(volume) = draft {
                    edit(volume);
                }
            },
            cx,
        );
    }

    /// Resets the profile to its defaults, shown on the chart at once.
    fn reset_session_profile_dialog(&mut self, cx: &mut Context<Self>) {
        self.edit_session_profile_draft(
            |draft| *draft = SessionProfileSettings::default_for(draft.kind()),
            cx,
        );
    }

    /// Apply: keeps the settings the chart already shows, closing only when it holds them.
    fn apply_session_profile_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.session_profile_dialog().map(|dialog| dialog.draft) else {
            return;
        };
        match self.apply_session_profile_settings(draft, cx) {
            Ok(()) => self.indicator_settings_dialog = None,
            Err(message) => {
                if let Some(dialog) = self.session_profile_dialog_mut() {
                    dialog.message = Some(message);
                }
            }
        }
        cx.notify();
    }
}

/// The preset values of a numeric setting, plus the current value when a saved layout holds
/// another accepted one, in ascending order.
fn numeric_choices<T: Copy + Ord>(
    presets: &[T],
    current: T,
    label: impl Fn(T) -> SharedString,
) -> Vec<DropdownChoice<T>> {
    let mut values = presets.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values
        .into_iter()
        .map(|value| DropdownChoice {
            label: label(value),
            value,
            selected: value == current,
        })
        .collect()
}

fn percent_label(percent: u8) -> SharedString {
    tr!(chart_menu::PROFILE_PERCENT, percent = percent).into()
}

fn count_label<T: Copy + ToString>(count: T) -> SharedString {
    count.to_string().into()
}

fn switch_row(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    checked: bool,
    on_change: impl Fn(&mut WorkspaceSurface, bool, &mut Context<WorkspaceSurface>) + 'static,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> AnyElement {
    let update = app.clone();
    div()
        .py_1()
        .child(
            SwitchRow::new(id, label, checked, theme)
                .description(description)
                .on_change(move |checked, _, _, cx| {
                    update.update(cx, |surface, surface_cx| {
                        on_change(surface, checked, surface_cx);
                    });
                }),
        )
        .into_any_element()
}

fn tpo_groups(
    app: &Entity<WorkspaceSurface>,
    dialog: &SessionProfileDialogState,
    tpo: TpoSettings,
    theme: &AerisTheme,
) -> [AnyElement; 2] {
    let open_menu = dialog.menu.as_deref();
    let period = SettingDropdown {
        menu: "tpo_period",
        label: tr!(chart_menu::TPO_PERIOD),
        open_menu,
        enabled: true,
        choices: numeric_choices(&TPO_PERIOD_MINUTES, tpo.period_minutes, |minutes| {
            tr!(chart_menu::PROFILE_MINUTES, minutes = minutes).into()
        }),
    }
    .render(
        app,
        |surface, minutes, cx| surface.edit_tpo_draft(|tpo| tpo.period_minutes = minutes, cx),
        theme,
    );
    let value_area = SettingDropdown {
        menu: "tpo_value_area",
        label: tr!(chart_menu::VALUE_AREA),
        open_menu,
        enabled: true,
        choices: numeric_choices(&VALUE_AREA_PERCENTS, tpo.value_area_percent, percent_label),
    }
    .render(
        app,
        |surface, percent, cx| surface.edit_tpo_draft(|tpo| tpo.value_area_percent = percent, cx),
        theme,
    );
    let initial_balance = SettingDropdown {
        menu: "tpo_initial_balance",
        label: tr!(chart_menu::INITIAL_BALANCE),
        open_menu,
        enabled: true,
        choices: numeric_choices(
            &TPO_INITIAL_BALANCE_PERIODS,
            tpo.initial_balance_periods,
            count_label,
        ),
    }
    .render(
        app,
        |surface, periods, cx| {
            surface.edit_tpo_draft(|tpo| tpo.initial_balance_periods = periods, cx);
        },
        theme,
    );
    let cells = SettingDropdown {
        menu: "tpo_cells",
        label: tr!(chart_menu::TPO_CELLS),
        open_menu,
        enabled: true,
        choices: dropdown_choices(&TPO_CELL_CHOICES, |cells| tpo.cells == cells),
    }
    .render(
        app,
        |surface, cells, cx| surface.edit_tpo_draft(|tpo| tpo.cells = cells, cx),
        theme,
    );
    [
        settings_group(
            tr!(chart_menu::CALCULATION),
            [
                setting_row(
                    tr!(chart_menu::TPO_PERIOD),
                    tr!(chart_menu::TPO_PERIOD_DESCRIPTION),
                    period,
                    theme,
                ),
                setting_row(
                    tr!(chart_menu::VALUE_AREA),
                    tr!(chart_menu::VALUE_AREA_DESCRIPTION),
                    value_area,
                    theme,
                ),
                setting_row(
                    tr!(chart_menu::INITIAL_BALANCE),
                    tr!(chart_menu::INITIAL_BALANCE_DESCRIPTION),
                    initial_balance,
                    theme,
                ),
            ],
            theme,
        ),
        settings_group(
            tr!(chart_menu::DISPLAY),
            [setting_row(
                tr!(chart_menu::TPO_CELLS),
                tr!(chart_menu::TPO_CELLS_DESCRIPTION),
                cells,
                theme,
            )],
            theme,
        ),
    ]
}

fn volume_profile_groups(
    app: &Entity<WorkspaceSurface>,
    dialog: &SessionProfileDialogState,
    volume: SessionVolumeProfileSettings,
    theme: &AerisTheme,
) -> [AnyElement; 2] {
    let open_menu = dialog.menu.as_deref();
    let sessions = SettingDropdown {
        menu: "volume_profile_sessions",
        label: tr!(chart_menu::SESSIONS_PER_PROFILE),
        open_menu,
        enabled: true,
        choices: numeric_choices(
            &SESSIONS_PER_PROFILE,
            volume.sessions_per_profile,
            count_label,
        ),
    }
    .render(
        app,
        |surface, sessions, cx| {
            surface.edit_volume_profile_draft(|volume| volume.sessions_per_profile = sessions, cx);
        },
        theme,
    );
    let rows = SettingDropdown {
        menu: "volume_profile_rows",
        label: tr!(chart_menu::PROFILE_ROWS),
        open_menu,
        enabled: true,
        choices: numeric_choices(&PROFILE_ROW_COUNTS, volume.row_count, count_label),
    }
    .render(
        app,
        |surface, rows, cx| surface.edit_volume_profile_draft(|volume| volume.row_count = rows, cx),
        theme,
    );
    let value_area = SettingDropdown {
        menu: "volume_profile_value_area",
        label: tr!(chart_menu::VALUE_AREA),
        open_menu,
        enabled: true,
        choices: numeric_choices(
            &VALUE_AREA_PERCENTS,
            volume.value_area_percent,
            percent_label,
        ),
    }
    .render(
        app,
        |surface, percent, cx| {
            surface.edit_volume_profile_draft(|volume| volume.value_area_percent = percent, cx);
        },
        theme,
    );
    [
        settings_group(
            tr!(chart_menu::CALCULATION),
            [
                setting_row(
                    tr!(chart_menu::SESSIONS_PER_PROFILE),
                    tr!(chart_menu::SESSIONS_PER_PROFILE_DESCRIPTION),
                    sessions,
                    theme,
                ),
                setting_row(
                    tr!(chart_menu::PROFILE_ROWS),
                    tr!(chart_menu::PROFILE_ROWS_DESCRIPTION),
                    rows,
                    theme,
                ),
                setting_row(
                    tr!(chart_menu::VALUE_AREA),
                    tr!(chart_menu::VALUE_AREA_DESCRIPTION),
                    value_area,
                    theme,
                ),
            ],
            theme,
        ),
        volume_profile_display_group(app, volume, theme),
    ]
}

fn volume_profile_display_group(
    app: &Entity<WorkspaceSurface>,
    volume: SessionVolumeProfileSettings,
    theme: &AerisTheme,
) -> AnyElement {
    settings_group(
        tr!(chart_menu::DISPLAY),
        [
            switch_row(
                "volume_profile_developing",
                tr!(chart_menu::SHOW_DEVELOPING),
                tr!(chart_menu::SHOW_DEVELOPING_DESCRIPTION),
                volume.show_developing,
                |surface, shown, cx| {
                    surface.edit_volume_profile_draft(|volume| volume.show_developing = shown, cx);
                },
                app,
                theme,
            ),
            switch_row(
                "volume_profile_naked_levels",
                tr!(chart_menu::EXTEND_NAKED_LEVELS),
                tr!(chart_menu::EXTEND_NAKED_LEVELS_DESCRIPTION),
                volume.extend_naked_levels,
                |surface, extended, cx| {
                    surface.edit_volume_profile_draft(
                        |volume| volume.extend_naked_levels = extended,
                        cx,
                    );
                },
                app,
                theme,
            ),
        ],
        theme,
    )
}

pub(super) fn session_profile_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &SessionProfileDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let groups = match dialog.draft {
        SessionProfileSettings::Tpo(tpo) => tpo_groups(app, dialog, tpo, theme),
        SessionProfileSettings::Volume(volume) => volume_profile_groups(app, dialog, volume, theme),
    };
    let dismiss = app.clone();
    let reset = app.clone();
    let cancel = app.clone();
    let apply = app.clone();
    Dialog::new(
        "session_profile_dialog",
        DialogSize::Md,
        theme,
        move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_session_profile_dialog);
        },
    )
    .title(tr!(chart_menu::INDICATOR_SETTINGS))
    .subtitle(profile_title(dialog.draft.kind()))
    .max_height(px(680.0))
    .child(settings_groups(groups, theme))
    .child(
        div()
            .text_xs()
            .text_color(gpui_color(colors.text_muted))
            .child(tr!(chart_menu::SESSION_PROFILE_NOTE)),
    )
    .children(dialog.message.map(|message| {
        div()
            .text_sm()
            .text_color(gpui_color(colors.danger))
            .child(message)
    }))
    .footer_leading(
        Button::new("session_profile_reset", theme)
            .variant(ButtonVariant::Destructive)
            .label(tr!(chart_menu::RESET_TO_DEFAULTS))
            .on_click(move |_, _, cx| {
                reset.update(cx, WorkspaceSurface::reset_session_profile_dialog);
            }),
    )
    .action(
        Button::new("session_profile_cancel", theme)
            .variant(ButtonVariant::Secondary)
            .label(tr!(common::CANCEL))
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_session_profile_dialog);
            }),
    )
    .action(
        Button::new("session_profile_apply", theme)
            .variant(ButtonVariant::Primary)
            .label(tr!(chart_menu::APPLY))
            .on_click(move |_, _, cx| {
                apply.update(cx, WorkspaceSurface::apply_session_profile_dialog);
            }),
    )
    .into_any_element()
}
