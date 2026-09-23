//! HTTP budgets and secret-free diagnostic helpers shared by owned clients.

use std::time::Duration;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) fn client_builder() -> reqwest::ClientBuilder {

    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
}

/// Diagnostics need only an origin: query, fragment, userinfo and even path
/// segments can carry CAS tickets or other credentials.

pub(crate) fn diagnostic_url(raw: &str) -> String {

    match reqwest::Url::parse(raw) {
        Ok(url) => url.origin().ascii_serialization(),
        Err(_) => "<invalid-url>".to_string(),
    }
}

/// Removes complete URL tokens from an error chain, including nested service
/// URLs. Raw upstream response bodies must never be passed to this helper.

pub(crate) fn diagnostic_text(raw: &str) -> String {

    raw.split_whitespace()
        .map(|word| {
            if let Some(index) = word.find("https://").or_else(|| word.find("http://")) {

                format!("{}{}", &word[..index], diagnostic_url(&word[index..]))
            } else {

                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]

mod tests {

    use super::*;

    #[test]

    fn diagnostics_drop_userinfo_path_query_fragment_and_nested_service() {

        let raw = "https://user:password@example.invalid/token-secret?service=https%3A%2F%2Fx%3Fticket%3DST-secret#cookie-secret";

        assert_eq!(diagnostic_url(raw), "https://example.invalid");

        assert_eq!(
            diagnostic_text(&format!("连接失败 {raw}")),
            "连接失败 https://example.invalid"
        );

        assert_eq!(diagnostic_url("非 URL secret"), "<invalid-url>");
    }

    #[test]

    fn all_budgets_are_finite_and_ordered() {

        assert!(CONNECT_TIMEOUT < READ_TIMEOUT);

        assert!(READ_TIMEOUT < REQUEST_TIMEOUT);

        assert!(REQUEST_TIMEOUT <= Duration::from_secs(60));
    }
}
