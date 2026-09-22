//! Shared terminal palette and motion helpers.
//!
//! Why:
//! Colors used to be picked inline at each call site, which produced one flat
//! color everywhere and no way to tell a heading from a hint. Routing color
//! through named roles keeps meaning stable as screens are added, and a shared
//! ramp gives the UI the vivid, themed look of tools like btop.
//!
//! How:
//! Truecolor (24-bit) values, chosen to stay legible on both dark and light
//! terminals. Every animated helper derives from the frame counter so nothing
//! keeps its own timer, and animation only appears where work is happening.

use ratatui::style::{Color, Modifier, Style};

/// Builds a 24-bit color from its components.

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {

    Color::Rgb(r, g, b)
}

/// Primary accent: the signature cyan used for focused frames and titles.

pub const ACCENT: Color = rgb(0, 215, 215);

/// Tertiary accent: warm amber for calls to action.

pub const ACCENT_WARM: Color = rgb(245, 175, 65);

/// Bright body text.

pub const TEXT: Color = rgb(226, 232, 240);

/// Muted text for hints and placeholders.

pub const MUTED: Color = rgb(110, 122, 143);

/// Positive outcome.

pub const OK: Color = rgb(105, 240, 140);

/// Caution.

pub const WARN: Color = rgb(255, 200, 75);

/// Failure.

pub const ERROR: Color = rgb(255, 95, 115);

/// Informational values.

pub const INFO: Color = rgb(120, 175, 255);

/// Selection background.

pub const SELECTION_BG: Color = rgb(0, 145, 165);

/// Foreground drawn on top of [`SELECTION_BG`].

pub const SELECTION_FG: Color = rgb(6, 12, 20);

/// Border of the panel that currently holds focus.

pub const BORDER_FOCUS: Color = ACCENT;

/// Border of unfocused panels.

pub const BORDER_IDLE: Color = rgb(58, 68, 88);

/// Ordered ramp used for gradients across titles and bars.

pub const RAMP: [Color; 6] = [
    rgb(0, 200, 255),
    rgb(0, 215, 215),
    rgb(110, 235, 165),
    rgb(255, 220, 110),
    rgb(255, 140, 120),
    rgb(205, 95, 245),
];

/// Animation spinner shown while a background job runs.

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Blends two colors, `t` running from 0.0 (start) to 1.0 (end).
///
/// Why:
/// Gradients need interpolation. Terminals do not blend for us, so a smooth
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
/// Motion should mark real activity. Pairing the glyph with the accent color
/// makes an in-flight request obvious in a screen full of static text.

pub fn spinner(tick: u64) -> (&'static str, Style) {

    let frame = SPINNER[(tick as usize / 2) % SPINNER.len()];

    (frame, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// Splits `text` into per-character spans colored along the shared ramp.
///
/// Why:
/// This is the btop-style gradient used for headings. Coloring per character
/// keeps the effect smooth at any width without measuring beforehand.

pub fn gradient_text(text: &str, offset: f32) -> Vec<ratatui::text::Span<'static>> {

    use ratatui::text::Span;

    let characters = text.chars().count();

    text.chars()
        .enumerate()
        .map(|(index, character)| {

            let position = if characters <= 1 {

                0.0
            } else {

                index as f32 / (characters - 1) as f32
            };

            let color = ramp((position + offset).rem_euclid(1.0));

            Span::styled(
                character.to_string(),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

/// A live activity badge: spinner plus label, or a quiet dot when idle.
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

/// Progress glyph pair for a `done / total` ratio, colored along the ramp.

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

    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Style for an unfocused panel title.

pub const fn subtitle_style() -> Style {

    Style::new().fg(MUTED)
}

/// Style for the currently selected list row.

pub const fn selection_style() -> Style {

    Style::new()
        .fg(SELECTION_FG)
        .bg(SELECTION_BG)
        .add_modifier(Modifier::BOLD)
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
