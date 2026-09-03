//! Native display geometry, refresh, and presentation-clock probing.
//!
//! The terminal needs each output's refresh interval and scale factor to schedule
//! chart redraws against real presentation boundaries, and the compositor's
//! presentation clock to interpret frame feedback. On Linux this adapter reads
//! that state from the Wayland session: `wl_output` supplies geometry, current
//! mode refresh, integer scale, and (version 4) connector names, the XDG output
//! manager supplies logical geometry so fractional scales are derived from real
//! physical-to-logical ratios rather than guessed, and `wp_presentation`
//! reports which clock presented-frame feedback will use. Per-frame feedback
//! itself requires a committed surface, so it remains the UI layer's
//! responsibility; this probe only reports the clock the feedback will carry.
//! Windows reads the same current-mode fields through the operating system's
//! display configuration APIs. macOS reads the native CoreGraphics-backed
//! display inventory through the pinned `display-info` adapter. Unsupported
//! Linux sessions report the port unavailable rather than inventing geometry.

use crate::CapabilityAvailability;
use core::fmt;
use std::error::Error;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::time::Duration;

#[cfg(target_os = "linux")]
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    protocol::{wl_output, wl_registry},
};
#[cfg(target_os = "linux")]
use wayland_protocols::{
    wp::presentation_time::client::wp_presentation,
    xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1},
};

/// Highest `wl_output` version this adapter binds; version 4 carries name and
/// description events.
#[cfg(target_os = "linux")]
const WL_OUTPUT_VERSION: u32 = 4;
/// Highest `zxdg_output_manager_v1` version this adapter binds; version 3
/// deprecates the per-output `done` event this probe does not rely on.
#[cfg(target_os = "linux")]
const XDG_OUTPUT_MANAGER_VERSION: u32 = 3;
/// `clockid_t` value identifying `CLOCK_MONOTONIC` in a `wp_presentation`
/// `clock_id` event.
const MONOTONIC_CLOCK_ID: u32 = 1;

/// Clock that presentation feedback timestamps are expressed in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationClock {
    /// Feedback timestamps share the `CLOCK_MONOTONIC` domain used by ingest
    /// and render latency recorders.
    Monotonic,
    /// A compositor clock outside the monotonic domain; callers must not
    /// compare these timestamps against local latency records.
    Unrecognized(u32),
}

impl PresentationClock {
    /// Classifies a raw `clockid_t` announced by the compositor.
    #[must_use]
    pub const fn from_clock_id(clock_id: u32) -> Self {
        if clock_id == MONOTONIC_CLOCK_ID {
            Self::Monotonic
        } else {
            Self::Unrecognized(clock_id)
        }
    }

    /// Reports whether feedback timestamps can be compared against local
    /// monotonic latency records.
    #[must_use]
    pub const fn is_monotonic(self) -> bool {
        matches!(self, Self::Monotonic)
    }
}

/// One physical output discovered in the native display session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplayOutput {
    name: Option<String>,
    description: Option<String>,
    refresh_millihertz: Option<u32>,
    pixel_size: Option<(u32, u32)>,
    integer_scale: Option<u32>,
    logical_size: Option<(u32, u32)>,
    effective_scale_milli: Option<u32>,
}

impl DisplayOutput {
    /// Connector name such as `eDP-1`, when the compositor reports one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Human-readable output description, when the compositor reports one.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Current mode refresh rate in millihertz.
    #[must_use]
    pub const fn refresh_millihertz(&self) -> Option<u32> {
        self.refresh_millihertz
    }

    /// Current mode refresh interval in nanoseconds.
    ///
    /// Returns `None` when the compositor did not report a mode or reported a
    /// zero refresh rate.
    #[must_use]
    pub fn refresh_interval_nanos(&self) -> Option<u64> {
        self.refresh_millihertz
            .filter(|&millihertz| millihertz > 0)
            .map(|millihertz| 1_000_000_000_000 / u64::from(millihertz))
    }

    /// Current mode size in physical pixels.
    #[must_use]
    pub const fn pixel_size(&self) -> Option<(u32, u32)> {
        self.pixel_size
    }

    /// Integer buffer scale announced through `wl_output.scale`.
    #[must_use]
    pub const fn integer_scale(&self) -> Option<u32> {
        self.integer_scale
    }

    /// Logical compositor-space size announced through XDG output.
    #[must_use]
    pub const fn logical_size(&self) -> Option<(u32, u32)> {
        self.logical_size
    }

    /// Effective scale in thousandths (`1_000` is `1.0x`), derived from physical
    /// versus logical geometry and falling back to the announced integer
    /// scale. Deriving from geometry captures fractional scales that the
    /// integer `wl_output.scale` event cannot express.
    ///
    /// Returns `None` when neither source is available, and geometry with a
    /// zero logical width or height is rejected instead of dividing by zero.
    #[must_use]
    pub fn effective_scale_milli(&self) -> Option<u32> {
        if self.effective_scale_milli.is_some() {
            return self.effective_scale_milli;
        }
        if let (Some((pixel_width, _)), Some((logical_width, _))) =
            (self.pixel_size, self.logical_size)
            && logical_width > 0
        {
            let milli = u64::from(pixel_width) * 1_000 / u64::from(logical_width);
            return u32::try_from(milli).ok();
        }
        self.integer_scale.map(|scale| scale * 1_000)
    }
}

/// Display session state discovered by one probe.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplayEnvironment {
    outputs: Vec<DisplayOutput>,
    presentation_clock: Option<PresentationClock>,
}

impl DisplayEnvironment {
    /// Every output the compositor advertised during the probe.
    #[must_use]
    pub fn outputs(&self) -> &[DisplayOutput] {
        &self.outputs
    }

    /// The clock the compositor's presentation feedback is expressed in, when
    /// the compositor supports the presentation-time protocol.
    #[must_use]
    pub const fn presentation_clock(&self) -> Option<PresentationClock> {
        self.presentation_clock
    }
}

/// Reason native display geometry or presentation state could not be probed.
#[derive(Debug)]
pub enum DisplayTimingError {
    /// This crate implements no display adapter for the running target.
    UnsupportedPlatform,
    /// No native display session exists; the host is headless or only serves a
    /// session type this adapter does not implement.
    NoSession,
    /// The native display transport failed while probing.
    Transport,
}

impl fmt::Display for DisplayTimingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                formatter.write_str("no display timing adapter exists for this platform")
            }
            Self::NoSession => formatter.write_str("no native display session is available"),
            Self::Transport => formatter.write_str("the native display transport failed"),
        }
    }
}

impl Error for DisplayTimingError {}

/// Native display geometry and presentation-clock probe.
///
/// A probe is a short-lived read of compositor state: outputs can appear,
/// disappear, and change mode at any time, so callers repeat the probe on
/// output or DPI change notifications instead of caching the result.
pub struct NativeDisplayProbe;

impl NativeDisplayProbe {
    /// Reports whether this crate implements a native display probe for the
    /// target. As with the credential vault, this reports compile scope; a
    /// Linux host without a running Wayland session still fails at
    /// [`Self::probe`] with [`DisplayTimingError::NoSession`].
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        if cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )) {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    }

    /// Reads the current outputs and presentation clock from the native
    /// display session.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsupported, no Wayland session is
    /// listening, or a protocol round trip fails.
    pub fn probe() -> Result<DisplayEnvironment, DisplayTimingError> {
        #[cfg(target_os = "linux")]
        return probe_wayland();

        #[cfg(target_os = "windows")]
        return probe_windows();

        #[cfg(target_os = "macos")]
        return probe_macos();

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(DisplayTimingError::UnsupportedPlatform)
    }
}

/// One observation of the Windows Desktop Window Manager composition timeline.
///
/// DWM reports these values in the system QPC clock domain. They prove that the
/// desktop compositor advanced and identify frames it classified as displayed;
/// they do not prove photons reached a physical panel.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowsCompositionTiming {
    qpc_frequency_hz: u64,
    refresh_period_qpc: u64,
    vblank_qpc: u64,
    compose_qpc: u64,
    displayed_frame: u64,
    displayed_qpc: u64,
    completed_frame: u64,
    completed_qpc: u64,
    refresh_count: u64,
    frames_late: u64,
    frames_dropped: u64,
    frames_missed: u64,
}

#[cfg(target_os = "windows")]
impl WindowsCompositionTiming {
    /// System QPC frequency used by every timestamp in this sample.
    #[must_use]
    pub const fn qpc_frequency_hz(self) -> u64 {
        self.qpc_frequency_hz
    }

    /// DWM's current refresh period, converted from QPC ticks to nanoseconds.
    #[must_use]
    pub fn refresh_period_nanos(self) -> Option<u64> {
        qpc_ticks_to_nanos(self.refresh_period_qpc, self.qpc_frequency_hz)
    }

    /// QPC timestamp of DWM's most recent vertical blank observation.
    #[must_use]
    pub const fn vblank_qpc(self) -> u64 {
        self.vblank_qpc
    }

    /// QPC timestamp at which DWM began its most recent composition pass.
    #[must_use]
    pub const fn compose_qpc(self) -> u64 {
        self.compose_qpc
    }

    /// Identifier of the last frame DWM classified as displayed.
    #[must_use]
    pub const fn displayed_frame(self) -> u64 {
        self.displayed_frame
    }

    /// QPC timestamp of the composition pass that displayed that frame.
    #[must_use]
    pub const fn displayed_qpc(self) -> u64 {
        self.displayed_qpc
    }

    /// Identifier of the last frame DWM marked complete.
    #[must_use]
    pub const fn completed_frame(self) -> u64 {
        self.completed_frame
    }

    /// QPC timestamp at which DWM marked that frame complete.
    #[must_use]
    pub const fn completed_qpc(self) -> u64 {
        self.completed_qpc
    }

    /// DWM vertical refresh counter.
    #[must_use]
    pub const fn refresh_count(self) -> u64 {
        self.refresh_count
    }

    /// Frames DWM classified as late at the time of this observation.
    #[must_use]
    pub const fn frames_late(self) -> u64 {
        self.frames_late
    }

    /// Frames DWM classified as dropped at the time of this observation.
    #[must_use]
    pub const fn frames_dropped(self) -> u64 {
        self.frames_dropped
    }

    /// Refreshes DWM classified as missed at the time of this observation.
    #[must_use]
    pub const fn frames_missed(self) -> u64 {
        self.frames_missed
    }
}

/// Windows DWM composition timeline probe.
///
/// Windows 8.1 and later require the timing API's window argument to be null,
/// so samples describe the compositor's primary-output timeline rather than a
/// particular application window. [`Self::flush`] establishes a compositor
/// present boundary, but neither API is physical scanout instrumentation.
#[cfg(target_os = "windows")]
pub struct WindowsCompositionProbe {
    qpc_frequency_hz: u64,
}

#[cfg(target_os = "windows")]
impl WindowsCompositionProbe {
    /// Opens a probe after validating the system QPC clock and DWM timing API.
    ///
    /// # Errors
    ///
    /// Returns [`DisplayTimingError::Transport`] when QPC or DWM timing is
    /// unavailable.
    pub fn new() -> Result<Self, DisplayTimingError> {
        windows_composition::probe()
    }

    /// Reads the current DWM composition counters and QPC timestamps.
    ///
    /// # Errors
    ///
    /// Returns [`DisplayTimingError::Transport`] when DWM rejects the sample.
    pub fn sample(&self) -> Result<WindowsCompositionTiming, DisplayTimingError> {
        windows_composition::sample(self.qpc_frequency_hz)
    }

    /// Blocks until DWM's next present has incorporated outstanding DirectX
    /// surface updates.
    ///
    /// This is a compositor synchronization boundary, not a panel scanout
    /// timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`DisplayTimingError::Transport`] when DWM rejects the flush.
    pub fn flush(&self) -> Result<(), DisplayTimingError> {
        windows_composition::flush()
    }
}

#[cfg(target_os = "windows")]
fn qpc_ticks_to_nanos(ticks: u64, frequency_hz: u64) -> Option<u64> {
    if frequency_hz == 0 {
        return None;
    }
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(frequency_hz)).ok()
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows_composition {
    use super::{DisplayTimingError, WindowsCompositionProbe, WindowsCompositionTiming};
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
        System::Performance::QueryPerformanceFrequency,
    };

    pub(super) fn probe() -> Result<WindowsCompositionProbe, DisplayTimingError> {
        let mut frequency = 0_i64;
        if unsafe { QueryPerformanceFrequency(&raw mut frequency) } == 0 || frequency <= 0 {
            return Err(DisplayTimingError::Transport);
        }
        let probe = WindowsCompositionProbe {
            qpc_frequency_hz: frequency.cast_unsigned(),
        };
        probe.sample()?;
        Ok(probe)
    }

    pub(super) fn sample(
        qpc_frequency_hz: u64,
    ) -> Result<WindowsCompositionTiming, DisplayTimingError> {
        let mut timing = DWM_TIMING_INFO {
            cbSize: u32::try_from(size_of::<DWM_TIMING_INFO>())
                .map_err(|_| DisplayTimingError::Transport)?,
            ..DWM_TIMING_INFO::default()
        };
        if unsafe { DwmGetCompositionTimingInfo(ptr::null_mut(), &raw mut timing) } < 0 {
            return Err(DisplayTimingError::Transport);
        }
        Ok(WindowsCompositionTiming {
            qpc_frequency_hz,
            refresh_period_qpc: timing.qpcRefreshPeriod,
            vblank_qpc: timing.qpcVBlank,
            compose_qpc: timing.qpcCompose,
            displayed_frame: timing.cFrameDisplayed,
            displayed_qpc: timing.qpcFrameDisplayed,
            completed_frame: timing.cFrameComplete,
            completed_qpc: timing.qpcFrameComplete,
            refresh_count: timing.cRefresh,
            frames_late: timing.cFramesLate,
            frames_dropped: timing.cFramesDropped,
            frames_missed: timing.cFramesMissed,
        })
    }

    pub(super) fn flush() -> Result<(), DisplayTimingError> {
        if unsafe { DwmFlush() } < 0 {
            return Err(DisplayTimingError::Transport);
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn probe_windows() -> Result<DisplayEnvironment, DisplayTimingError> {
    let displays = display_info::DisplayInfo::all().map_err(|_| DisplayTimingError::Transport)?;
    let outputs = displays
        .into_iter()
        .filter_map(|display| {
            if display.width == 0
                || display.height == 0
                || !display.frequency.is_finite()
                || display.frequency <= 0.0
                || !display.scale_factor.is_finite()
                || display.scale_factor <= 0.0
            {
                return None;
            }
            let refresh_interval = Duration::from_secs_f32(display.frequency.recip());
            let refresh_millihertz =
                u32::try_from(1_000_000_000_000_u128.checked_div(refresh_interval.as_nanos())?)
                    .ok()
                    .filter(|&rate| (1..=1_000_000).contains(&rate))?;
            let scale_milli =
                u32::try_from(Duration::from_secs_f32(display.scale_factor).as_millis())
                    .ok()
                    .filter(|&scale| scale > 0)?;
            Some(DisplayOutput {
                name: Some(display.name),
                description: (!display.friendly_name.is_empty()).then_some(display.friendly_name),
                refresh_millihertz: Some(refresh_millihertz),
                pixel_size: Some((display.width, display.height)),
                integer_scale: None,
                logical_size: None,
                effective_scale_milli: Some(scale_milli),
            })
        })
        .collect();
    Ok(DisplayEnvironment {
        outputs,
        presentation_clock: None,
    })
}

#[cfg(target_os = "macos")]
fn probe_macos() -> Result<DisplayEnvironment, DisplayTimingError> {
    let displays = display_info::DisplayInfo::all().map_err(|_| DisplayTimingError::Transport)?;
    let outputs = displays
        .into_iter()
        .filter_map(|display| {
            if display.width == 0
                || display.height == 0
                || !display.frequency.is_finite()
                || display.frequency <= 0.0
                || !display.scale_factor.is_finite()
                || display.scale_factor <= 0.0
            {
                return None;
            }
            let refresh_interval = Duration::from_secs_f32(display.frequency.recip());
            let refresh_millihertz =
                u32::try_from(1_000_000_000_000_u128.checked_div(refresh_interval.as_nanos())?)
                    .ok()
                    .filter(|&rate| (1..=1_000_000).contains(&rate))?;
            let scale_milli =
                u32::try_from(Duration::from_secs_f32(display.scale_factor).as_millis())
                    .ok()
                    .filter(|&scale| scale > 0)?;
            Some(DisplayOutput {
                name: Some(display.name),
                description: (!display.friendly_name.is_empty()).then_some(display.friendly_name),
                refresh_millihertz: Some(refresh_millihertz),
                pixel_size: Some((display.width, display.height)),
                integer_scale: None,
                logical_size: None,
                effective_scale_milli: Some(scale_milli),
            })
        })
        .collect();
    Ok(DisplayEnvironment {
        outputs,
        presentation_clock: None,
    })
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct OutputProbe {
    output: DisplayOutput,
    proxy: Option<wl_output::WlOutput>,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct ProbeState {
    outputs: Vec<OutputProbe>,
    xdg_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    presentation_clock: Option<u32>,
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_output" => {
                let index = state.outputs.len();
                let proxy = registry.bind::<wl_output::WlOutput, _, _>(
                    name,
                    version.min(WL_OUTPUT_VERSION),
                    handle,
                    index,
                );
                state.outputs.push(OutputProbe {
                    output: DisplayOutput::default(),
                    proxy: Some(proxy),
                });
            }
            "zxdg_output_manager_v1" => {
                state.xdg_manager =
                    Some(registry.bind(name, version.min(XDG_OUTPUT_MANAGER_VERSION), handle, ()));
            }
            "wp_presentation" => {
                registry.bind::<wp_presentation::WpPresentation, _, _>(name, 1, handle, ());
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_output::WlOutput, usize> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(probe) = state.outputs.get_mut(index) else {
            return;
        };
        match event {
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } => {
                if matches!(flags, WEnum::Value(mode) if mode.contains(wl_output::Mode::Current))
                    && width > 0
                    && height > 0
                {
                    probe.output.refresh_millihertz = u32::try_from(refresh).ok();
                    probe.output.pixel_size = Some((width.cast_unsigned(), height.cast_unsigned()));
                }
            }
            wl_output::Event::Scale { factor } => {
                probe.output.integer_scale = u32::try_from(factor).ok().filter(|&scale| scale > 0);
            }
            wl_output::Event::Name { name } => {
                probe.output.name = Some(name);
            }
            wl_output::Event::Description { description } => {
                probe.output.description = Some(description);
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<zxdg_output_manager_v1::ZxdgOutputManagerV1, ()> for ProbeState {
    fn event(
        _: &mut Self,
        _: &zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _: zxdg_output_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for ProbeState {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(probe) = state.outputs.get_mut(index) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                if width > 0 && height > 0 {
                    probe.output.logical_size =
                        Some((width.cast_unsigned(), height.cast_unsigned()));
                }
            }
            zxdg_output_v1::Event::Name { name } => {
                probe.output.name = Some(name);
            }
            zxdg_output_v1::Event::Description { description } => {
                probe.output.description = Some(description);
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wp_presentation::WpPresentation, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wp_presentation::Event::ClockId { clk_id } = event else {
            return;
        };
        state.presentation_clock = Some(clk_id);
    }
}

#[cfg(target_os = "linux")]
fn probe_wayland() -> Result<DisplayEnvironment, DisplayTimingError> {
    let connection = Connection::connect_to_env().map_err(|error| match error {
        wayland_client::ConnectError::NoCompositor | wayland_client::ConnectError::NoWaylandLib => {
            DisplayTimingError::NoSession
        }
        wayland_client::ConnectError::InvalidFd => DisplayTimingError::Transport,
    })?;
    let display = connection.display();
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    let mut state = ProbeState::default();
    display.get_registry(&handle, ());
    queue
        .roundtrip(&mut state)
        .map_err(|_| DisplayTimingError::Transport)?;
    if let Some(manager) = &state.xdg_manager {
        for (index, probe) in state.outputs.iter().enumerate() {
            if let Some(proxy) = &probe.proxy {
                manager.get_xdg_output(proxy, &handle, index);
            }
        }
        queue
            .roundtrip(&mut state)
            .map_err(|_| DisplayTimingError::Transport)?;
    }
    Ok(DisplayEnvironment {
        outputs: state
            .outputs
            .into_iter()
            .map(|probe| probe.output)
            .collect(),
        presentation_clock: state
            .presentation_clock
            .map(PresentationClock::from_clock_id),
    })
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "windows"))]
    use super::DisplayTimingError;
    #[cfg(target_os = "linux")]
    use super::MONOTONIC_CLOCK_ID;
    use super::{DisplayEnvironment, DisplayOutput, NativeDisplayProbe, PresentationClock};
    use crate::CapabilityAvailability;

    fn output(
        pixel: Option<(u32, u32)>,
        logical: Option<(u32, u32)>,
        scale: Option<u32>,
    ) -> DisplayOutput {
        DisplayOutput {
            name: None,
            description: None,
            refresh_millihertz: None,
            pixel_size: pixel,
            integer_scale: scale,
            logical_size: logical,
            effective_scale_milli: None,
        }
    }

    #[test]
    fn derived_scale_captures_fractional_ratios() {
        assert_eq!(
            output(Some((2_880, 1_800)), Some((1_920, 1_200)), Some(1)).effective_scale_milli(),
            Some(1_500)
        );
        assert_eq!(
            output(Some((3_840, 2_160)), Some((1_920, 1_080)), Some(2)).effective_scale_milli(),
            Some(2_000)
        );
    }

    #[test]
    fn scale_falls_back_to_the_announced_integer() {
        assert_eq!(
            output(Some((2_560, 1_440)), None, Some(2)).effective_scale_milli(),
            Some(2_000)
        );
        assert_eq!(output(None, None, None).effective_scale_milli(), None);
    }

    #[test]
    fn zero_logical_geometry_never_divides() {
        assert_eq!(
            output(Some((1_920, 1_080)), Some((0, 0)), Some(1)).effective_scale_milli(),
            Some(1_000)
        );
    }

    #[test]
    fn refresh_interval_is_the_reciprocal_of_the_current_mode() {
        let mut at_144hz = output(None, None, None);
        at_144hz.refresh_millihertz = Some(144_000);
        assert_eq!(at_144hz.refresh_interval_nanos(), Some(6_944_444));

        let mut zero_mode = output(None, None, None);
        zero_mode.refresh_millihertz = Some(0);
        assert_eq!(zero_mode.refresh_interval_nanos(), None);
    }

    #[test]
    fn presentation_clock_classification_preserves_unknown_clocks() {
        assert_eq!(
            PresentationClock::from_clock_id(1),
            PresentationClock::Monotonic
        );
        assert!(PresentationClock::Monotonic.is_monotonic());
        assert_eq!(
            PresentationClock::from_clock_id(0),
            PresentationClock::Unrecognized(0)
        );
        assert!(!PresentationClock::Unrecognized(0).is_monotonic());
    }

    #[test]
    fn environment_exposes_only_discovered_state() {
        let environment = DisplayEnvironment::default();
        assert!(environment.outputs().is_empty());
        assert_eq!(environment.presentation_clock(), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_probe_reports_real_session_state_or_no_session() {
        assert_eq!(
            NativeDisplayProbe::availability(),
            CapabilityAvailability::Available
        );
        match NativeDisplayProbe::probe() {
            Ok(environment) => {
                for output in environment.outputs() {
                    if let Some(millihertz) = output.refresh_millihertz() {
                        assert!(millihertz > 0);
                        assert!(millihertz <= 1_000_000);
                    }
                    if let Some(scale) = output.effective_scale_milli() {
                        assert!(scale >= 1_000 / 8);
                        assert!(scale <= 8_000);
                    }
                }
                if let Some(clock) = environment.presentation_clock() {
                    match clock {
                        PresentationClock::Monotonic => assert!(clock.is_monotonic()),
                        PresentationClock::Unrecognized(clock_id) => {
                            assert_ne!(clock_id, MONOTONIC_CLOCK_ID);
                            assert!(!clock.is_monotonic());
                        }
                    }
                }
            }
            Err(DisplayTimingError::NoSession | DisplayTimingError::Transport) => {}
            Err(error) => panic!("unexpected probe failure: {error}"),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_probe_reports_validated_outputs_or_transport_failure() {
        assert_eq!(
            NativeDisplayProbe::availability(),
            CapabilityAvailability::Available
        );
        match NativeDisplayProbe::probe() {
            Ok(environment) => {
                for output in environment.outputs() {
                    if let Some(millihertz) = output.refresh_millihertz() {
                        assert!(millihertz > 0);
                        assert!(millihertz <= 1_000_000);
                    }
                    if let Some(scale) = output.effective_scale_milli() {
                        assert!(scale > 0);
                    }
                    assert!(output.pixel_size().is_some());
                }
                assert_eq!(environment.presentation_clock(), None);
            }
            Err(DisplayTimingError::Transport) => {}
            Err(error) => panic!("unexpected probe failure: {error}"),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    #[test]
    fn unsupported_targets_fail_explicitly() {
        assert_eq!(
            NativeDisplayProbe::availability(),
            CapabilityAvailability::Unavailable
        );
        assert!(matches!(
            NativeDisplayProbe::probe(),
            Err(DisplayTimingError::UnsupportedPlatform)
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_probe_reports_current_display_modes() {
        assert_eq!(
            NativeDisplayProbe::availability(),
            CapabilityAvailability::Available
        );
        let environment = NativeDisplayProbe::probe().expect("Windows display APIs are available");
        assert!(!environment.outputs().is_empty());
        for output in environment.outputs() {
            assert!(output.name().is_some_and(|name| !name.is_empty()));
            assert!(output.refresh_millihertz().is_some_and(|rate| rate > 0));
            assert!(output.pixel_size().is_some());
            assert!(
                output
                    .effective_scale_milli()
                    .is_some_and(|scale| scale > 0)
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn qpc_conversion_preserves_refresh_precision() {
        assert_eq!(
            super::qpc_ticks_to_nanos(166_667, 10_000_000),
            Some(16_666_700)
        );
        assert_eq!(super::qpc_ticks_to_nanos(1, 0), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_composition_probe_reports_dwm_timeline() {
        let probe = super::WindowsCompositionProbe::new()
            .expect("Windows DWM composition timing is available");
        let sample = probe.sample().expect("DWM timing sample succeeds");
        assert!(sample.qpc_frequency_hz() > 0);
        assert!(
            sample
                .refresh_period_nanos()
                .is_some_and(|period| period > 0)
        );
        assert!(sample.vblank_qpc() > 0);
        assert!(sample.compose_qpc() > 0);
        assert!(sample.refresh_count() > 0);
    }
}
