//! Request signing for the seminar-room (研讨室) service.
//!
//! Why:
//! `cgyy.buaa.edu.cn` rejects unsigned requests. Every call must carry an
//! `app-key`, a millisecond `timestamp`, and an MD5 `sign` computed over the
//! path plus the sorted request parameters. Without it the service answers with
//! an auth error that looks like a session problem, which makes the failure
//! hard to diagnose.
//!
//! How:
//! The digest input is the literal concatenation below. The prefix appears
//! twice, once at the start and once after the timestamp, and parameters are
//! sorted by name with no separator between a key and its value.

use md5::{Digest, Md5};

/// Shared secret that starts and ends every digest input.

pub const PREFIX: &str = "c640ca392cd45fb3a55b00a63a86c618";

/// Value sent in the `app-key` header.

pub const APP_KEY: &str = "8fceb735082b5a529312040b58ea780b";

/// Parameters the service excludes from signing and from the request.
///
/// Why:
/// These are server-managed bookkeeping fields. The desktop client strips them
/// before signing, so including them would produce a digest the server cannot
/// reproduce.

const REMOVED_KEYS: [&str; 7] = [
    "gmtCreate",
    "gmtModified",
    "creator",
    "modifier",
    "id",
    "_index",
    "_rowKey",
];

/// Request parameter set, as name/value pairs.

pub type Params<'a> = &'a [(&'a str, &'a str)];

/// Builds the digest input for a request.
///
/// Why:
/// Separated from the digest so the exact string can be asserted in tests. A
/// mismatch here produces a signature the server rejects with no explanation.

pub fn sign_payload(path: &str, params: Params<'_>, timestamp: i64) -> String {

    let normalized = if path.starts_with('/') {

        path.to_string()
    } else {

        format!("/{path}")
    };

    let mut cleaned: Vec<(&str, &str)> = params
        .iter()
        .copied()
        .filter(|(key, _)| !REMOVED_KEYS.contains(key))
        .collect();

    cleaned.sort_by(|left, right| left.0.cmp(right.0));

    let mut payload = String::with_capacity(128);

    payload.push_str(PREFIX);

    payload.push_str(&normalized);

    for (key, value) in cleaned {

        payload.push_str(key);

        payload.push_str(value);
    }

    payload.push_str(&timestamp.to_string());

    payload.push(' ');

    payload.push_str(PREFIX);

    payload
}

/// Computes the `sign` header for a request.

pub fn sign(path: &str, params: Params<'_>, timestamp: i64) -> String {

    let payload = sign_payload(path, params, timestamp);

    let mut hasher = Md5::new();

    hasher.update(payload.as_bytes());

    let digest = hasher.finalize();

    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Adds the `nocache` parameter used by GET requests when it is absent.
///
/// Why:
/// The service caches GET responses keyed on this value; without it a repeat
/// query can return a stale reservation calendar.

pub fn add_nocache(params: Params<'_>, timestamp: i64) -> Vec<(String, String)> {

    let mut owned: Vec<(String, String)> = params
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();

    if !owned.iter().any(|(key, _)| key == "nocache") {

        owned.push(("nocache".to_string(), timestamp.to_string()));
    }

    owned
}

#[cfg(test)]

mod tests {

    use super::{PREFIX, sign_payload};

    #[test]

    fn payload_uses_the_documented_layout() {

        // prefix + path + sorted key/value pairs + timestamp + space + prefix
        let payload = sign_payload("/api/codes", &[("b", "2"), ("a", "1")], 1700000000000);

        assert_eq!(
            payload,
            format!("{PREFIX}/api/codesa1b21700000000000 {PREFIX}")
        );
    }

    #[test]

    fn a_missing_leading_slash_is_added() {

        let with = sign_payload("/api/codes", &[], 1);

        let without = sign_payload("api/codes", &[], 1);

        assert_eq!(with, without);
    }

    #[test]

    fn server_managed_fields_are_excluded() {

        // Including id/creator would make the digest unreproducible upstream.
        let payload = sign_payload(
            "/api/x",
            &[("id", "9"), ("creator", "me"), ("keep", "1")],
            5,
        );

        assert!(!payload.contains("id9"));

        assert!(!payload.contains("creatorme"));

        assert!(payload.contains("keep1"));
    }

    #[test]

    fn parameter_order_does_not_affect_the_digest() {

        // The service sorts by name; insertion order must not matter.
        assert_eq!(
            sign_payload("/p", &[("b", "2"), ("a", "1")], 7),
            sign_payload("/p", &[("a", "1"), ("b", "2")], 7)
        );
    }
}
