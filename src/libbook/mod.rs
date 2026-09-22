//! Library seat reservation (图书馆座位) for the booking service.
//!
//! Why:
//! Seat booking lives at `booking.lib.buaa.edu.cn`, reachable only after a CAS
//! exchange, and reservations must be sent as encrypted payloads.
//!
//! How:
//! The CAS handshake yields a bearer token; [`crypto`] builds the encrypted
//! reservation body. The API layer ties the two together.

mod api;
mod crypto;

// Public surface. The API types are named so they have a stable import path.
// Public surface. Named so the types have a stable import path even where
// callers infer them from method returns.
#[allow(unused_imports)]
pub use api::{Area, AreaDetail, Booking, Library, Seat, TimeSlot};
#[allow(unused_imports)]
pub use crypto::{EncryptedReserveBody, encrypt_reserve};
