//! Unified-auth image captcha: where the picture is kept and how it is shown.
//!
//! Why this is its own module:
//! The SSO page can demand a captcha in the TUI, in the CLI and in the
//! seminar-room login. All three need the same two things: a file the user can
//! open in any image viewer, and a picture drawn straight in the terminal so
//! the file usually does not have to be opened at all.
//!
//! How the terminal picture works:
//! Each character cell is split into an upper and a lower half with `▀`: the
//! foreground colours the top half, the background the bottom. That gives two
//! square-ish pixels per cell, works in every truecolour terminal and inside
//! tmux, and needs no image protocol (kitty, sixel) that a multiplexer may
//! swallow.

use std::{
    fs,
    io::{self, BufRead as _, IsTerminal as _, Write as _},
    path::PathBuf,
    sync::OnceLock,
};

use anyhow::{Context as _, Result};
use image::{GrayImage, imageops::FilterType};

use crate::model::LoginCaptchaChallenge;

/// File stem of the saved captcha, under the state directory.
///
/// Why one fixed name:
/// A stable path can be left open in an image viewer that reloads on change,
/// and the directory never accumulates stale pictures.

const FILE_STEM: &str = "captcha";

/// Where a captcha picture with `extension` is written.

pub fn image_path(extension: &str) -> Result<PathBuf> {

    Ok(captcha_dir()?.join(format!("{FILE_STEM}.{extension}")))
}

#[cfg(not(test))]

fn captcha_dir() -> Result<PathBuf> {

    crate::paths::state_dir()
}

/// Tests write inside the build tree, never into the user's state directory.
#[cfg(test)]

fn captcha_dir() -> Result<PathBuf> {

    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-state"))
}

/// Writes the captcha picture to its fixed path and returns that path.
///
/// The extension follows the real format (the SSO serves PNG), so viewers that
/// trust the extension open it. The write goes through a temporary file and a
/// rename, so a viewer watching the path never reads half a picture.

pub fn save(bytes: &[u8]) -> Result<PathBuf> {

    let extension = match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::Gif) => "gif",
        Ok(image::ImageFormat::Bmp) => "bmp",
        Ok(image::ImageFormat::WebP) => "webp",
        _ => "img",
    };

    let path = image_path(extension)?;

    let parent = path.parent().context("验证码图片路径缺少上级目录")?;

    fs::create_dir_all(parent)
        .with_context(|| format!("创建验证码目录失败: {}", parent.display()))?;

    let staging = parent.join(format!(".{FILE_STEM}.{extension}.partial"));

    fs::write(&staging, bytes)
        .with_context(|| format!("写入验证码图片失败: {}", staging.display()))?;

    fs::rename(&staging, &path)
        .with_context(|| format!("写入验证码图片失败: {}", path.display()))?;

    Ok(path)
}

/// A picture prepared for half-block drawing: `rows[r][c]` is the grey level
/// of the top and bottom half of cell `(c, r)`.

#[derive(Clone, Debug, Eq, PartialEq)]

pub struct HalfBlocks {
    pub rows: Vec<Vec<(u8, u8)>>,
}

#[cfg(test)]

impl HalfBlocks {
    pub fn width(&self) -> usize {

        self.rows.first().map_or(0, Vec::len)
    }

    pub fn height(&self) -> usize {

        self.rows.len()
    }
}

/// Scales the picture to fit `max_cols` × `max_rows` cells, keeping its shape.
///
/// Why grey and stretched:
/// Captcha colours carry nothing, while contrast is what makes the glyphs
/// legible once the picture is shrunk to a few dozen cells. Stretching the
/// darkest pixel to black and the lightest to white also keeps the picture
/// readable on terminals that round truecolour down to 256 colours.
///
/// Returns `None` when the bytes are not a picture this build can decode or
/// the area is too small to hold anything.

pub fn half_blocks(bytes: &[u8], max_cols: usize, max_rows: usize) -> Option<HalfBlocks> {

    if max_cols == 0 || max_rows == 0 {

        return None;
    }

    let gray = image::load_from_memory(bytes).ok()?.to_luma8();

    let (width, height) = gray.dimensions();

    if width == 0 || height == 0 {

        return None;
    }

    // One cell holds one pixel across and two down. Never enlarge: blowing a
    // small picture up adds blur, not detail.
    let scale = (max_cols as f64 / f64::from(width))
        .min((max_rows * 2) as f64 / f64::from(height))
        .min(1.0);

    let target_width = ((f64::from(width) * scale).round() as u32).max(1);

    let target_height = ((f64::from(height) * scale).round() as u32).max(2);

    let scaled = image::imageops::resize(&gray, target_width, target_height, FilterType::Triangle);

    let stretched = stretch_contrast(scaled);

    let rows = (0..target_height.div_ceil(2))
        .map(|row| {

            (0..target_width)
                .map(|col| {

                    let top = stretched.get_pixel(col, row * 2)[0];

                    // An odd height leaves the last bottom half empty; white
                    // matches the captcha background.
                    let bottom = stretched
                        .get_pixel_checked(col, row * 2 + 1)
                        .map_or(255, |pixel| pixel[0]);

                    (top, bottom)
                })
                .collect()
        })
        .collect();

    Some(HalfBlocks { rows })
}

fn stretch_contrast(mut gray: GrayImage) -> GrayImage {

    let (min, max) = gray.pixels().fold((u8::MAX, u8::MIN), |(min, max), pixel| {

        (min.min(pixel[0]), max.max(pixel[0]))
    });

    if max > min {

        let span = f64::from(max - min);

        for pixel in gray.pixels_mut() {

            pixel[0] = ((f64::from(pixel[0] - min) / span) * 255.0).round() as u8;
        }
    }

    gray
}

/// The picture as ANSI truecolour text, one line per cell row.

pub fn ansi(blocks: &HalfBlocks) -> String {

    let mut out = String::new();

    for row in &blocks.rows {

        for &(top, bottom) in row {

            out.push_str(&format!(
                "\x1b[38;2;{top};{top};{top}m\x1b[48;2;{bottom};{bottom};{bottom}m▀"
            ));
        }

        out.push_str("\x1b[0m\n");
    }

    out
}

/// What the person at the terminal answered.

#[derive(Clone, Debug, Eq, PartialEq)]

pub enum Answer {
    Code(String),
    /// Fetch another picture for the same login.
    Refresh,
    Cancel,
}

/// Asks for one captcha. Runs on a blocking thread: it reads standard input.

pub type Prompt = fn(&LoginCaptchaChallenge) -> Answer;

static PROMPT: OnceLock<Prompt> = OnceLock::new();

/// Lets the login ask for a captcha instead of failing.
///
/// Why opt-in and process-wide:
/// Only an interactive CLI may block on standard input. The TUI owns the
/// terminal and asks through its own form, and an agent calling the CLI has
/// no one to answer, so neither installs a prompt; the login then reports the
/// captcha as an error that names the saved picture.

pub fn install_prompt(prompt: Prompt) {

    let _ = PROMPT.set(prompt);
}

pub fn installed_prompt() -> Option<Prompt> {

    PROMPT.get().copied()
}

/// Whether the terminal prompt may be used: a person must be able to see the
/// picture (stderr) and type the answer (stdin).

pub fn terminal_is_interactive() -> bool {

    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// The CLI prompt: draws the picture on stderr and reads one line from stdin.
///
/// Standard output is left untouched so `--json` output stays parseable.

pub fn terminal_prompt(challenge: &LoginCaptchaChallenge) -> Answer {

    let mut stderr = io::stderr().lock();

    let _ = writeln!(stderr);

    let _ = writeln!(stderr, "统一认证要求输入验证码。");

    if let Some(message) = &challenge.message {

        let _ = writeln!(stderr, "上次提交: {message}");
    }

    // Leave a column spare: a line exactly as wide as the terminal wraps on
    // some emulators and would break every row of the picture.
    let columns = crossterm::terminal::size()
        .map_or(80, |(columns, _)| usize::from(columns))
        .saturating_sub(1)
        .min(75);

    if let Some(blocks) = half_blocks(&challenge.image, columns, 15) {

        let _ = write!(stderr, "{}", ansi(&blocks));
    }

    if let Some(path) = &challenge.captcha_path {

        let _ = writeln!(stderr, "验证码图片: {path}");
    }

    let _ = write!(stderr, "输入验证码后回车（r 换一张，直接回车取消）: ");

    let _ = stderr.flush();

    drop(stderr);

    let mut line = String::new();

    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {

        return Answer::Cancel;
    }

    parse_answer(&line)
}

fn parse_answer(line: &str) -> Answer {

    match line.trim() {
        "" => Answer::Cancel,
        "r" | "R" => Answer::Refresh,
        code => Answer::Code(code.to_string()),
    }
}

/// Normalises a typed captcha the way the SSO page does before submitting.
///
/// The page's own script sends `captcha.trim().toLocaleLowerCase()`, so a
/// code typed with capitals must be lowered here too.

pub fn normalize(code: &str) -> String {

    code.trim().to_lowercase()
}

#[cfg(test)]

mod tests {

    use super::*;

    fn png(width: u32, height: u32, shade: impl Fn(u32, u32) -> u8) -> Vec<u8> {

        let image = GrayImage::from_fn(width, height, |x, y| image::Luma([shade(x, y)]));

        let mut bytes = Vec::new();

        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();

        bytes
    }

    #[test]

    fn fits_the_sso_picture_into_the_requested_cells_keeping_its_shape() {

        // The SSO serves 150×60 pictures.
        let bytes = png(150, 60, |x, _| if x < 75 { 0 } else { 255 });

        let blocks = half_blocks(&bytes, 75, 15).unwrap();

        assert_eq!((blocks.width(), blocks.height()), (75, 15));

        let narrow = half_blocks(&bytes, 60, 40).unwrap();

        assert_eq!((narrow.width(), narrow.height()), (60, 12));
    }

    #[test]

    fn never_enlarges_and_stretches_contrast_to_full_range() {

        let bytes = png(10, 4, |x, _| if x < 5 { 100 } else { 150 });

        let blocks = half_blocks(&bytes, 80, 40).unwrap();

        assert_eq!((blocks.width(), blocks.height()), (10, 2));

        assert_eq!(blocks.rows[0][0], (0, 0));

        assert_eq!(blocks.rows[0][9], (255, 255));
    }

    #[test]

    fn rejects_bytes_that_are_not_a_picture() {

        assert!(half_blocks(b"<html>login</html>", 40, 10).is_none());

        assert!(half_blocks(&png(4, 4, |_, _| 0), 0, 10).is_none());
    }

    #[test]

    fn ansi_ends_every_row_with_a_reset() {

        let blocks = HalfBlocks {
            rows: vec![vec![(0, 255)], vec![(255, 0)]],
        };

        let text = ansi(&blocks);

        assert_eq!(text.matches("\x1b[0m\n").count(), 2);

        assert!(text.starts_with("\x1b[38;2;0;0;0m\x1b[48;2;255;255;255m▀"));
    }

    #[test]

    fn answers_and_codes_are_normalised_like_the_sso_page() {

        assert_eq!(parse_answer("  22V37n \n"), Answer::Code("22V37n".into()));

        assert_eq!(parse_answer("r\n"), Answer::Refresh);

        assert_eq!(parse_answer("\n"), Answer::Cancel);

        assert_eq!(normalize(" 22V37N "), "22v37n");
    }
}
