//! The opt-in market recording choice and its dialog, opened from the profile menu. The market
//! runtime owns the recorder, its files and the size cap; the desktop keeps only the user's
//! choice, saved in the workspace document, and shows the usage the runtime publishes.

use super::native_ui::numeric_text;
use super::*;
use aeris_contracts::{WorkspaceMarketRecordingState, WorkspaceRecordedInstrumentState};
use aeris_localization::{shell, tr};
use aeris_market_runtime::{
    DEFAULT_RECORDING_CAPACITY_BYTES, MAXIMUM_RECORDED_INSTRUMENTS,
    MAXIMUM_RECORDING_CAPACITY_BYTES, MINIMUM_RECORDING_CAPACITY_BYTES, MarketRecordingError,
    MarketRecordingFailure, MarketRecordingSession, MarketRecordingSettings, MarketRecordingStatus,
    RecordedInstrument, is_recording_replay_provider,
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// The size limits the dialog offers, each inside the runtime's accepted range.
const CAPACITY_CHOICES: [u64; 6] = [512 * MIB, GIB, 2 * GIB, 5 * GIB, 10 * GIB, 20 * GIB];
/// How often the open dialog reads the runtime's published usage again.
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const MARKET_RECORDING_MAX_HEIGHT: f32 = 600.0;
/// The newest recorded sessions the session chooser lists for one instrument.
const MAXIMUM_LISTED_SESSIONS: usize = 12;
const NANOS_PER_MILLI: i64 = 1_000_000;

/// One instrument the user chose to record, with the symbol shown when it was chosen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ChosenInstrument {
    pub(super) instrument: RecordedInstrument,
    pub(super) display_symbol: String,
}

impl ChosenInstrument {
    fn from_product(product: &InstallProviderInstrument) -> Self {
        Self {
            instrument: RecordedInstrument {
                provider_id: product.provider.clone(),
                instrument_id: product.instrument_id.clone(),
            },
            display_symbol: product.display_symbol.clone(),
        }
    }
}

/// The user's recording choice: the instruments to record and the cap of every recording
/// together. The default records nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MarketRecordingChoice {
    instruments: Vec<ChosenInstrument>,
    capacity_bytes: u64,
}

impl Default for MarketRecordingChoice {
    fn default() -> Self {
        Self {
            instruments: Vec::new(),
            capacity_bytes: DEFAULT_RECORDING_CAPACITY_BYTES,
        }
    }
}

/// The choice already holds the most instruments the runtime records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InstrumentLimitReached;

impl MarketRecordingChoice {
    /// Restores the saved choice. Invalid and repeated instruments are dropped, at most
    /// [`MAXIMUM_RECORDED_INSTRUMENTS`] are kept, and a cap outside the runtime's range uses
    /// the default.
    pub(super) fn from_workspace(state: Option<&WorkspaceMarketRecordingState>) -> Self {
        let Some(state) = state else {
            return Self::default();
        };
        let capacity_bytes = if (MINIMUM_RECORDING_CAPACITY_BYTES
            ..=MAXIMUM_RECORDING_CAPACITY_BYTES)
            .contains(&state.capacity_bytes)
        {
            state.capacity_bytes
        } else {
            DEFAULT_RECORDING_CAPACITY_BYTES
        };
        let mut choice = Self {
            instruments: Vec::new(),
            capacity_bytes,
        };
        for saved in &state.instruments {
            if choice.instruments.len() == MAXIMUM_RECORDED_INSTRUMENTS {
                break;
            }
            let instrument = RecordedInstrument {
                provider_id: saved.provider.clone(),
                instrument_id: saved.instrument_id.clone(),
            };
            if valid_field(&instrument.provider_id)
                && valid_field(&instrument.instrument_id)
                && !choice.contains(&instrument)
            {
                let display_symbol = if valid_field(&saved.display_symbol) {
                    saved.display_symbol.clone()
                } else {
                    String::new()
                };
                choice.instruments.push(ChosenInstrument {
                    instrument,
                    display_symbol,
                });
            }
        }
        choice
    }

    /// The document form. `None` while nothing is chosen at the default cap, so a document
    /// written before recording existed keeps its bytes.
    pub(super) fn to_workspace(&self) -> Option<WorkspaceMarketRecordingState> {
        (*self != Self::default()).then(|| WorkspaceMarketRecordingState {
            instruments: self
                .instruments
                .iter()
                .map(|chosen| WorkspaceRecordedInstrumentState {
                    provider: chosen.instrument.provider_id.clone(),
                    instrument_id: chosen.instrument.instrument_id.clone(),
                    display_symbol: chosen.display_symbol.clone(),
                })
                .collect(),
            capacity_bytes: self.capacity_bytes,
        })
    }

    pub(super) fn contains(&self, instrument: &RecordedInstrument) -> bool {
        self.instruments
            .iter()
            .any(|chosen| chosen.instrument == *instrument)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }

    /// The choice with `chosen` added, or removed when it is already chosen.
    pub(super) fn toggled(&self, chosen: ChosenInstrument) -> Result<Self, InstrumentLimitReached> {
        let mut next = self.clone();
        if self.contains(&chosen.instrument) {
            next.instruments
                .retain(|existing| existing.instrument != chosen.instrument);
        } else if self.instruments.len() >= MAXIMUM_RECORDED_INSTRUMENTS {
            return Err(InstrumentLimitReached);
        } else {
            next.instruments.push(chosen);
        }
        Ok(next)
    }

    pub(super) fn with_capacity(&self, capacity_bytes: u64) -> Self {
        Self {
            capacity_bytes,
            ..self.clone()
        }
    }

    pub(super) fn settings(&self, data_root: &std::path::Path) -> MarketRecordingSettings {
        MarketRecordingSettings {
            data_root: data_root.to_path_buf(),
            instruments: self
                .instruments
                .iter()
                .map(|chosen| chosen.instrument.clone())
                .collect(),
            capacity_bytes: self.capacity_bytes,
        }
    }
}

fn valid_field(field: &str) -> bool {
    !field.is_empty() && field.len() <= aeris_market_data::MAXIMUM_MARKET_DATA_FIELD_BYTES
}

/// Why the recorder refused the last change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingRefusal {
    Limit,
    Busy,
    Unavailable,
}

impl From<MarketRecordingError> for RecordingRefusal {
    fn from(error: MarketRecordingError) -> Self {
        match error {
            MarketRecordingError::TooManyInstruments => Self::Limit,
            MarketRecordingError::Busy => Self::Busy,
            MarketRecordingError::DuplicateInstrument
            | MarketRecordingError::InvalidInstrument
            | MarketRecordingError::CapacityOutOfRange
            | MarketRecordingError::DataRootChanged
            | MarketRecordingError::NotConfigured
            | MarketRecordingError::Unavailable => Self::Unavailable,
        }
    }
}

impl RecordingRefusal {
    fn message(self) -> String {
        match self {
            Self::Limit => tr!(
                shell::MARKET_RECORDING_LIMIT,
                maximum = MAXIMUM_RECORDED_INSTRUMENTS
            ),
            Self::Busy => tr!(shell::MARKET_RECORDING_BUSY).to_string(),
            Self::Unavailable => tr!(shell::MARKET_RECORDING_UNAVAILABLE).to_string(),
        }
    }
}

/// The recording choice and the dialog's own state.
#[derive(Default)]
pub(super) struct MarketRecordingUi {
    choice: MarketRecordingChoice,
    refusal: Option<RecordingRefusal>,
    confirming_delete_all: bool,
    /// The instrument whose recorded sessions the dialog lists to choose a replay from.
    choosing_session: Option<RecordedInstrument>,
    /// Repaints the open dialog with fresh usage; dropping it stops the refresh.
    refresh: Option<gpui::Task<()>>,
}

impl MarketRecordingUi {
    pub(super) fn restore(choice: MarketRecordingChoice) -> Self {
        Self {
            choice,
            ..Self::default()
        }
    }

    pub(super) fn saved_state(&self) -> Option<WorkspaceMarketRecordingState> {
        self.choice.to_workspace()
    }
}

impl TerminalApp {
    /// Applies the restored choice at startup; nothing starts while no instrument is chosen.
    pub(super) fn start_market_recording(&mut self) {
        if self.recording.choice.is_empty() {
            return;
        }
        let settings = self
            .recording
            .choice
            .settings(self.lifecycle.persistence().data_root());
        if let Err(error) = self.lifecycle.market().configure_recording(settings) {
            diagnostic!("Aeris market recording could not start: {error}");
            self.recording.refusal = Some(error.into());
        }
    }

    pub(super) fn open_market_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .overlays
            .open_market_recording()
            .is_some_and(OverlayKind::pins_title_bar)
        {
            self.release_frameless_title_bar(window, cx);
        }
        // The current choice is applied again so the recorder indexes existing recordings and
        // their usage shows even while nothing is recorded; it also retries a stopped recorder.
        let settings = self
            .recording
            .choice
            .settings(self.lifecycle.persistence().data_root());
        self.recording.refusal = self
            .lifecycle
            .market()
            .configure_recording(settings)
            .err()
            .map(RecordingRefusal::from);
        self.recording.confirming_delete_all = false;
        self.recording.refresh = Some(cx.spawn(async move |terminal, cx| {
            loop {
                cx.background_executor()
                    .timer(STATUS_REFRESH_INTERVAL)
                    .await;
                let open = terminal.update(cx, |terminal, terminal_cx| {
                    let open = terminal.overlays.is_open(OverlayKind::MarketRecording);
                    if open {
                        terminal_cx.notify();
                    }
                    open
                });
                if !matches!(open, Ok(true)) {
                    break;
                }
            }
        }));
        // The shell's key handler closes the dialog on Escape, so it needs keyboard focus.
        self.window.chrome_focus().focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_market_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.recording.refresh = None;
        self.recording.confirming_delete_all = false;
        self.recording.choosing_session = None;
        if self.overlays.close(OverlayKind::MarketRecording) {
            self.focus_workspace(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_recorded_instrument(
        &mut self,
        chosen: ChosenInstrument,
        cx: &mut Context<Self>,
    ) {
        if let Ok(choice) = self.recording.choice.toggled(chosen) {
            self.commit_market_recording(choice, cx);
        } else {
            self.recording.refusal = Some(RecordingRefusal::Limit);
            cx.notify();
        }
    }

    pub(super) fn set_market_recording_capacity(
        &mut self,
        capacity_bytes: u64,
        cx: &mut Context<Self>,
    ) {
        let choice = self.recording.choice.with_capacity(capacity_bytes);
        self.commit_market_recording(choice, cx);
    }

    /// Hands `choice` to the runtime and keeps and saves it only once the runtime accepted it.
    fn commit_market_recording(&mut self, choice: MarketRecordingChoice, cx: &mut Context<Self>) {
        self.recording.confirming_delete_all = false;
        let settings = choice.settings(self.lifecycle.persistence().data_root());
        match self.lifecycle.market().configure_recording(settings) {
            Ok(()) => {
                self.recording.refusal = None;
                if choice != self.recording.choice {
                    self.recording.choice = choice;
                    self.record_layout_change(cx);
                }
            }
            Err(error) => self.recording.refusal = Some(error.into()),
        }
        cx.notify();
    }

    /// Deletes one instrument's recordings, or every recording for `None` after the user
    /// confirms with a second press. Recording continues for chosen instruments.
    pub(super) fn delete_market_recordings(
        &mut self,
        instrument: Option<RecordedInstrument>,
        cx: &mut Context<Self>,
    ) {
        if instrument.is_none() && !self.recording.confirming_delete_all {
            self.recording.confirming_delete_all = true;
            cx.notify();
            return;
        }
        self.recording.confirming_delete_all = false;
        self.recording.refusal = self
            .lifecycle
            .market()
            .delete_recordings(instrument)
            .err()
            .map(RecordingRefusal::from);
        cx.notify();
    }

    /// A row's Open replay: opens its only recorded session, or lists its sessions to choose.
    fn choose_recording_replay(
        &mut self,
        instrument: RecordedInstrument,
        sessions: &[MarketRecordingSession],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let [only] = sessions {
            self.open_recording_replay(instrument, only.started_unix_millis, window, cx);
            return;
        }
        self.recording.choosing_session =
            (self.recording.choosing_session.as_ref() != Some(&instrument)).then_some(instrument);
        cx.notify();
    }

    /// Closes the dialog and replays the session on the active chart.
    fn open_recording_replay(
        &mut self,
        instrument: RecordedInstrument,
        session_started_unix_millis: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_market_recording(window, cx);
        let surface = self.active_surface().clone();
        surface.update(cx, |surface, surface_cx| {
            surface.open_recording_replay(instrument, session_started_unix_millis, surface_cx);
        });
    }

    pub(super) fn market_recording_layer(
        &self,
        terminal: &Entity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        self.overlays
            .is_open(OverlayKind::MarketRecording)
            .then(|| {
                let current = self
                    .active_surface()
                    .read(cx)
                    .product
                    .as_ref()
                    .filter(|product| !is_recording_replay_provider(&product.provider))
                    .map(ChosenInstrument::from_product);
                market_recording_dialog(
                    terminal,
                    &MarketRecordingView {
                        choice: &self.recording.choice,
                        status: self.lifecycle.market().recording_status(),
                        current,
                        refusal: self.recording.refusal,
                        confirming_delete_all: self.recording.confirming_delete_all,
                        choosing_session: self.recording.choosing_session.as_ref(),
                    },
                    &self.theme,
                )
            })
    }
}

struct MarketRecordingView<'a> {
    choice: &'a MarketRecordingChoice,
    status: MarketRecordingStatus,
    current: Option<ChosenInstrument>,
    refusal: Option<RecordingRefusal>,
    confirming_delete_all: bool,
    choosing_session: Option<&'a RecordedInstrument>,
}

/// One listed recording: an instrument with files, a chosen one, or both.
struct RecordingEntry {
    chosen: ChosenInstrument,
    bytes: u64,
    recording: bool,
    /// Its recorded sessions, oldest first.
    sessions: Vec<MarketRecordingSession>,
}

fn recording_entries(view: &MarketRecordingView<'_>) -> Vec<RecordingEntry> {
    let mut entries = view
        .status
        .instruments
        .iter()
        .map(|usage| RecordingEntry {
            chosen: ChosenInstrument {
                instrument: usage.instrument.clone(),
                display_symbol: usage.display_symbol.clone(),
            },
            bytes: usage.bytes,
            recording: view.choice.contains(&usage.instrument),
            sessions: usage.sessions.clone(),
        })
        .collect::<Vec<_>>();
    for chosen in &view.choice.instruments {
        if !entries
            .iter()
            .any(|entry| entry.chosen.instrument == chosen.instrument)
        {
            entries.push(RecordingEntry {
                chosen: chosen.clone(),
                bytes: 0,
                recording: true,
                sessions: Vec::new(),
            });
        }
    }
    entries
}

fn shown_symbol(chosen: &ChosenInstrument) -> &str {
    if chosen.display_symbol.is_empty() {
        &chosen.instrument.instrument_id
    } else {
        &chosen.display_symbol
    }
}

/// A byte count in megabytes below one gigabyte and in tenths of a gigabyte above.
fn size_label(bytes: u64) -> String {
    if bytes >= GIB {
        let tenths = bytes.saturating_mul(10) / GIB;
        let value = if tenths.is_multiple_of(10) {
            (tenths / 10).to_string()
        } else {
            format!("{}.{}", tenths / 10, tenths % 10)
        };
        tr!(shell::SIZE_GIGABYTES, value = value)
    } else {
        tr!(shell::SIZE_MEGABYTES, value = bytes.div_ceil(MIB))
    }
}

fn market_recording_dialog(
    terminal: &Entity<TerminalApp>,
    view: &MarketRecordingView<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let dismiss = terminal.clone();
    let delete_all = terminal.clone();
    let has_recordings = view.status.instruments.iter().any(|usage| usage.bytes > 0);
    Dialog::new(
        "market_recording",
        DialogSize::Md,
        theme,
        move |window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_market_recording(window, terminal_cx);
            });
        },
    )
    .title(tr!(shell::MARKET_RECORDING))
    .subtitle(tr!(shell::MARKET_RECORDING_SUBTITLE))
    .max_height(px(MARKET_RECORDING_MAX_HEIGHT))
    .child(current_instrument_row(terminal, view, theme))
    .children(recording_messages(view, theme))
    .child(capacity_section(terminal, view, theme))
    .child(recordings_section(terminal, view, theme))
    .footer_leading(tr!(shell::SHORTCUTS_FOOTER))
    .action(
        Button::new("market_recording_delete_all", theme)
            .variant(if view.confirming_delete_all {
                ButtonVariant::Destructive
            } else {
                ButtonVariant::Secondary
            })
            .button_size(ButtonSize::Sm)
            .label(if view.confirming_delete_all {
                tr!(shell::MARKET_RECORDING_CONFIRM_DELETE_ALL)
            } else {
                tr!(shell::MARKET_RECORDING_DELETE_ALL)
            })
            .disabled(!has_recordings)
            .on_press(move |_, _, cx| {
                delete_all.update(cx, |terminal, terminal_cx| {
                    terminal.delete_market_recordings(None, terminal_cx);
                });
            }),
    )
    .into_any_element()
}

/// The switch that records the active chart's instrument, with what recording keeps.
fn current_instrument_row(
    terminal: &Entity<TerminalApp>,
    view: &MarketRecordingView<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let Some(current) = view.current.clone() else {
        return div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(gpui_color(colors.text_default))
                    .child(tr!(shell::MARKET_RECORDING_NO_CHART)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(tr!(shell::MARKET_RECORDING_NOTE)),
            )
            .into_any_element();
    };
    let toggle = terminal.clone();
    let recording = view.choice.contains(&current.instrument);
    SwitchRow::new(
        "market_recording_current",
        tr!(
            shell::MARKET_RECORDING_RECORD,
            symbol = shown_symbol(&current)
        ),
        recording,
        theme,
    )
    .description(tr!(shell::MARKET_RECORDING_NOTE))
    .on_change(move |_, _, _, cx| {
        let chosen = current.clone();
        toggle.update(cx, |terminal, terminal_cx| {
            terminal.toggle_recorded_instrument(chosen, terminal_cx);
        });
    })
    .into_any_element()
}

/// The refusal, the recorder's failure and the events it skipped, when there are any.
fn recording_messages(view: &MarketRecordingView<'_>, theme: &AerisTheme) -> Vec<AnyElement> {
    let colors = theme.colors;
    let failure = view.status.failure.map(|failure| match failure {
        MarketRecordingFailure::StorageUnavailable => tr!(shell::MARKET_RECORDING_STORAGE_FAILED),
        MarketRecordingFailure::TooManyFiles => tr!(shell::MARKET_RECORDING_TOO_MANY_FILES),
    });
    let mut messages = view
        .refusal
        .map(RecordingRefusal::message)
        .into_iter()
        .chain(failure.map(str::to_string))
        .map(|message| {
            div()
                .text_xs()
                .text_color(gpui_color(colors.danger))
                .child(message)
                .into_any_element()
        })
        .collect::<Vec<_>>();
    if view.status.dropped_events > 0 {
        messages.push(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(tr!(
                    shell::MARKET_RECORDING_DROPPED,
                    events = view.status.dropped_events
                ))
                .into_any_element(),
        );
    }
    messages
}

fn section_heading(label: &'static str, theme: &AerisTheme) -> Div {
    div()
        .text_xs()
        .font_weight(platform_font_weight(TypographyRole::Strong))
        .text_color(gpui_color(theme.colors.text_muted))
        .child(label)
}

/// The usage against the cap and the cap choices.
fn capacity_section(
    terminal: &Entity<TerminalApp>,
    view: &MarketRecordingView<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let usage = if view.status.ready {
        tr!(
            shell::MARKET_RECORDING_USAGE,
            used = size_label(view.status.used_bytes),
            cap = size_label(view.choice.capacity_bytes)
        )
    } else {
        tr!(shell::MARKET_RECORDING_CHECKING).to_string()
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(section_heading(tr!(shell::MARKET_RECORDING_CAP), theme))
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_secondary))
                        .map(numeric_text)
                        .child(usage),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(tr!(shell::MARKET_RECORDING_CAP_DESCRIPTION)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(CAPACITY_CHOICES.iter().enumerate().map(|(index, &cap)| {
                    let pick = terminal.clone();
                    Button::new(("market_recording_cap", index), theme)
                        .variant(ButtonVariant::Secondary)
                        .button_size(ButtonSize::Sm)
                        .tabular_numerals()
                        .selected(cap == view.choice.capacity_bytes)
                        .label(size_label(cap))
                        .on_press(move |_, _, cx| {
                            pick.update(cx, |terminal, terminal_cx| {
                                terminal.set_market_recording_capacity(cap, terminal_cx);
                            });
                        })
                })),
        )
        .into_any_element()
}

/// Every instrument with recordings or chosen for recording, with its size, a switch and a
/// delete button.
fn recordings_section(
    terminal: &Entity<TerminalApp>,
    view: &MarketRecordingView<'_>,
    theme: &AerisTheme,
) -> AnyElement {
    let entries = recording_entries(view);
    let empty = entries.is_empty().then(|| {
        div()
            .text_xs()
            .text_color(gpui_color(theme.colors.text_muted))
            .child(tr!(shell::MARKET_RECORDING_NONE))
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(section_heading(tr!(shell::MARKET_RECORDING_LIST), theme))
        .children(empty)
        .children(entries.into_iter().enumerate().map(|(index, entry)| {
            let choosing = view.choosing_session == Some(&entry.chosen.instrument);
            recording_row(terminal, index, entry, choosing, theme)
        }))
        .into_any_element()
}

/// The time a recorded session started, for the session chooser.
fn session_label(session: &MarketRecordingSession) -> String {
    let started = i64::try_from(session.started_unix_millis)
        .unwrap_or(i64::MAX)
        .saturating_mul(NANOS_PER_MILLI);
    tr!(
        shell::MARKET_RECORDING_SESSION,
        start = super::recording_replay::recorded_time_label(started),
        size = size_label(session.bytes)
    )
}

/// The newest sessions of a recording, newest first, for the session chooser.
fn listed_sessions(
    sessions: &[MarketRecordingSession],
) -> impl Iterator<Item = &MarketRecordingSession> {
    sessions.iter().rev().take(MAXIMUM_LISTED_SESSIONS)
}

fn session_chooser(
    terminal: &Entity<TerminalApp>,
    index: usize,
    instrument: &RecordedInstrument,
    sessions: &[MarketRecordingSession],
    theme: &AerisTheme,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pl_4()
        .child(section_heading(
            tr!(shell::MARKET_RECORDING_SESSIONS),
            theme,
        ))
        .children(
            listed_sessions(sessions)
                .enumerate()
                .map(|(position, session)| {
                    let open = terminal.clone();
                    let instrument = instrument.clone();
                    let started = session.started_unix_millis;
                    Button::new(
                        (
                            "market_recording_session",
                            index * MAXIMUM_LISTED_SESSIONS + position,
                        ),
                        theme,
                    )
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Sm)
                    .tabular_numerals()
                    .icon(header_icon(HugeIcon::Play))
                    .label(session_label(session))
                    .on_press(move |_, window, cx| {
                        let instrument = instrument.clone();
                        open.update(cx, |terminal, terminal_cx| {
                            terminal.open_recording_replay(
                                instrument,
                                started,
                                window,
                                terminal_cx,
                            );
                        });
                    })
                }),
        )
}

fn recording_row(
    terminal: &Entity<TerminalApp>,
    index: usize,
    entry: RecordingEntry,
    choosing: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let RecordingEntry {
        chosen,
        bytes,
        recording,
        sessions,
    } = entry;
    let symbol = shown_symbol(&chosen).to_string();
    let toggle = terminal.clone();
    let delete = terminal.clone();
    let replay = terminal.clone();
    let instrument = chosen.instrument.clone();
    let replay_instrument = chosen.instrument.clone();
    let chooser =
        choosing.then(|| session_chooser(terminal, index, &chosen.instrument, &sessions, theme));
    let replay_sessions = sessions.clone();
    let row = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div().flex_1().min_w_0().child(
                SwitchRow::new(
                    ("market_recording_instrument", index),
                    symbol.clone(),
                    recording,
                    theme,
                )
                .description(size_label(bytes))
                .on_change(move |_, _, _, cx| {
                    let chosen = chosen.clone();
                    toggle.update(cx, |terminal, terminal_cx| {
                        terminal.toggle_recorded_instrument(chosen, terminal_cx);
                    });
                }),
            ),
        )
        .child(
            Button::new(("market_recording_replay", index), theme)
                .variant(ButtonVariant::Ghost)
                .icon(header_icon(HugeIcon::Play))
                .label(tr!(shell::MARKET_RECORDING_OPEN_REPLAY))
                .aria_label(tr!(
                    shell::MARKET_RECORDING_OPEN_REPLAY_ARIA,
                    symbol = symbol
                ))
                .selected(choosing)
                .disabled(sessions.is_empty())
                .on_press(move |_, window, cx| {
                    let instrument = replay_instrument.clone();
                    replay.update(cx, |terminal, terminal_cx| {
                        terminal.choose_recording_replay(
                            instrument,
                            &replay_sessions,
                            window,
                            terminal_cx,
                        );
                    });
                }),
        )
        .child(
            Button::new(("market_recording_delete", index), theme)
                .variant(ButtonVariant::Ghost)
                .icon(header_icon(HugeIcon::Trash))
                .aria_label(tr!(shell::MARKET_RECORDING_DELETE_ARIA, symbol = symbol))
                .danger_on_hover()
                .disabled(bytes == 0)
                .on_press(move |_, _, cx| {
                    let instrument = instrument.clone();
                    delete.update(cx, |terminal, terminal_cx| {
                        terminal.delete_market_recordings(Some(instrument), terminal_cx);
                    });
                }),
        );
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(row)
        .children(chooser)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(instrument_id: &str) -> ChosenInstrument {
        ChosenInstrument {
            instrument: RecordedInstrument {
                provider_id: "provider-a".to_string(),
                instrument_id: instrument_id.to_string(),
            },
            display_symbol: format!("{instrument_id} symbol"),
        }
    }

    #[test]
    fn nothing_is_recorded_by_default_and_the_default_writes_no_section() {
        let choice = MarketRecordingChoice::from_workspace(None);
        assert!(choice.is_empty());
        assert_eq!(choice.to_workspace(), None);
        assert_eq!(
            MarketRecordingChoice::from_workspace(Some(&WorkspaceMarketRecordingState::default())),
            MarketRecordingChoice::default(),
            "an empty saved section opens as the default"
        );
    }

    #[test]
    fn a_saved_choice_round_trips_and_damaged_entries_are_dropped() {
        let choice = MarketRecordingChoice::default()
            .toggled(chosen("instrument-1"))
            .expect("first instrument")
            .with_capacity(5 * GIB);
        let saved = choice.to_workspace().expect("a chosen instrument is saved");
        assert_eq!(MarketRecordingChoice::from_workspace(Some(&saved)), choice);

        let mut damaged = saved.clone();
        damaged.capacity_bytes = 1;
        damaged.instruments.push(saved.instruments[0].clone());
        damaged.instruments.push(WorkspaceRecordedInstrumentState {
            provider: String::new(),
            instrument_id: "orphan".to_string(),
            display_symbol: "ORPHAN".to_string(),
        });
        let restored = MarketRecordingChoice::from_workspace(Some(&damaged));
        assert_eq!(restored.instruments, choice.instruments);
        assert_eq!(restored.capacity_bytes, DEFAULT_RECORDING_CAPACITY_BYTES);
    }

    #[test]
    fn the_choice_is_bounded_and_toggles_off_again() {
        let mut choice = MarketRecordingChoice::default();
        for index in 0..MAXIMUM_RECORDED_INSTRUMENTS {
            choice = choice
                .toggled(chosen(&format!("instrument-{index}")))
                .expect("within the bound");
        }
        assert_eq!(
            choice.toggled(chosen("one-too-many")),
            Err(InstrumentLimitReached)
        );
        let removed = choice.toggled(chosen("instrument-0")).expect("removal");
        assert!(!removed.contains(&chosen("instrument-0").instrument));
        assert_eq!(removed.instruments.len(), MAXIMUM_RECORDED_INSTRUMENTS - 1);

        let mut oversized = choice.to_workspace().expect("saved");
        oversized
            .instruments
            .push(WorkspaceRecordedInstrumentState {
                provider: "provider-a".to_string(),
                instrument_id: "past-the-bound".to_string(),
                display_symbol: String::new(),
            });
        assert_eq!(
            MarketRecordingChoice::from_workspace(Some(&oversized))
                .instruments
                .len(),
            MAXIMUM_RECORDED_INSTRUMENTS
        );
    }

    #[test]
    fn the_session_chooser_lists_the_newest_sessions_first_within_its_bound() {
        let sessions = (0..20_u64)
            .map(|index| MarketRecordingSession {
                started_unix_millis: index * 1_000,
                bytes: MIB,
            })
            .collect::<Vec<_>>();
        let listed = listed_sessions(&sessions)
            .map(|session| session.started_unix_millis)
            .collect::<Vec<_>>();
        assert_eq!(listed.len(), MAXIMUM_LISTED_SESSIONS);
        assert_eq!(listed.first(), Some(&19_000));
        assert_eq!(listed.last(), Some(&8_000));
        let label = session_label(&sessions[1]);
        assert!(label.contains("00:00:01 UTC"), "{label}");
        assert!(label.ends_with(&size_label(MIB)), "{label}");
    }

    #[test]
    fn every_offered_cap_is_inside_the_runtime_range() {
        for cap in CAPACITY_CHOICES {
            assert!(
                (MINIMUM_RECORDING_CAPACITY_BYTES..=MAXIMUM_RECORDING_CAPACITY_BYTES)
                    .contains(&cap)
            );
        }
        assert!(CAPACITY_CHOICES.contains(&DEFAULT_RECORDING_CAPACITY_BYTES));
    }

    #[test]
    fn settings_carry_the_data_root_the_instruments_and_the_cap() {
        let choice = MarketRecordingChoice::default()
            .toggled(chosen("instrument-1"))
            .expect("instrument");
        let root = std::path::Path::new("aeris-root");
        let settings = choice.settings(root);
        assert_eq!(settings.data_root, root);
        assert_eq!(
            settings.instruments,
            vec![chosen("instrument-1").instrument]
        );
        assert_eq!(settings.capacity_bytes, DEFAULT_RECORDING_CAPACITY_BYTES);
    }
}
