//! Request encryption for library seat reservation.
//!
//! Why:
//! `booking.lib.buaa.edu.cn` will not accept a reservation in clear text. The
//! seat, segment, and date are serialized to JSON, encrypted, and sent as a
//! base64 `aesjson` field. Sending plaintext is rejected without explanation.
//!
//! How:
//! AES-128-CBC with PKCS#7 padding, a fixed IV, and a key derived from the
//! reservation date: the eight digits of the date followed by the same digits
//! reversed. The key is never transmitted; the server derives it the same way.

use aes::{
    Aes128,
    cipher::{BlockCipherEncrypt, KeyInit},
};

// Aliased because `aes::cipher::Array` is the cipher's block type, and the
// name would otherwise read as a plain array.
use aes::cipher::Array as BlockArray;
use anyhow::{Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

/// Fixed IV used by the upstream client.

const IV: &[u8; 16] = b"ZZWBKJ_ZHIHUAWEI";

/// Fields the reservation payload carries.
///
/// Why:
/// A struct rather than a map so the JSON key names (`seat_id`, `start_time`,
/// `end_time`) are written once and cannot drift between the serializer and the
/// server's expectations.

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]

pub struct EncryptedReserveBody {
    pub seat_id:    String,
    pub segment:    String,
    pub day:        String,
    pub start_time: String,
    pub end_time:   String,
}

/// Derives the AES key from a reservation date.
///
/// Why:
/// The key is the eight digits of the date followed by those digits reversed,
/// giving 16 bytes. It is tied to the date, so reusing a payload on another day
/// fails at the server rather than silently booking the wrong slot.

fn aes_key(day: &str) -> Result<[u8; 16]> {

    let digits: String = day.chars().filter(char::is_ascii_digit).collect();

    if digits.len() != 8 {

        bail!("预约日期必须包含 8 位数字（形如 2026-03-10），得到: {day}");
    }

    let reversed: String = digits.chars().rev().collect();

    let key: String = format!("{digits}{reversed}");

    key.as_bytes()
        .try_into()
        .map_err(|_| anyhow!("图书预约密钥长度错误"))
}

/// PKCS#7 padding to the AES block size.

fn pkcs7_pad(input: &[u8]) -> Vec<u8> {

    let padding = 16 - (input.len() % 16);

    let mut output = input.to_vec();

    output.extend(std::iter::repeat_n(padding as u8, padding));

    output
}

/// Encrypts a reservation payload into the `aesjson` string.

pub fn encrypt_reserve(body: &EncryptedReserveBody) -> Result<String> {

    // `encodeDefaults` upstream means empty strings are still emitted, so the
    // field set must match exactly.
    let plaintext =
        serde_json::to_string(body).map_err(|error| anyhow!("序列化预约内容失败: {error}"))?;

    encrypt_json(&plaintext, &body.day)
}

/// Encrypts arbitrary JSON with the date-derived key.

pub fn encrypt_json(plaintext: &str, day: &str) -> Result<String> {

    let key = aes_key(day)?;

    let cipher = Aes128::new(&BlockArray::from(key));

    let padded = pkcs7_pad(plaintext.as_bytes());

    let mut encrypted = Vec::with_capacity(padded.len());

    let mut previous: [u8; 16] = *IV;

    for chunk in padded.chunks_exact(16) {

        let bytes: [u8; 16] = chunk.try_into().expect("块大小由 chunks_exact 保证");

        let mut block = [0_u8; 16];

        for index in 0..16 {

            block[index] = bytes[index] ^ previous[index];
        }

        let mut array = BlockArray::from(block);

        cipher.encrypt_block(&mut array);

        let out: [u8; 16] = array.into();

        encrypted.extend_from_slice(&out);

        previous = out;
    }

    Ok(BASE64.encode(encrypted))
}

#[cfg(test)]

mod tests {

    use super::{EncryptedReserveBody, aes_key, encrypt_json, encrypt_reserve, pkcs7_pad};

    #[test]

    fn key_is_the_date_digits_plus_their_reverse() {

        assert_eq!(
            &aes_key("2026-03-10").expect("应有密钥"),
            b"2026031001306202"
        );
    }

    #[test]

    fn a_date_without_eight_digits_is_rejected() {

        // A malformed date must fail loudly rather than derive a wrong key and
        // book against the wrong slot.
        assert!(aes_key("2026-03").is_err());

        assert!(aes_key("明天").is_err());
    }

    #[test]

    fn padding_always_extends_to_a_full_block() {

        assert_eq!(pkcs7_pad(b"").len(), 16);

        assert_eq!(pkcs7_pad(b"0123456789abcdef").len(), 32);

        assert_eq!(pkcs7_pad(b"x")[1..], [15_u8; 15]);
    }

    #[test]

    fn ciphertext_is_a_whole_number_of_blocks() {

        // PKCS#7 always adds padding, so the output is longer than the input
        // and a multiple of the block size.
        let encrypted = encrypt_json("{\"a\":1}", "2026-03-10").expect("应能加密");

        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encrypted)
            .expect("应能解码");

        assert_eq!(raw.len() % 16, 0);

        assert_eq!(raw.len(), 16);
    }

    #[test]

    fn encryption_is_deterministic_for_a_fixed_iv_and_key() {

        // The server decrypts with the same derived key and IV, so the same
        // input on the same day must produce the same ciphertext.
        let first = encrypt_json("{\"a\":1}", "2026-03-10").expect("应能加密");

        let second = encrypt_json("{\"a\":1}", "2026-03-10").expect("应能加密");

        assert_eq!(first, second);
    }

    #[test]

    fn changing_the_day_changes_the_ciphertext() {

        let first = encrypt_json("{\"a\":1}", "2026-03-10").expect("应能加密");

        let second = encrypt_json("{\"a\":1}", "2026-03-11").expect("应能加密");

        assert_ne!(first, second, "不同日期派生不同密钥");
    }

    #[test]

    fn reserve_body_serializes_with_the_servers_key_names() {

        let body = EncryptedReserveBody {
            seat_id:    "S1".to_string(),
            segment:    "1".to_string(),
            day:        "2026-03-10".to_string(),
            start_time: String::new(),
            end_time:   String::new(),
        };

        let json = serde_json::to_string(&body).expect("应能序列化");

        assert!(json.contains("\"seat_id\""), "服务器要求 seat_id 形式");

        assert!(json.contains("\"start_time\""));

        assert!(json.contains("\"end_time\""));

        assert!(encrypt_reserve(&body).is_ok());
    }
}
