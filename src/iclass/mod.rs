//! iClass module split around the HTTP client implementation and future helpers.

mod api;
pub(crate) mod http;

pub use api::IClassApi;

/// Token validity is deliberately local: five minutes at most, cleared on
/// authentication failure or a new login. Never serialized or shared globally.

pub(crate) struct CachedToken {
    value:      String,
    expires_at: std::time::Instant,
}

impl CachedToken {
    pub(crate) fn new(value: String) -> Self {

        Self {
            value,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(300),
        }
    }

    pub(crate) fn valid(&self) -> Option<String> {

        (std::time::Instant::now() < self.expires_at).then(|| self.value.clone())
    }
}

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {

        f.debug_struct("CachedToken")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
