//! Seminar-room (研讨室) reservation for the CGYY service.
//!
//! Why:
//! Rooms are booked through `cgyy.buaa.edu.cn`, which signs every request and
//! gates submission behind a slider captcha. Both are handled here so the rest
//! of the app only deals with rooms and times.

mod captcha;
mod signer;

pub use captcha::{CaptchaChallenge, SolvedCaptcha, solve as solve_captcha};
pub use signer::{APP_KEY, add_nocache, sign, sign_payload};
