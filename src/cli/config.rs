//! CLI configuration loading, validation, and scheduler time parsing.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::NaiveTime;
use serde::Deserialize;

use crate::model::LoginInput;

use super::core::SignSource;

const APP_CONFIG_RELATIVE_PATH: &str = "iclass-buaa/config.toml";

/// Automation settings loaded from the CLI config file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AutomationConfig {
    pub(crate) student_id: String,
    #[serde(default)]
    pub(crate) use_vpn: bool,
    #[serde(default)]
    pub(crate) vpn_username: String,
    #[serde(default)]
    pub(crate) vpn_password: String,
    #[serde(default = "default_enable_iclass")]
    pub(crate) enable_iclass: bool,
    #[serde(default)]
    pub(crate) enable_bykc: bool,
    #[serde(default = "default_advance_minutes")]
    pub(crate) advance_minutes: i64,
    #[serde(default = "default_retry_count")]
    pub(crate) retry_count: u32,
    #[serde(default = "default_retry_interval_seconds")]
    pub(crate) retry_interval_seconds: u64,
    #[serde(default = "default_include_courses")]
    pub(crate) include_courses: Vec<String>,
    #[serde(default)]
    pub(crate) exclude_courses: Vec<String>,
    #[serde(default)]
    pub(crate) iclass_include_courses: Vec<String>,
    #[serde(default)]
    pub(crate) iclass_exclude_courses: Vec<String>,
    #[serde(default)]
    pub(crate) bykc_include_courses: Vec<String>,
    #[serde(default)]
    pub(crate) bykc_exclude_courses: Vec<String>,
    #[serde(default = "default_planner_time")]
    pub(crate) planner_time: String,
    #[serde(default = "default_planner_interval_minutes")]
    pub(crate) planner_interval_minutes: u32,
}

pub(crate) fn load_config(path: Option<&Path>) -> Result<AutomationConfig> {
    let config_path = resolve_config_path(path)?;
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("读取配置失败: {}", config_path.display()))?;
    let config: AutomationConfig = toml::from_str(&raw)
        .with_context(|| format!("解析 TOML 失败: {}", config_path.display()))?;
    ensure_config_permissions(&config_path, &config)?;
    config.validate()?;
    Ok(config)
}

pub(crate) fn resolve_config_path(path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path {
        return path
            .canonicalize()
            .with_context(|| format!("找不到配置文件: {}", path.display()));
    }

    let candidates = candidate_config_paths()?;
    if let Some(found) = candidates.iter().find(|path| path.is_file()) {
        return found
            .canonicalize()
            .with_context(|| format!("无法解析配置路径: {}", found.display()));
    }

    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("找不到配置文件，已搜索: {searched}")
}

pub(crate) fn parse_planner_time(value: &str) -> Result<NaiveTime> {
    let formats = ["%H:%M", "%H:%M:%S"];
    for format in formats {
        if let Ok(parsed) = NaiveTime::parse_from_str(value, format) {
            return Ok(parsed);
        }
    }
    bail!("planner_time 格式必须是 HH:MM 或 HH:MM:SS")
}

pub(crate) fn validate_planner_interval_minutes(value: u32) -> Result<()> {
    if value == 0 {
        bail!("planner_interval_minutes 必须大于 0");
    }
    Ok(())
}

fn candidate_config_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if let Some(path) = user_xdg_config_path() {
        push_unique(&mut paths, path);
    }
    if let Some(path) = home_config_path()? {
        push_unique(&mut paths, path);
    }

    for path in system_xdg_config_paths() {
        push_unique(&mut paths, path);
    }

    push_unique(
        &mut paths,
        PathBuf::from("/etc").join(APP_CONFIG_RELATIVE_PATH),
    );

    Ok(paths)
}

fn user_xdg_config_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME").map(|base| PathBuf::from(base).join(APP_CONFIG_RELATIVE_PATH))
}

fn home_config_path() -> Result<Option<PathBuf>> {
    let Some(home) = env::var_os("HOME") else {
        return Ok(None);
    };
    Ok(Some(
        PathBuf::from(home)
            .join(".config")
            .join(APP_CONFIG_RELATIVE_PATH),
    ))
}

fn system_xdg_config_paths() -> Vec<PathBuf> {
    let raw = env::var_os("XDG_CONFIG_DIRS")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/etc/xdg".to_string());

    raw.split(':')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| PathBuf::from(segment).join(APP_CONFIG_RELATIVE_PATH))
        .collect()
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn ensure_config_permissions(path: &Path, config: &AutomationConfig) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if !config.use_vpn || config.vpn_password.is_empty() {
            return Ok(());
        }

        let metadata =
            fs::metadata(path).with_context(|| format!("读取配置权限失败: {}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            bail!(
                "配置文件包含 vpn_password 时权限必须是 600，当前为 {:o}: {}",
                mode,
                path.display()
            );
        }
    }
    Ok(())
}

impl AutomationConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.student_id.trim().is_empty() {
            bail!("student_id 不能为空");
        }
        if self.use_vpn && (self.vpn_username.trim().is_empty() || self.vpn_password.is_empty()) {
            bail!("use_vpn = true 时必须提供 vpn_username 和 vpn_password");
        }
        if self.enable_bykc && !self.use_vpn {
            bail!("enable_bykc = true 时必须同时启用 VPN");
        }
        if !self.enable_iclass && !self.enable_bykc {
            bail!("enable_iclass 和 enable_bykc 不能同时关闭");
        }
        if self.retry_count == 0 {
            bail!("retry_count 必须大于 0");
        }
        if self.planner_time.trim().is_empty() {
            bail!("planner_time 不能为空");
        }

        parse_planner_time(&self.planner_time)?;
        validate_planner_interval_minutes(self.planner_interval_minutes)?;
        Ok(())
    }

    pub(crate) fn login_input(&self) -> LoginInput {
        LoginInput {
            student_id: self.student_id.clone(),
            use_vpn: self.use_vpn,
            vpn_username: self.vpn_username.clone(),
            vpn_password: self.vpn_password.clone(),
        }
    }

    pub(crate) fn source_include_patterns(&self, source: SignSource) -> &[String] {
        match source {
            SignSource::IClass if !self.iclass_include_courses.is_empty() => {
                &self.iclass_include_courses
            }
            SignSource::Bykc if !self.bykc_include_courses.is_empty() => &self.bykc_include_courses,
            _ => &self.include_courses,
        }
    }

    pub(crate) fn source_exclude_patterns(&self, source: SignSource) -> &[String] {
        match source {
            SignSource::IClass if !self.iclass_exclude_courses.is_empty() => {
                &self.iclass_exclude_courses
            }
            SignSource::Bykc if !self.bykc_exclude_courses.is_empty() => &self.bykc_exclude_courses,
            _ => &self.exclude_courses,
        }
    }
}

fn default_advance_minutes() -> i64 {
    5
}

fn default_enable_iclass() -> bool {
    true
}

fn default_retry_count() -> u32 {
    6
}

fn default_retry_interval_seconds() -> u64 {
    30
}

fn default_include_courses() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_planner_time() -> String {
    "07:00:00".to_string()
}

fn default_planner_interval_minutes() -> u32 {
    10
}
