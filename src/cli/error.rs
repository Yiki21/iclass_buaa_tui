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

    let lower = message.to_ascii_lowercase();

    let contains = |needle: &str| lower.contains(needle) || message.contains(needle);

    // Authentication: the fix is to log in again, not to retry.
    if contains("登录")
        || contains("unauthor")
        || contains("sso")
        || contains("ticket")
        || contains("access_token")
        || contains("凭据")
    {

        return ErrorCode {
            code:      "not_authenticated",
            retryable: false,
        };
    }

    // Configuration: the user must change something.
    if contains("配置文件") || contains("权限") || contains("config") {

        return ErrorCode {
            code:      "config_invalid",
            retryable: false,
        };
    }

    // Bad arguments: the command line itself is wrong.
    if contains("格式必须")
        || contains("无效")
        || contains("必须大于")
        || contains("至少")
        || contains("需要 --yes")
    {

        return ErrorCode {
            code:      "invalid_argument",
            retryable: false,
        };
    }

    // Rate limiting and transient upstream trouble: retrying is reasonable.
    if contains("429") || contains("限流") || contains("too many") {

        return ErrorCode {
            code:      "rate_limited",
            retryable: true,
        };
    }

    if contains("超时") || contains("timed out") || contains("timeout") {

        return ErrorCode {
            code:      "upstream_timeout",
            retryable: true,
        };
    }

    // The account or resource is in a state the caller must resolve.
    if contains("423") || contains("locked") || contains("锁定") {

        return ErrorCode {
            code:      "account_locked",
            retryable: true,
        };
    }

    if contains("已满") || contains("不可预约") || contains("已被占用") {

        return ErrorCode {
            code:      "resource_unavailable",
            retryable: false,
        };
    }

    if contains("401") || contains("403") {

        return ErrorCode {
            code:      "not_authenticated",
            retryable: false,
        };
    }

    // Transport failures from the HTTP layer.
    if contains("连接") || contains("connect") || contains("dns") || contains("解析") {

        return ErrorCode {
            code:      "network_error",
            retryable: true,
        };
    }

    // Upstream answered, but not with something usable.
    if contains("http 5") || contains("502") || contains("503") || contains("bad gateway") {

        return ErrorCode {
            code:      "upstream_error",
            retryable: true,
        };
    }

    ErrorCode {
        code:      "unknown",
        retryable: false,
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
