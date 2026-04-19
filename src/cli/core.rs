//! Shared CLI runtime types used across command handlers and schedulers.

use chrono::{DateTime, Local};

/// A normalized sign target used by the planner and retry logic.
#[derive(Debug, Clone)]
pub(crate) struct ListedTarget {
    pub(crate) source: SignSource,
    pub(crate) action: SignAction,
    pub(crate) name: String,
    pub(crate) course_id: String,
    pub(crate) target_id: String,
    pub(crate) date: String,
    pub(crate) start_time: String,
    pub(crate) end_time: String,
    pub(crate) signed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignSource {
    IClass,
    Bykc,
}

impl SignSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::IClass => "iclass",
            Self::Bykc => "bykc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignAction {
    SignIn,
    SignOut,
}

impl SignAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SignIn => "sign-in",
            Self::SignOut => "sign-out",
        }
    }
}

/// Retry behavior shared by course fetch and sign operations.
#[derive(Debug, Clone)]
pub(crate) struct RetryPolicy {
    pub(crate) max_attempts: u32,
    pub(crate) interval_seconds: u64,
}

/// Planner state for a course in the current automation cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollStatusKind {
    /// The planner has not reached the configured daily start time yet.
    WaitingForDailyStart,
    /// The daily planner is active, but this course is not in its sign window yet.
    WaitingForCourse,
    /// The course should be signed immediately.
    DueNow,
    /// The course is already signed.
    Signed,
    /// The course has already ended.
    Expired,
    /// The target is missing the identifier required by its sign API.
    MissingCourseSchedId,
}

/// A sign target plus its computed planner state and first eligible sign time.
#[derive(Debug, Clone)]
pub(crate) struct EvaluatedCourse {
    pub(crate) course: ListedTarget,
    pub(crate) status: PollStatusKind,
    pub(crate) available_at: Option<DateTime<Local>>,
}
