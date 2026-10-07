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
///
/// Why the operation matters:
/// A read that times out is safe to repeat. A write that times out may already
/// have happened, so repeating it can duplicate the effect. The same text is
/// therefore classified differently depending on whether the caller was
/// writing, and only the caller knows which.

pub fn classify_for(message: &str, operation: Operation) -> ErrorCode {

    let classified = failure::classify_text(message, operation);

    ErrorCode {
        code:      classified.code(),
        retryable: classified.retryable && !classified.kind.is_auth_expiry(),
    }
}

/// Classifies a read failure.
///
/// Why it stays:
/// Most callers really are reading, and this is the shorter name for that case.
/// It is used by the tests that pin the read-side mapping.

#[cfg(test)]

pub fn classify(message: &str) -> ErrorCode {

    classify_for(message, Operation::Read)
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
    ///
    /// Why the whole error, not its text:
    /// The outer message is often a wrapper ("登录失败"), and the
    /// actionable detail, a rate limit or a taken seat, lives in the cause chain
    /// or in a typed `Failure` further down. `failure::classify` walks both;
    /// classifying only the top message lost the diagnosis and fell back to
    /// `unknown`. The operation is passed through because a write whose outcome
    /// is unknown must not advertise itself as retryable.

    pub fn from_error(
        error: &anyhow::Error,
        command: Option<String>,
        operation: Operation,
    ) -> Self {

        let message = error.to_string();

        let classified = failure::classify(error, operation);

        let causes = error.chain().skip(1).map(ToString::to_string).collect();

        Self {
            error: ErrorPayload { message, causes },
            command,
            retryable: classified.retryable && !classified.kind.is_auth_expiry(),
            code: classified.code(),
        }
    }
}

#[cfg(test)]

mod tests {

    use super::classify;

    #[test]

    fn a_write_that_timed_out_is_never_reported_as_retryable() {

        use super::{ErrorReport, classify_for};
        use crate::failure::Operation;

        // The same transient text: safe to repeat as a read, unsafe as a write,
        // because the write may already have taken effect.
        for text in ["请求超时", "upstream returned 503", "连接失败: dns error"] {

            assert!(
                classify_for(text, Operation::Read).retryable,
                "{text} 读应可重试"
            );

            assert!(
                !classify_for(text, Operation::Write).retryable,
                "{text}: 写入结果未知时不能标成可重试"
            );
        }

        let error = anyhow::anyhow!("请求超时");

        let report =
            ErrorReport::from_error(&error, Some("seat-book".to_string()), Operation::Write);

        assert!(!report.retryable);
    }

    #[test]

    fn the_code_comes_from_the_cause_chain_not_just_the_outer_message() {

        use super::ErrorReport;
        use crate::failure::Operation;

        // Seen live: the outer context says only "登录失败", while the reason
        // worth acting on is one cause deeper. Classifying the outer string
        // alone reported `unknown`.
        let error = anyhow::anyhow!("登录失败：尝试登录太过频繁，请稍后再试")
            .context("登录失败：获取问卷数据");

        let report =
            ErrorReport::from_error(&error, Some("eval-submit".to_string()), Operation::Read);

        assert_eq!(report.code, "rate_limited", "应从未层原因识别出限流");

        assert!(report.retryable, "限流可以重试");
    }

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

    #[test]

    fn live_upstream_messages_map_to_the_documented_codes() {

        // The exact strings two campus services returned, verbatim. The
        // documented code table is only useful if these produce the code it
        // promises rather than falling through to `unknown`.
        let limited = classify("登录失败：尝试登录太过频繁，请稍后再试");

        assert_eq!(limited.code, "rate_limited");

        // `classify` strips auth-expiry from retryable; rate limiting stays.
        assert!(limited.retryable);

        let duplicate =
            classify("图书馆拒绝了请求：非常抱歉，由于您在该时段已存在座位预约，不可重复预约");

        assert_eq!(duplicate.code, "already_booked");

        assert!(!duplicate.retryable);
    }
}
