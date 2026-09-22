//! Seminar-room (研讨室) reservation for the CGYY service.
//!
//! Why:
//! Rooms are booked through `cgyy.buaa.edu.cn`, which signs every request and
//! gates submission behind a slider captcha. Both are handled here so the rest
//! of the app only deals with rooms and times.

mod api;
mod captcha;
mod signer;

// Public surface. `Order` and `VenueSite` are named even though callers often
// infer them from method returns, so the types have a stable import path.
#[allow(unused_imports)]
pub use api::{DayInfo, Order, ReservationRequest, VenueSite};
