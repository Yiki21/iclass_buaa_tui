//! Shared terminal palette and motion helpers.
//!
//! Why:
//! Colors used to be picked inline at each call site, which produced one flat
//! color everywhere and no way to tell a heading from a hint. Routing color
//! through named roles keeps meaning stable as screens are added.
//!
//! How:
//! Truecolor (24-bit) values, deliberately desaturated. btop stays readable
//! because its greys and blues are quiet and only the data carries saturation.
//! Every animated helper derives from the frame counter, so nothing keeps its
//! own timer and motion only appears where work is actually happening.

use ratatui::style::{Color, Style};

/// Builds a 24-bit color from its components.

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {

    Color::Rgb(r, g, b)
}

/// Primary accent: a soft cyan for focused frames and titles.

pub const ACCENT: Color = rgb(96, 180, 188);

/// Secondary accent: muted amber for field labels.

pub const ACCENT_WARM: Color = rgb(196, 168, 116);

/// Bright body text.

pub const TEXT: Color = rgb(205, 212, 222);

/// Muted text for hints and placeholders.

pub const MUTED: Color = rgb(122, 132, 148);

/// Positive outcome.

pub const OK: Color = rgb(132, 190, 148);

/// Caution.

pub const WARN: Color = rgb(214, 183, 118);

/// Failure.

pub const ERROR: Color = rgb(214, 124, 134);

/// Informational values.

pub const INFO: Color = rgb(132, 166, 208);

/// Selection background.

pub const SELECTION_BG: Color = rgb(58, 96, 110);

/// Foreground drawn on top of [`SELECTION_BG`].

pub const SELECTION_FG: Color = rgb(232, 238, 245);

/// Border of the panel that currently holds focus.

pub const BORDER_FOCUS: Color = ACCENT;

/// Border of unfocused panels.

pub const BORDER_IDLE: Color = rgb(72, 82, 98);

/// Ordered ramp used for the progress bar gradient.

pub const RAMP: [Color; 5] = [
    rgb(96, 150, 188),
    rgb(96, 180, 188),
    rgb(132, 190, 148),
    rgb(196, 180, 116),
    rgb(190, 138, 148),
];

/// Animation spinner shown while a background job runs.

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Blends two colors, `t` running from 0.0 (start) to 1.0 (end).
///
/// Why:
/// Gradients need interpolation, and terminals do not blend for us, so the
/// ramp has to be computed here from the same 24-bit values the palette uses.

pub fn mix(start: Color, end: Color, t: f32) -> Color {

    let t = t.clamp(0.0, 1.0);

    let (Color::Rgb(sr, sg, sb), Color::Rgb(er, eg, eb)) = (start, end) else {

        return start;
    };

    let blend = |a: u8, b: u8| -> u8 {

        (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
    };

    rgb(blend(sr, er), blend(sg, eg), blend(sb, eb))
}

/// Samples the shared ramp at position `t` in `0.0..=1.0`.

pub fn ramp(t: f32) -> Color {

    let t = t.clamp(0.0, 1.0);

    let scaled = t * (RAMP.len() - 1) as f32;

    let index = (scaled.floor() as usize).min(RAMP.len() - 2);

    mix(RAMP[index], RAMP[index + 1], scaled - scaled.floor())
}

/// A spinner frame for the given tick, plus its color.
///
/// Why:
/// Motion should mark real activity. This is the only animation in the UI, so
/// it stays a small, steady glyph rather than a moving banner.

pub fn spinner(tick: u64) -> (&'static str, Style) {

    let frame = SPINNER[(tick as usize / 2) % SPINNER.len()];

    (frame, Style::new().fg(ACCENT))
}

/// A live activity badge: a spinner plus label while busy, a quiet dot when idle.
///
/// How:
/// Only callers that know a job is running pass `true`, so an idle screen stays
/// completely still and motion keeps meaning "something is happening".

pub fn activity_badge(tick: u64, busy: bool, label: &str) -> ratatui::text::Span<'static> {

    activity_badge_styled(tick, busy, label, muted_style())
}

/// Same as [`activity_badge`] but with a caller-chosen idle style.
///
/// Why:
/// An idle state is not always neutral. "离线可读" is a good outcome and should
/// keep its green even when nothing is spinning.

pub fn activity_badge_styled(
    tick: u64,
    busy: bool,
    label: &str,
    idle: Style,
) -> ratatui::text::Span<'static> {

    use ratatui::text::Span;

    if busy {

        let (frame, style) = spinner(tick);

        Span::styled(format!("{frame} {label}"), style)
    } else {

        Span::styled(format!("· {label}"), idle)
    }
}

/// Progress glyph span set for a `done / total` ratio, shaded along the ramp.
///
/// Why:
/// A single glance at a partly filled bar conveys more than the numbers beside
/// it, and the gradient keeps a saturated fill from looking like a solid block.

pub fn progress_bar(done: usize, total: usize, width: usize) -> Vec<ratatui::text::Span<'static>> {

    use ratatui::text::Span;

    if width == 0 {

        return Vec::new();
    }

    let ratio = if total == 0 {

        0.0
    } else {

        (done as f32 / total as f32).clamp(0.0, 1.0)
    };

    let filled = (ratio * width as f32).round() as usize;

    (0..width)
        .map(|index| {

            let position = if width <= 1 {

                0.0
            } else {

                index as f32 / (width - 1) as f32
            };

            if index < filled {

                Span::styled("█", Style::new().fg(ramp(position)))
            } else {

                Span::styled("░", Style::new().fg(BORDER_IDLE))
            }
        })
        .collect()
}

/// Style for a focused panel title.

pub const fn title_style() -> Style {

    Style::new().fg(ACCENT)
}

/// Style for an unfocused panel title.

pub const fn subtitle_style() -> Style {

    Style::new().fg(MUTED)
}

/// Style for the currently selected list row.

pub const fn selection_style() -> Style {

    Style::new().fg(SELECTION_FG).bg(SELECTION_BG)
}

/// Style for a field label.

pub const fn label_style() -> Style {

    Style::new().fg(ACCENT_WARM)
}

/// Style for body text.

pub const fn text_style() -> Style {

    Style::new().fg(TEXT)
}

/// Style for hints and placeholders.

pub const fn muted_style() -> Style {

    Style::new().fg(MUTED)
}
