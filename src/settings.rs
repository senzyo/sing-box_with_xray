//! 配置文件加载。
//!
//! 从 `configs/settings.toml` 读取用户配置（GitHub 代理、日志级别、下载重试参数）。
//! 文件缺失或解析失败时静默回退到默认值，不中断程序运行。

use serde::Deserialize;
use std::path::Path;
use std::sync::Mutex;

/// 应用配置，对应 `configs/settings.toml`。
///
/// 所有字段均提供默认值，配置文件缺失或解析失败时自动回退。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default = "default_gh_proxy")]
    pub gh_proxy: GhProxy,
    #[serde(default)]
    pub log: Log,
    #[serde(default)]
    pub download: Download,
}

/// 加载期间收集的警告信息，供 init_logging 之后通过 tracing 输出。
static LOAD_WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// GitHub CDN 代理配置。
#[derive(Debug, Clone, Deserialize)]
pub struct GhProxy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 代理 URL 前缀，会拼接在 GitHub 原始下载链接前面。
    #[serde(default = "default_proxy_url")]
    pub url: String,
}

/// 日志配置。
#[derive(Debug, Clone, Deserialize)]
pub struct Log {
    /// 日志级别，可选值: "debug", "info", "warn", "error"。
    #[serde(default = "default_log_level")]
    pub level: String,
}

/// 下载重试配置。
#[derive(Debug, Clone, Deserialize)]
pub struct Download {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_retry_delay")]
    pub retry_delay_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_proxy_url() -> String {
    // 默认 GitHub CDN 代理，用于网络受限地区加速下载
    "https://gh-proxy.com/".to_string()
}

fn default_log_level() -> String {
    "debug".to_string()
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_delay() -> u64 {
    2
}

fn default_gh_proxy() -> GhProxy {
    GhProxy {
        enabled: true,
        url: default_proxy_url(),
    }
}

impl Default for GhProxy {
    fn default() -> Self {
        default_gh_proxy()
    }
}

impl Default for Log {
    fn default() -> Self {
        Log {
            level: default_log_level(),
        }
    }
}

impl Default for Download {
    fn default() -> Self {
        Download {
            max_retries: default_max_retries(),
            retry_delay_secs: default_retry_delay(),
        }
    }
}

const ALLOWED_LEVELS: &[&str] = &["debug", "info", "warn", "error"];

impl Settings {
    /// 从 `exe_dir/configs/settings.toml` 加载配置。
    /// 文件不存在或格式错误时打印警告并返回默认值。
    pub fn load(exe_dir: &Path) -> Self {
        let path = exe_dir.join("configs").join("settings.toml");

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                push_warning(format!("读取配置文件失败 ({}), 使用默认配置: {e}", path.display()));
                return Self::default();
            }
        };

        match toml::from_str::<Settings>(&text) {
            Ok(mut s) => {
                s.validate();
                s
            }
            Err(e) => {
                push_warning(format!("解析配置文件失败 ({}), 使用默认配置: {e}", path.display()));
                Self::default()
            }
        }
    }

    /// 取出加载期间收集的警告。调用后清空，仅首次调用返回内容。
    pub fn take_warnings() -> Vec<String> {
        LOAD_WARNINGS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    /// 校验配置值合法性，非法值回退到默认值。
    fn validate(&mut self) {
        let level = self.log.level.to_lowercase();
        if ALLOWED_LEVELS.contains(&level.as_str()) {
            self.log.level = level;
        } else {
            push_warning(format!(
                "无效的日志级别 \"{}\", 可选值: {:?}, 回退到 \"debug\"",
                self.log.level, ALLOWED_LEVELS
            ));
            self.log.level = "debug".to_string();
        }

        if self.download.max_retries == 0 {
            push_warning("max_retries 不能为 0, 回退到默认值 3".to_string());
            self.download.max_retries = 3;
        }

        if self.download.retry_delay_secs == 0 {
            push_warning("retry_delay_secs 不能为 0, 回退到默认值 2".to_string());
            self.download.retry_delay_secs = 2;
        }

        if self.gh_proxy.enabled && self.gh_proxy.url.is_empty() {
            push_warning("gh_proxy 已启用但 url 为空, 自动关闭代理".to_string());
            self.gh_proxy.enabled = false;
        }
    }
}

fn push_warning(msg: String) {
    eprintln!("警告: {msg}");
    if let Ok(mut warnings) = LOAD_WARNINGS.lock() {
        warnings.push(msg);
    }
}
