//! Desktop notifications for automation outcomes.
//!
//! Why:
//! The planner runs unattended. When it signs in successfully nobody needs to
//! be told, but when it *fails* — a locked BYKC account, an expired session, a
//! lost race for a sign-in window — the user would otherwise find out only by
//! opening the app and reading the log. That is the worst failure mode this
//! tool has, so a failure is worth interrupting for.
//!
//! How:
//! Best-effort and non-fatal. Notification tooling is often absent (headless
//! servers, containers, minimal installs), and a missing notifier must never
//! turn a successful sign-in into a failed run. Every failure here is swallowed
//! and reported to the caller only as a boolean.

use std::process::{Command, Stdio};

/// How much a notification matters.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub enum Urgency {
    /// Sign-in succeeded; informational.
    Normal,
    /// Something needs attention.
    Critical,
}

impl Urgency {
    fn as_str(self) -> &'static str {

        match self {
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }
}

/// Sends a desktop notification if the platform offers a way to.
///
/// Why:
/// Returns a result rather than nothing so callers can surface the reason when
/// the user explicitly asked for a test notification, while the planner can
/// ignore it entirely.

pub fn notify(summary: &str, body: &str, urgency: Urgency) -> Result<(), String> {

    if cfg!(target_os = "macos") {

        return notify_macos(summary, body);
    }

    if cfg!(target_os = "windows") {

        return notify_windows(summary, body);
    }

    notify_linux(summary, body, urgency)
}

/// macOS: `osascript` driving Notification Center.
///
/// How:
/// Text is passed as an AppleScript string literal, so quotes and backslashes
/// are escaped. Without that a course name containing a quote would break the
/// script and the notification would silently not appear.

fn notify_macos(summary: &str, body: &str) -> Result<(), String> {

    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape_applescript(body),
        escape_applescript(summary),
    );

    run(Command::new("osascript").arg("-e").arg(script))
}

/// Windows: PowerShell toast through the WinRT notification API.

fn notify_windows(summary: &str, body: &str) -> Result<(), String> {

    let script = format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, \
         ContentType = WindowsRuntime] > $null; $t = \
         [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.\
         Notifications.ToastTemplateType]::ToastText02); $n = $t.GetElementsByTagName('text'); \
         $n.Item(0).AppendChild($t.CreateTextNode('{}')) > $null; \
         $n.Item(1).AppendChild($t.CreateTextNode('{}')) > $null; \
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('iClass \
         BUAA').Show([Windows.UI.Notifications.ToastNotification]::new($t));",
        escape_powershell(summary),
        escape_powershell(body),
    );

    run(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script))
}

/// Linux: `notify-send`, falling back to `zenity` on desktops without it.

fn notify_linux(summary: &str, body: &str, urgency: Urgency) -> Result<(), String> {

    let send = run(Command::new("notify-send")
        .arg("-u")
        .arg(urgency.as_str())
        .arg("--app-name=iClass BUAA")
        .arg(summary)
        .arg(body));

    if send.is_ok() {

        return Ok(());
    }

    run(Command::new("zenity")
        .arg("--notification")
        .arg(format!("--text={summary}\n{body}")))
}

fn run(command: &mut Command) -> Result<(), String> {

    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("无法启动通知程序: {error}"))?;

    if output.status.success() {

        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    Err(format!("通知程序返回 {}: {}", output.status, stderr.trim()))
}

fn escape_applescript(value: &str) -> String {

    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_powershell(value: &str) -> String {

    value.replace('\'', "''")
}

#[cfg(test)]

mod tests {

    use super::{Urgency, escape_applescript, escape_powershell};

    #[test]

    fn applescript_escapes_quotes_and_backslashes() {

        // A course name with a quote would otherwise terminate the literal and
        // silently drop the notification.
        assert_eq!(escape_applescript(r#"高等数学 "A""#), r#"高等数学 \"A\""#);

        assert_eq!(escape_applescript(r"路径 C:\temp"), r"路径 C:\\temp");
    }

    #[test]

    fn powershell_escapes_single_quotes() {

        assert_eq!(escape_powershell("it's due"), "it''s due");
    }

    #[test]

    fn urgency_maps_to_notify_send_levels() {

        assert_eq!(Urgency::Normal.as_str(), "normal");

        assert_eq!(Urgency::Critical.as_str(), "critical");
    }
}
