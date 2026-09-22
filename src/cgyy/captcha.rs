//! Solver for the seminar-room slider captcha.
//!
//! Why:
//! Reserving a room requires passing a `blockPuzzle` captcha: the service sends
//! a background image and a puzzle piece, and expects the horizontal offset
//! where the piece belongs. Solving it by hand would make unattended
//! reservation impossible, and this is the same approach the desktop client
//! uses.
//!
//! How:
//! The piece image carries an alpha mask marking which pixels are real. That
//! mask is cropped to the piece, then both images are reduced to edges. The
//! piece is slid across the background and scored on how well its edges align
//! with background edges inside the mask, rewarding overlap and penalising
//! collisions with flat areas.

use aes::{
    Aes128,
    cipher::{BlockCipherEncrypt, KeyInit},
};

// Aliased because `aes::cipher::Array` is a different type from the block
// array used by the cipher, and both appear in this module.
use aes::cipher::Array as BlockArray;
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

/// One captcha challenge returned by the service.

#[derive(Clone, Debug, Default)]

pub struct CaptchaChallenge {
    pub secret_key: String,
    pub token:      String,
    /// Background image, base64 (optionally a data URL).
    pub original:   String,
    /// Puzzle piece, base64 (optionally a data URL).
    pub jigsaw:     String,
}

/// A solved captcha, ready to be sent with the reservation.

#[derive(Clone, Debug, Default)]

pub struct SolvedCaptcha {
    pub move_distance:        u32,
    pub point_json_data:      String,
    pub point_json:           String,
    pub captcha_verification: String,
    /// Challenge token, echoed back to the verify endpoint.
    pub token:                String,
}

/// A decoded image reduced to what the solver needs.

struct Raster {
    width:  usize,
    height: usize,
    /// ARGB pixels, one per position.
    argb:   Vec<u32>,
}

/// Reads a base64 image into an ARGB raster.
///
/// Why:
/// Decoding lives here rather than at the call site so a malformed payload
/// reports as a captcha error instead of a generic image failure.

fn decode_image(encoded: &str) -> Result<Raster> {

    // A data URL prefix may or may not be present; take whatever follows the
    // last one.
    let trimmed = match encoded.rsplit_once("base64,") {
        Some((_, tail)) => tail,
        None => encoded,
    }
    .trim();

    let bytes = BASE64
        .decode(trimmed)
        .map_err(|error| anyhow!("验证码图片不是有效的 base64: {error}"))?;

    let image = image::load_from_memory(&bytes)
        .map_err(|error| anyhow!("验证码图片解码失败: {error}"))?
        .to_rgba8();

    let (width, height) = image.dimensions();

    let mut argb = Vec::with_capacity((width * height) as usize);

    for pixel in image.pixels() {

        let [r, g, b, a] = pixel.0;

        argb.push((u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
    }

    Ok(Raster {
        width: width as usize,
        height: height as usize,
        argb,
    })
}

fn luminance(pixel: u32) -> i32 {

    let r = ((pixel >> 16) & 0xff) as i32;

    let g = ((pixel >> 8) & 0xff) as i32;

    let b = (pixel & 0xff) as i32;

    (r * 30 + g * 59 + b * 11) / 100
}

fn to_gray(raster: &Raster) -> Vec<Vec<i32>> {

    (0..raster.height)
        .map(|y| {

            (0..raster.width)
                .map(|x| luminance(raster.argb[y * raster.width + x]))
                .collect()
        })
        .collect()
}

/// Marks the pixels that belong to the piece.
///
/// How:
/// The piece is drawn on transparency, so alpha is the primary signal. Some
/// deliveries use a near-white background instead, which the luminance
/// threshold catches.

fn build_mask(raster: &Raster) -> Vec<Vec<bool>> {

    (0..raster.height)
        .map(|y| {

            (0..raster.width)
                .map(|x| {

                    let pixel = raster.argb[y * raster.width + x];

                    let alpha = (pixel >> 24) & 0xff;

                    if alpha > 10 {

                        return true;
                    }

                    luminance(pixel) < 250
                })
                .collect()
        })
        .collect()
}

/// Marks strong discontinuities, which are what the alignment scores against.

fn edge_detect(gray: &[Vec<i32>]) -> Vec<Vec<i32>> {

    let height = gray.len();

    let width = gray.first().map_or(0, Vec::len);

    (0..height)
        .map(|y| {

            (0..width)
                .map(|x| {

                    let center = gray[y][x];

                    let right = gray[y][(x + 1).min(width - 1)];

                    let down = gray[(y + 1).min(height - 1)][x];

                    let edge = (center - right).abs() + (center - down).abs();

                    if edge > 35 { 255 } else { 0 }
                })
                .collect()
        })
        .collect()
}

/// Tightest box containing every masked pixel.

fn bounding_box(mask: &[Vec<bool>]) -> Option<(usize, usize, usize, usize)> {

    let mut min_x = usize::MAX;

    let mut min_y = usize::MAX;

    let mut max_x = 0;

    let mut max_y = 0;

    let mut found = false;

    for (y, row) in mask.iter().enumerate() {

        for (x, &set) in row.iter().enumerate() {

            if !set {

                continue;
            }

            found = true;

            min_x = min_x.min(x);

            min_y = min_y.min(y);

            max_x = max_x.max(x);

            max_y = max_y.max(y);
        }
    }

    found.then_some((min_x, min_y, max_x, max_y))
}

fn crop<T: Copy>(source: &[Vec<T>], bounds: (usize, usize, usize, usize)) -> Vec<Vec<T>> {

    let (min_x, min_y, max_x, max_y) = bounds;

    (min_y..=max_y)
        .map(|y| (min_x..=max_x).map(|x| source[y][x]).collect())
        .collect()
}

/// Finds the horizontal offset where the piece fits the background.
///
/// Why:
/// This is the whole captcha. A wrong offset fails verification and the
/// reservation cannot proceed, so the scoring is deliberately conservative:
/// it rewards piece edges landing on background edges and penalises them
/// landing on flat background.

fn solve_offset(background: &Raster, piece: &Raster) -> Result<u32> {

    let background_gray = to_gray(background);

    let piece_gray = to_gray(piece);

    let mask = build_mask(piece);

    let bounds =
        bounding_box(&mask).ok_or_else(|| anyhow!("验证码拼图缺少有效遮罩，无法定位缺口"))?;

    let cropped_piece = crop(&piece_gray, bounds);

    let cropped_mask = crop(&mask, bounds);

    let background_edges = edge_detect(&background_gray);

    let piece_edges = edge_detect(&cropped_piece);

    let piece_height = piece_edges.len();

    let piece_width = piece_edges.first().map_or(0, Vec::len);

    if piece_height == 0 || piece_width == 0 {

        bail!("验证码拼图尺寸为空");
    }

    let background_height = background_edges.len();

    let background_width = background_edges.first().map_or(0, Vec::len);

    if piece_height > background_height || piece_width > background_width {

        bail!("验证码拼图比背景图还大，无法匹配");
    }

    let y_max = background_height - piece_height;

    let x_max = background_width - piece_width;

    let mut best_score = f64::NEG_INFINITY;

    let mut best_x = 0_usize;

    for y in 0..=y_max {

        for x in 0..=x_max {

            let mut score = 0.0_f64;

            let mut edge_pixels = 0_u32;

            let mut mask_pixels = 0_u32;

            for py in 0..piece_height {

                for px in 0..piece_width {

                    if !cropped_mask[py][px] {

                        continue;
                    }

                    mask_pixels += 1;

                    let background_value = background_edges[y + py][x + px];

                    let piece_value = piece_edges[py][px];

                    if piece_value > 0 {

                        edge_pixels += 1;

                        score += if background_value > 0 { 3.0 } else { -1.5 };
                    } else if background_value == 0 {

                        score += 0.15;
                    }
                }
            }

            if mask_pixels == 0 || edge_pixels == 0 {

                continue;
            }

            score /= f64::from(edge_pixels);

            // Tiny tie-breaker so the leftmost match wins on equal scores.
            score += f64::from(mask_pixels) * 0.0001;

            if score > best_score {

                best_score = score;

                best_x = x;
            }
        }
    }

    Ok(best_x as u32)
}

/// Encrypts a payload with the challenge's key.
///
/// Why:
/// The service expects `pointJson` and `captchaVerification` as base64 of
/// AES-128-ECB ciphertext under `secretKey`. Sending plaintext is rejected.

fn encrypt(plaintext: &str, secret_key: &str) -> Result<String> {

    let key: &[u8; 16] = secret_key.as_bytes().try_into().map_err(|_| {

        anyhow!(
            "验证码密钥长度非法: {}（当前仅支持 16 字节）",
            secret_key.len()
        )
    })?;

    if !matches!(key.len(), 16 | 24 | 32) {

        bail!("验证码密钥长度非法: {}", key.len());
    }

    // The service issues a 16-byte key; longer keys are not supported by the
    // AES-128 block this client uses.
    if key.len() != 16 {

        bail!("验证码密钥不是 16 字节，当前仅支持 AES-128");
    }

    let cipher = Aes128::new(&BlockArray::from(*key));

    let padded = pkcs5_pad(plaintext.as_bytes());

    let mut encrypted = Vec::with_capacity(padded.len());

    for chunk in padded.chunks_exact(16) {

        let bytes: [u8; 16] = chunk.try_into().expect("块大小由 chunks_exact 保证");

        let mut block = BlockArray::from(bytes);

        cipher.encrypt_block(&mut block);

        encrypted.extend_from_slice(&block);
    }

    Ok(BASE64.encode(encrypted))
}

/// PKCS#5/PKCS#7 padding to the AES block size.

fn pkcs5_pad(input: &[u8]) -> Vec<u8> {

    let padding = 16 - (input.len() % 16);

    let mut output = input.to_vec();

    output.extend(std::iter::repeat_n(padding as u8, padding));

    output
}

/// Solves a challenge end to end.

pub fn solve(challenge: &CaptchaChallenge) -> Result<SolvedCaptcha> {

    let background = decode_image(&challenge.original)?;

    let piece = decode_image(&challenge.jigsaw)?;

    let move_distance = solve_offset(&background, &piece)?;

    let point_json_data = format!("{{\"x\":{move_distance},\"y\":5}}");

    let point_json = encrypt(&point_json_data, &challenge.secret_key)?;

    let captcha_verification = encrypt(
        &format!("{}---{}", challenge.token, point_json_data),
        &challenge.secret_key,
    )?;

    Ok(SolvedCaptcha {
        move_distance,
        point_json_data,
        point_json,
        captcha_verification,
        token: challenge.token.clone(),
    })
}

#[cfg(test)]

mod tests {

    use super::{CaptchaChallenge, pkcs5_pad, solve_offset};
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    #[test]

    fn pkcs5_padding_always_adds_at_least_one_byte() {

        // A full block must still gain a whole block of padding, or the server
        // cannot strip it.
        assert_eq!(pkcs5_pad(b"0123456789abcdef").len(), 32);

        assert_eq!(pkcs5_pad(b"short").len(), 16);

        assert_eq!(pkcs5_pad(b"short")[5..], [11_u8; 11]);
    }

    /// Builds a PNG with a solid colour, optionally with a marked block.
    ///
    /// Why:
    /// The solver is image logic; testing it needs real encoded images rather
    /// than hand-built rasters, so this mirrors what the service sends.

    fn png_with_block(
        width: u32,
        height: u32,
        block_at: Option<(u32, u32)>,
        opaque_start: u32,
    ) -> String {

        let mut buffer = image::RgbaImage::new(width, height);

        for y in 0..height {

            for x in 0..width {

                // A deterministic gradient so edges exist to align against.
                let shade = ((x * 7 + y * 3) % 200) as u8;

                let inside = block_at
                    .is_some_and(|(bx, by)| x >= bx && x < bx + 20 && y >= by && y < by + 20);

                let pixel = if inside {

                    [255, 255, 255, 255]
                } else if x < opaque_start {

                    [0, 0, 0, 0]
                } else {

                    [shade, shade, shade, 255]
                };

                buffer.put_pixel(x, y, image::Rgba(pixel));
            }
        }

        let mut bytes = Vec::new();

        image::DynamicImage::ImageRgba8(buffer)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("应能编码 PNG");

        BASE64.encode(&bytes)
    }

    #[test]

    fn solver_locates_the_piece_offset() {

        // Background with the gap at a known column.
        let background = png_with_block(200, 60, Some((137, 20)), 0);

        // Piece: transparent except for a 20x20 mark, matching the gap.
        let piece = png_with_block(25, 25, Some((0, 0)), 5);

        let raster_background = super::decode_image(&background).expect("背景图应可解码");

        let raster_piece = super::decode_image(&piece).expect("拼图应可解码");

        let offset = solve_offset(&raster_background, &raster_piece).expect("应能求解偏移");

        // The mask starts at x=5 in the piece, so the reported offset is the
        // gap position minus that lead-in, within a small tolerance.
        assert!(
            (125..=140).contains(&offset),
            "偏移应在缺口附近（约 132），得到 {offset}"
        );
    }

    #[test]

    fn solver_rejects_a_piece_larger_than_the_background() {

        let background = png_with_block(20, 20, None, 0);

        let piece = png_with_block(40, 40, None, 0);

        let raster_background = super::decode_image(&background).expect("背景图应可解码");

        let raster_piece = super::decode_image(&piece).expect("拼图应可解码");

        assert!(
            solve_offset(&raster_background, &raster_piece).is_err(),
            "拼图比背景大时应报错而不是给出错误偏移"
        );
    }

    #[test]

    fn solve_encrypts_both_payloads() {

        let challenge = CaptchaChallenge {
            secret_key: "0123456789abcdef".to_string(),
            token:      "tok".to_string(),
            original:   png_with_block(200, 60, Some((137, 20)), 0),
            jigsaw:     png_with_block(25, 25, Some((0, 0)), 5),
        };

        let solved = super::solve(&challenge).expect("应能求解");

        assert!(solved.point_json_data.starts_with("{\"x\":"));

        // AES-ECB of a 12-byte payload is one padded block -> 16 bytes -> 24
        // base64 characters ending in padding.
        assert_eq!(solved.point_json.len(), 24, "应为一整块密文的 base64");

        assert!(
            solved.captcha_verification.len() > solved.point_json.len(),
            "captchaVerification 含 token，应更长"
        );
    }
}
