//! Classifying failures so callers can react without parsing prose.
//!
//! Why:
//! An agent that calls this tool has to decide what to do when a call fails:
//! retry, re-authenticate, fix an argument, or give up. The error text is for a
//! human and changes freely; a stable code is what makes that decision
//! programmable.
//!
//! How:
//! Codes are derived from the failure's own message, since errors travel as
//! `anyhow` values built at many call sites. The mapping is deliberately
//! keyword-based and kept in one place, with `unknown` as the honest fallback
//! rather than guessing.

use serde::Serialize;

use crate::failure::{self, Operation};

/// A stable failure code plus whether retrying could help.

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]

pub struct ErrorCode {
    pub code:      &'static str,
    pub retryable: bool,
}

/// Classifies a failure from its message.
///
/// How:
/// The order matters: authentication is checked before the generic cases, since
/// an expired session often surfaces as a transport-looking error.

pub fn classify(message: &str) -> ErrorCode {

    let classified = failure::classify_text(message, Operation::Read);

    ErrorCode {
        code:      classified.code(),
        retryable: classified.retryable && !classified.kind.is_auth_expiry(),
    }
}

/// The JSON document emitted when a `--json` call fails.
///
/// Why:
/// A caller that asked for JSON should not have to switch back to parsing prose
/// just because the call failed. The same shape carries the code, the message
/// and the cause chain.

#[derive(Debug, Serialize)]

pub struct ErrorReport {
    pub error:     ErrorPayload,
    pub command:   Option<String>,
    pub retryable: bool,
    pub code:      &'static str,
}

#[derive(Debug, Serialize)]

pub struct ErrorPayload {
    pub message: String,
    /// Underlying causes, outermost first.
    pub causes:  Vec<String>,
}

impl ErrorReport {
    /// Builds a report from an `anyhow` error.

    pub fn from_error(error: &anyhow::Error, command: Option<String>) -> Self {

        let message = error.to_string();

        let classified = classify(&message);

        let causes = error.chain().skip(1).map(ToString::to_string).collect();

        Self {
            error: ErrorPayload { message, causes },
            command,
            retryable: classified.retryable,
            code: classified.code,
        }
    }
}

#[cfg(test)]

mod tests {

    use super::classify;

    #[test]

    fn authentication_failures_are_not_retryable() {

        // Retrying an expired session just wastes the caller's budget.
        let code = classify("研讨室登录状态已失效，请重新登录");

        assert_eq!(code.code, "not_authenticated");

        assert!(!code.retryable);

        assert_eq!(classify("HTTP 401").code, "not_authenticated");
    }

    #[test]

    fn transient_failures_are_retryable() {

        assert!(classify("请求超时").retryable);

        assert!(classify("upstream returned 503").retryable);

        assert!(classify("连接失败: dns error").retryable);

        assert!(classify("触发限流").retryable);
    }

    #[test]

    fn argument_problems_are_the_callers_fault() {

        let code = classify("日期格式必须是 YYYY-MM-DD: 明天");

        assert_eq!(code.code, "invalid_argument");

        assert!(!code.retryable);
    }

    #[test]

    fn taken_resources_are_reported_distinctly() {

        // A full room is not a transient error; retrying books nothing.
        let code = classify("时段 3 已不可预约，请刷新后重试");

        assert_eq!(code.code, "resource_unavailable");

        assert!(!code.retryable);
    }

    #[test]

    fn an_unrecognised_failure_is_not_guessed_at() {

        // Better an honest `unknown` than a wrong code that misleads a retry
        // loop.
        let code = classify("某种没见过的情况");

        assert_eq!(code.code, "unknown");

        assert!(!code.retryable);
    }
}
