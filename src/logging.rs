//! Minimal JSONL event logger shared by the TUI and CLI.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]

pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

#[derive(Debug)]

struct LoggerConfig {
    level: LogLevel,
    path:  PathBuf,
}

static LOGGER: OnceLock<Mutex<LoggerConfig>> = OnceLock::new();

pub fn init(level: LogLevel, path: Option<PathBuf>) -> Result<PathBuf> {

    let path = path.unwrap_or(default_log_path()?);

    if let Some(parent) = path.parent() {

        fs::create_dir_all(parent)
            .with_context(|| format!("创建日志目录失败: {}", parent.display()))?;
    }

    let config = LoggerConfig {
        level,
        path: path.clone(),
    };

    if LOGGER.set(Mutex::new(config)).is_err()
        && let Some(lock) = LOGGER.get()
        && let Ok(mut current) = lock.lock()
    {

        current.level = level;

        current.path = path.clone();
    }

    Ok(path)
}

pub fn default_log_path() -> Result<PathBuf> {

    if let Some(state_home) = env::var_os("XDG_STATE_HOME") {

        return Ok(PathBuf::from(state_home).join("iclass-buaa/events.jsonl"));
    }

    let home = env::var_os("HOME").ok_or_else(|| anyhow!("HOME 未设置，无法定位日志目录"))?;

    Ok(PathBuf::from(home).join(".local/state/iclass-buaa/events.jsonl"))
}

pub fn path() -> Option<PathBuf> {

    LOGGER
        .get()
        .and_then(|lock| lock.lock().ok().map(|config| config.path.clone()))
}

pub fn parse_level(value: &str) -> Result<LogLevel> {

    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" | "warning" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        _ => Err(anyhow!("log-level 必须是 error、warn、info 或 debug")),
    }
}

pub fn event(level: LogLevel, target: &str, message: impl AsRef<str>, fields: Value) {

    let Some(lock) = LOGGER.get() else {

        return;
    };

    let Ok(config) = lock.lock() else {

        return;
    };

    if level > config.level {

        return;
    }

    let record = json!({
        "ts": Utc::now().to_rfc3339(),
        "level": level_label(level),
        "target": target,
        "message": redact(message.as_ref()),
        "fields": redact_value(fields),
    });

    if let Ok(line) = serde_json::to_string(&record) {

        let _ = append_line(&config.path, &line);
    }
}

fn append_line(path: &Path, line: &str) -> Result<()> {

    use std::io::Write as _;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("打开日志文件失败: {}", path.display()))?;

    writeln!(file, "{line}").context("写入日志失败")
}

fn level_label(level: LogLevel) -> &'static str {

    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    }
}

fn redact_value(value: Value) -> Value {

    match value {
        Value::String(text) => Value::String(redact(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        Value::Object(map) => {
            Value::Object(
                map.into_iter()
                    .map(|(key, value)| {
                        if is_sensitive_key(&key) {

                            (key, Value::String("<redacted>".to_string()))
                        } else {

                            (key, redact_value(value))
                        }
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

fn redact(input: &str) -> String {

    let mut output = input.to_string();

    for marker in ["password", "token", "cookie", "session"] {

        output = redact_marker(&output, marker);
    }

    output
}

fn redact_marker(input: &str, marker: &str) -> String {

    let lower = input.to_ascii_lowercase();

    let Some(index) = lower.find(marker) else {

        return input.to_string();
    };

    let end = input[index..]
        .find([' ', '\t', '\n', '\r', ',', ';'])
        .map(|offset| index + offset)
        .unwrap_or(input.len());

    format!("{}{}=<redacted>{}", &input[..index], marker, &input[end..])
}

fn is_sensitive_key(key: &str) -> bool {

    let key = key.to_ascii_lowercase();

    key.contains("password")
        || key.contains("token")
        || key.contains("cookie")
        || key.contains("session")
}

#[cfg(test)]

mod tests {

    use serde_json::json;

    use super::{LogLevel, parse_level, redact_value};

    #[test]

    fn parse_log_level_accepts_expected_values() {

        assert_eq!(parse_level("error").unwrap(), LogLevel::Error);

        assert_eq!(parse_level("warning").unwrap(), LogLevel::Warn);

        assert!(parse_level("trace").is_err());
    }

    #[test]

    fn redact_value_masks_sensitive_fields() {

        let value = redact_value(json!({
            "password": "secret",
            "nested": { "token": "abc", "message": "cookie=raw failed" },
        }));

        assert_eq!(value["password"], "<redacted>");

        assert_eq!(value["nested"]["token"], "<redacted>");

        assert_eq!(value["nested"]["message"], "cookie=<redacted> failed");
    }
}
