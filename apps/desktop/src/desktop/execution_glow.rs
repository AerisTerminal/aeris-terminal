//! A multicolor glow along the terminal window's edges when a new fill is announced, the way
//! a voice assistant lights the screen edges: four colors ease in from the edges over a fine
//! grid of twinkling dots, travel clockwise around the window and ease out.
//!
//! The glow is a presentation of the same fill announcement the fill toast shows; the trading
//! owner stays authoritative for the fill. `with_animation` collapses to its final, invisible
//! frame under reduced motion, so the glow never shows then.

use super::*;
use gpui::{BoxShadow, Rgba, canvas, fill, size};
use std::f32::consts::TAU;

const GLOW_DURATION: Duration = Duration::from_millis(1_400);
/// Share of the glow spent easing in, and the share after which it eases out.
const GLOW_RISE: f32 = 0.3;
const GLOW_FADE_FROM: f32 = 0.5;
const GLOW_PEAK_OPACITY: f32 = 0.85;
/// How far each edge's light reaches into the window before its blur.
const GLOW_EDGE_DEPTH: f32 = 5.0;
const GLOW_BLUR: f32 = 24.0;
/// How many edges the colors travel clockwise over one glow.
const GLOW_TRAVEL: f32 = 1.5;

const DOT_SPACING: f32 = 9.0;
const DOT_SIZE: f32 = 2.0;
/// Each dot row's distance from the window edge and its strength.
const DOT_ROWS: [(f32, f32); 3] = [(3.0, 1.0), (9.0, 0.55), (15.0, 0.25)];
/// Top and bottom rows stop this far from the corners, where the side rows already run.
const DOT_CORNER: f32 = 18.0;
/// How far a dot drifts in and out with the passing wave.
const DOT_RIPPLE: f32 = 1.5;
/// Twinkle waves along one edge, and how many of them pass during one glow.
const DOT_WAVES_PER_EDGE: f32 = 6.0;
const DOT_WAVE_PASSES: f32 = 3.0;
/// Larger windows are treated as this long per edge, which bounds the dots painted per frame.
const MAXIMUM_EDGE_LENGTH: f32 = 8_192.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExecutionGlow {
    sequence: u64,
    started: Instant,
}

/// The newest glow; a later fill restarts it, so at most one glow exists per window.
#[derive(Debug, Default)]
pub(super) struct ExecutionGlows {
    current: Option<ExecutionGlow>,
    next_sequence: u64,
}

impl ExecutionGlows {
    pub(super) fn start(&mut self, now: Instant) {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.current = Some(ExecutionGlow {
            sequence: self.next_sequence,
            started: now,
        });
    }

    pub(super) fn active(&self, now: Instant) -> Option<ExecutionGlow> {
        self.current
            .filter(|glow| now.saturating_duration_since(glow.started) < GLOW_DURATION)
    }
}

/// Eases with zero speed and acceleration at both ends, so the glow never pops in or out.
fn smootherstep(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * value * (value * (value * 6.0 - 15.0) + 10.0)
}

fn glow_opacity(progress: f32) -> f32 {
    let strength = if progress < GLOW_RISE {
        smootherstep(progress / GLOW_RISE)
    } else if progress < GLOW_FADE_FROM {
        1.0
    } else {
        1.0 - smootherstep((progress - GLOW_FADE_FROM) / (1.0 - GLOW_FADE_FROM))
    };
    GLOW_PEAK_OPACITY * strength
}

/// The color at `position` around the palette loop, blending between neighbors.
fn palette_color(palette: &[Rgba; 4], position: f32) -> Hsla {
    let wrapped = position.rem_euclid(4.0);
    let (from, next) = match wrapped {
        value if value < 1.0 => (palette[0], palette[1]),
        value if value < 2.0 => (palette[1], palette[2]),
        value if value < 3.0 => (palette[2], palette[3]),
        _ => (palette[3], palette[0]),
    };
    let blend = wrapped.fract();
    let mix = |a: f32, b: f32| a + (b - a) * blend;
    Hsla::from(Rgba {
        r: mix(from.r, next.r),
        g: mix(from.g, next.g),
        b: mix(from.b, next.b),
        a: mix(from.a, next.a),
    })
}

/// Left, top, right and bottom inset shadows, each lit from its own edge.
fn edge_shadows(palette: &[Rgba; 4], progress: f32) -> Vec<BoxShadow> {
    let travel = GLOW_TRAVEL * progress;
    let blur = GLOW_BLUR * (0.8 + 0.2 * glow_opacity(progress) / GLOW_PEAK_OPACITY);
    [
        (0.0, GLOW_EDGE_DEPTH, 0.0),
        (1.0, 0.0, GLOW_EDGE_DEPTH),
        (2.0, -GLOW_EDGE_DEPTH, 0.0),
        (3.0, 0.0, -GLOW_EDGE_DEPTH),
    ]
    .into_iter()
    .map(|(phase, x, y)| {
        BoxShadow::new(px(x), px(y), palette_color(palette, phase - travel))
            .blur_radius(px(blur))
            .inset()
    })
    .collect()
}

/// One dot of the edge grid, in window-local logical pixels. `position` runs clockwise around
/// the edges in edge units, matching the shadow phases: 0 is the left edge's middle.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FabricDot {
    x: f32,
    y: f32,
    position: f32,
    strength: f32,
}

/// The window edges in clockwise order; each runs clockwise, so the left edge runs upward.
#[derive(Clone, Copy)]
enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    /// Where the edge starts in clockwise edge units; its middle is its shadow phase.
    fn phase_start(self) -> f32 {
        match self {
            Self::Left => -0.5,
            Self::Top => 0.5,
            Self::Right => 1.5,
            Self::Bottom => 2.5,
        }
    }

    /// The point `along` the edge (clockwise) and `depth` in from it.
    fn place(self, along: f32, depth: f32, width: f32, height: f32) -> (f32, f32) {
        match self {
            Self::Left => (depth, height - along),
            Self::Top => (along, depth),
            Self::Right => (width - depth, along),
            Self::Bottom => (width - along, height - depth),
        }
    }
}

fn fabric_dots(width: f32, height: f32, progress: f32) -> Vec<FabricDot> {
    let mut dots = Vec::new();
    if !(width.is_finite() && height.is_finite())
        || width <= 2.0 * DOT_CORNER
        || height <= 2.0 * DOT_CORNER
    {
        return dots;
    }
    let width = width.min(MAXIMUM_EDGE_LENGTH);
    let height = height.min(MAXIMUM_EDGE_LENGTH);
    for edge in [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom] {
        let horizontal = matches!(edge, Edge::Top | Edge::Bottom);
        let length = if horizontal { width } else { height };
        let phase = edge.phase_start();
        let mut along = DOT_SPACING * 0.5;
        while along < length {
            let in_corner = horizontal && (along < DOT_CORNER || along > length - DOT_CORNER);
            if !in_corner {
                let position = phase + along / length;
                for (depth, row_strength) in DOT_ROWS {
                    let wave = TAU * (position * DOT_WAVES_PER_EDGE - progress * DOT_WAVE_PASSES)
                        + depth * 0.35;
                    let depth = (depth + DOT_RIPPLE * wave.cos()).max(1.0);
                    let (x, y) = edge.place(along, depth, width, height);
                    dots.push(FabricDot {
                        x,
                        y,
                        position,
                        strength: row_strength * (0.55 + 0.45 * wave.sin()),
                    });
                }
            }
            along += DOT_SPACING;
        }
    }
    dots
}

/// A window-sized layer that takes no pointer input.
pub(super) fn execution_glow_layer(glow: ExecutionGlow, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let palette = [
        colors.swatch_blue,
        colors.swatch_red,
        colors.swatch_amber,
        colors.swatch_green,
    ]
    .map(|color| Rgba::from(gpui_color(color)));
    div()
        .absolute()
        .inset_0()
        .opacity(0.0)
        .with_animation(
            ("execution_glow", glow.sequence),
            Animation::new(GLOW_DURATION),
            move |layer, progress| {
                let travel = GLOW_TRAVEL * progress;
                layer
                    .opacity(glow_opacity(progress))
                    .shadow(edge_shadows(&palette, progress))
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, (), window, _| {
                                let dots = fabric_dots(
                                    f32::from(bounds.size.width),
                                    f32::from(bounds.size.height),
                                    progress,
                                );
                                for dot in dots {
                                    let mut color = palette_color(&palette, dot.position - travel);
                                    color.a *= dot.strength;
                                    let origin = bounds.origin
                                        + point(
                                            px(dot.x - DOT_SIZE / 2.0),
                                            px(dot.y - DOT_SIZE / 2.0),
                                        );
                                    window.paint_quad(
                                        fill(
                                            Bounds::new(origin, size(px(DOT_SIZE), px(DOT_SIZE))),
                                            color,
                                        )
                                        .corner_radii(px(DOT_SIZE / 2.0)),
                                    );
                                }
                            },
                        )
                        .size_full(),
                    )
            },
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glow_lasts_only_its_duration_and_a_later_fill_restarts_it() {
        let mut glows = ExecutionGlows::default();
        let now = Instant::now();
        glows.start(now);
        let first = glows.active(now).map(|glow| glow.sequence);
        assert!(first.is_some());
        assert!(glows.active(now + GLOW_DURATION).is_none());
        let later = now + Duration::from_millis(900);
        glows.start(later);
        let second = glows.active(later + Duration::from_millis(900));
        assert!(second.is_some());
        assert_ne!(second.map(|glow| glow.sequence), first);
    }

    #[test]
    fn the_glow_eases_in_holds_and_eases_out_to_nothing() {
        assert_eq!(glow_opacity(0.0), 0.0);
        assert!(glow_opacity(0.05) < 0.05 * GLOW_PEAK_OPACITY);
        assert!((glow_opacity(0.4) - GLOW_PEAK_OPACITY).abs() < f32::EPSILON);
        assert!(glow_opacity(0.97) < 0.03 * GLOW_PEAK_OPACITY);
        assert_eq!(glow_opacity(1.0), 0.0);
    }

    #[test]
    fn colors_blend_between_neighbors_and_wrap_around_the_loop() {
        let palette = [
            Rgba {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            Rgba {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
            Rgba {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
            Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        ];
        let halfway = Rgba::from(palette_color(&palette, 0.5));
        assert!((halfway.r - 0.5).abs() < 0.01 && (halfway.g - 0.5).abs() < 0.01);
        let wrapped = Rgba::from(palette_color(&palette, -1.0));
        assert!(wrapped.r < 0.01 && wrapped.g < 0.01 && wrapped.b < 0.01);
    }

    #[test]
    fn dots_stay_in_the_thin_band_along_the_edges() {
        let (width, height) = (1_200.0, 800.0);
        let dots = fabric_dots(width, height, 0.4);
        assert_ne!(dots.len(), 0);
        let band = 15.0 + DOT_RIPPLE;
        for dot in &dots {
            let edge_distance = dot.x.min(dot.y).min(width - dot.x).min(height - dot.y);
            assert!(edge_distance <= band + f32::EPSILON, "{dot:?}");
            assert!((0.0..=1.0).contains(&dot.strength), "{dot:?}");
        }
    }

    #[test]
    fn huge_or_tiny_windows_paint_a_bounded_number_of_dots() {
        assert_eq!(fabric_dots(20.0, 20.0, 0.5).len(), 0);
        assert_eq!(fabric_dots(f32::INFINITY, 800.0, 0.5).len(), 0);
        let most = fabric_dots(1.0e9, 1.0e9, 0.5).len();
        let perimeter = 4.0 * MAXIMUM_EDGE_LENGTH;
        let bound = (perimeter / DOT_SPACING + 4.0) * 3.0;
        assert!(most > 0);
        assert!(f64::from(u32::try_from(most).expect("dot count fits")) <= f64::from(bound));
    }
}
