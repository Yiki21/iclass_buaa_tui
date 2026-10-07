//! Shared, typed failure classification for safe retry decisions.
//!
//! The classifier deliberately examines both the complete `anyhow` cause chain
//! and structured `reqwest` errors.  Human-facing messages remain available to
//! callers, but retry policy is derived from this stable type instead of from
//! whichever context happened to be added last.

use std::fmt;

use anyhow::Error;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub enum Operation {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]

pub enum FailureKind {
    AuthExpired,
    Credentials,
    AccountLocked,
    RateLimited,
    ConnectTimeout,
    ReadTimeout,
    UpstreamTimeout,
    Network,
    Http5xx,
    ResourceUnavailable,
    /// The caller already holds what the write would create. The command line is
    /// fine and the resource is not gone: the state is the problem, so the fix
    /// is to cancel the existing one or choose another target.
    AlreadyBooked,
    ConfigInvalid,
    InvalidArgument,
    Unknown,
}

impl FailureKind {
    pub const fn code(self) -> &'static str {

        match self {
            Self::AuthExpired => "not_authenticated",
            Self::Credentials => "not_authenticated",
            Self::AccountLocked => "account_locked",
            Self::RateLimited => "rate_limited",
            Self::ConnectTimeout => "connect_timeout",
            Self::ReadTimeout => "read_timeout",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::Network => "network_error",
            Self::Http5xx => "upstream_error",
            Self::ResourceUnavailable => "resource_unavailable",
            Self::AlreadyBooked => "already_booked",
            Self::ConfigInvalid => "config_invalid",
            Self::InvalidArgument => "invalid_argument",
            Self::Unknown => "unknown",
        }
    }

    pub const fn is_auth_expiry(self) -> bool {

        matches!(self, Self::AuthExpired)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub struct Classification {
    pub kind:                  FailureKind,
    /// Whether an automatic retry is safe for this operation.
    pub retryable:             bool,
    /// A write was sent, but its final outcome cannot be known safely.
    pub write_outcome_unknown: bool,
}

impl Classification {
    pub const fn code(self) -> &'static str {

        self.kind.code()
    }
}

/// A typed marker used when a BYKC operation has an explicit failure phase.
/// This lets callers preserve the classification through `anyhow::Context`.
#[derive(Debug)]

pub struct Failure {
    pub kind:    FailureKind,
    pub message: String,
}

impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {

        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {

        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

pub fn classify(error: &Error, operation: Operation) -> Classification {

    if let Some(kind) = typed_kind(error) {

        return for_operation(kind, operation);
    }

    let text = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ");

    classify_text(&text, operation)
}

pub fn classify_text(text: &str, operation: Operation) -> Classification {

    let lower = text.to_ascii_lowercase();

    let contains = |needle: &str| lower.contains(needle) || text.contains(needle);

    // Transport phases must win over generic words such as "login" in an
    // outer context.  A timeout while reading a response is not auth expiry.
    let kind = if contains("读取") && (contains("超时") || contains("timeout")) {

        FailureKind::ReadTimeout
    } else if contains("连接") && (contains("超时") || contains("timeout")) {

        FailureKind::ConnectTimeout
    } else if contains("timed out") || contains("timeout") || contains("超时") {

        FailureKind::UpstreamTimeout
    } else if contains("423") || contains("locked") || contains("锁定") {

        FailureKind::AccountLocked
    } else if contains("429")
        || contains("限流")
        || contains("too many")
        // SSO's own wording, e.g. `尝试登录太过频繁，请稍后再试`.
        || contains("频繁")
    {

        FailureKind::RateLimited
    } else if contains("5xx")
        || contains("http 500")
        || contains("http 502")
        || contains("503")
        || contains("http 504")
        || contains("bad gateway")
    {

        FailureKind::Http5xx
    } else if contains("已失效")
        || contains("会话已失效")
        || contains("未登录")
        || contains("401")
        || contains("unauthor")
        || contains("access_token")
        || contains("ticket expired")
    {

        FailureKind::AuthExpired
    } else if contains("账号或密码")
        || contains("密码错误")
        || contains("bad credentials")
        || contains("invalid credentials")
    {

        FailureKind::Credentials
    } else if contains("重复预约") || contains("不能重复") {

        // The library's own wording for a seat the caller already holds in
        // that segment (`…已存在座位预约，不可重复预约`). Nothing to pick around:
        // every seat in the segment is refused until the existing booking goes.
        FailureKind::AlreadyBooked
    } else if contains("已满")
        || contains("不可预约")
        || contains("已被占用")
        || contains("已被预约")
    {

        // A seat, room or course someone else took, or a window that closed.
        FailureKind::ResourceUnavailable
    } else if contains("配置文件") || contains("权限") || contains("config") {

        FailureKind::ConfigInvalid
    } else if contains("格式必须")
        || contains("无效")
        || contains("必须大于")
        || contains("至少")
        || contains("需要 --yes")
        || contains("不能为空")
    {

        FailureKind::InvalidArgument
    } else if contains("连接")
        || contains("connect")
        || contains("dns")
        || contains("网络")
        || contains("request failed")
    {

        FailureKind::Network
    } else {

        FailureKind::Unknown
    };

    for_operation(kind, operation)
}

fn typed_kind(error: &Error) -> Option<FailureKind> {

    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<Failure>().map(|failure| failure.kind))
        .or_else(|| {

            error.chain().find_map(|cause| {

                let reqwest_error = cause.downcast_ref::<reqwest::Error>()?;

                if reqwest_error.is_timeout() {

                    Some(FailureKind::UpstreamTimeout)
                } else if reqwest_error.is_connect() {

                    Some(FailureKind::Network)
                } else {

                    reqwest_error
                        .status()
                        .filter(|status| status.is_server_error())
                        .map(|_| FailureKind::Http5xx)
                }
            })
        })
}

fn for_operation(kind: FailureKind, operation: Operation) -> Classification {

    let read_retry = matches!(
        kind,
        FailureKind::AuthExpired
            | FailureKind::RateLimited
            | FailureKind::ConnectTimeout
            | FailureKind::ReadTimeout
            | FailureKind::UpstreamTimeout
            | FailureKind::Network
            | FailureKind::Http5xx
    );

    let write_outcome_unknown = matches!(
        kind,
        FailureKind::ConnectTimeout
            | FailureKind::ReadTimeout
            | FailureKind::UpstreamTimeout
            | FailureKind::Network
            | FailureKind::Http5xx
            | FailureKind::Unknown
    ) && operation == Operation::Write;

    Classification {
        kind,
        // Auth expiry is the only write retry.  The request was rejected before
        // business execution and the refreshed session is safe to use once.
        retryable: match operation {
            Operation::Read => read_retry,
            Operation::Write => kind.is_auth_expiry(),
        },
        write_outcome_unknown,
    }
}

#[cfg(test)]

mod tests {

    use anyhow::anyhow;

    use super::{FailureKind, Operation, classify, classify_text};

    #[test]

    fn cause_chain_transport_error_beats_login_context() {

        let error = anyhow!("登录请求失败").context("读取响应超时");

        let classification = classify(&error, Operation::Read);

        assert_eq!(classification.kind, FailureKind::ReadTimeout);

        assert!(classification.retryable);
    }

    #[test]

    fn writes_never_replay_uncertain_transport_outcomes() {

        for message in ["读取响应超时", "连接超时", "HTTP 503", "connection reset"] {

            let classification = classify_text(message, Operation::Write);

            assert!(!classification.retryable, "{message}");

            assert!(classification.write_outcome_unknown, "{message}");
        }
    }

    #[test]

    fn only_explicit_auth_expiry_can_retry_a_write() {

        let classification = classify_text("BYKC 会话已失效", Operation::Write);

        assert_eq!(classification.kind, FailureKind::AuthExpired);

        assert!(classification.retryable);

        assert!(!classification.write_outcome_unknown);

        let locked = classify_text("HTTP 423 Locked", Operation::Write);

        assert!(!locked.retryable);
    }

    /// Messages observed live from the campus services, verbatim.
    ///
    /// Why:
    /// The documented code table is only true if these exact strings map to the
    /// codes it names; a paraphrase in a test would not catch a keyword miss.

    const LIVE_RATE_LIMIT: &str = "登录失败：尝试登录太过频繁，请稍后再试";

    const LIVE_ALREADY_BOOKED: &str =
        "图书馆拒绝了请求：非常抱歉，由于您在该时段已存在座位预约，不可重复预约";

    #[test]

    fn live_sso_rate_limit_is_rate_limited() {

        let read = classify_text(LIVE_RATE_LIMIT, Operation::Read);

        assert_eq!(read.kind, FailureKind::RateLimited);

        assert_eq!(read.code(), "rate_limited");

        // A rate-limited write is still never replayed automatically.
        assert!(!classify_text(LIVE_RATE_LIMIT, Operation::Write).retryable);
    }

    #[test]

    fn live_duplicate_booking_is_already_booked_and_never_retried() {

        for operation in [Operation::Read, Operation::Write] {

            let classification = classify_text(LIVE_ALREADY_BOOKED, operation);

            assert_eq!(classification.code(), "already_booked");

            assert!(!classification.retryable);

            // The service answered; the write did not vanish in transit.
            assert!(!classification.write_outcome_unknown);
        }
    }

    #[test]

    fn a_seat_taken_by_someone_else_stays_resource_unavailable() {

        // The 0.9.2 rejection wording: someone else holds it, so another seat
        // is the right answer, unlike `already_booked`.
        let classification = classify_text("图书馆拒绝了请求：该座位已被预约", Operation::Write);

        assert_eq!(classification.code(), "resource_unavailable");
    }

    /// Every kind, written so a new variant fails to compile here until it is
    /// added, and with it to the documented tables below.

    fn every_kind() -> Vec<FailureKind> {

        let all = [
            FailureKind::AuthExpired,
            FailureKind::Credentials,
            FailureKind::AccountLocked,
            FailureKind::RateLimited,
            FailureKind::ConnectTimeout,
            FailureKind::ReadTimeout,
            FailureKind::UpstreamTimeout,
            FailureKind::Network,
            FailureKind::Http5xx,
            FailureKind::ResourceUnavailable,
            FailureKind::AlreadyBooked,
            FailureKind::ConfigInvalid,
            FailureKind::InvalidArgument,
            FailureKind::Unknown,
        ];

        for kind in all {

            match kind {
                FailureKind::AuthExpired
                | FailureKind::Credentials
                | FailureKind::AccountLocked
                | FailureKind::RateLimited
                | FailureKind::ConnectTimeout
                | FailureKind::ReadTimeout
                | FailureKind::UpstreamTimeout
                | FailureKind::Network
                | FailureKind::Http5xx
                | FailureKind::ResourceUnavailable
                | FailureKind::AlreadyBooked
                | FailureKind::ConfigInvalid
                | FailureKind::InvalidArgument
                | FailureKind::Unknown => {}
            }
        }

        all.to_vec()
    }

    #[test]

    fn every_emitted_code_is_documented_for_callers() {

        // The code table is what a caller acts on. A code the classifier can
        // emit but no table names falls through to the caller's default.
        let documents = [
            (
                "AGENTS.md",
                include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/AGENTS.md")),
            ),
            (
                "skills/buaa-campus/SKILL.md",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/skills/buaa-campus/SKILL.md"
                )),
            ),
        ];

        for kind in every_kind() {

            for (name, text) in documents {

                assert!(
                    text.contains(&format!("`{}`", kind.code())),
                    "{name} 的错误码表缺少 {}",
                    kind.code()
                );
            }
        }
    }
}
