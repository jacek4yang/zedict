use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub db: Option<PathBuf>,
    pub brief: Option<bool>,
    /// 英英释义自动翻译：true 连管道输出也翻，false 从不自动翻；缺省只在交互终端翻
    pub en_zh: Option<bool>,
    /// 终端配色：auto / ascii / rich；命令行与环境变量优先
    pub theme: Option<crate::render::ThemeChoice>,
    pub llm_base_url: Option<String>,
    pub llm_model: Option<String>,
}

pub struct Settings {
    pub config: Config,
    pub config_path: PathBuf,
    pub db: PathBuf,
}

pub fn dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", "zedict").context("无法定位用户目录；请设置用户 HOME/USERPROFILE")
}

/// 网络结果与音频只允许写入缓存目录，绝不写词库。测试可用 ZEDICT_CACHE_DIR 重定向。
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ZEDICT_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs()?.cache_dir().to_path_buf())
}

impl Settings {
    pub fn load(config_path: Option<&Path>, db: Option<&Path>) -> Result<Self> {
        let explicit_config = config_path
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("ZEDICT_CONFIG").map(PathBuf::from));
        let path = match &explicit_config {
            Some(path) => path.clone(),
            None => dirs()?.config_dir().join("config.toml"),
        };
        let config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("配置格式错误: {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit_config.is_none() => {
                Config::default()
            }
            Err(e) => return Err(e).with_context(|| format!("读取配置: {}", path.display())),
        };
        let db = if let Some(path) = db {
            path.to_path_buf()
        } else if let Some(path) = std::env::var_os("ZEDICT_DB") {
            PathBuf::from(path)
        } else if let Some(db) = &config.db {
            if db.is_absolute() {
                db.clone()
            } else {
                path.parent().unwrap_or(Path::new(".")).join(db)
            }
        } else {
            dirs()?.data_dir().join("dict.sqlite")
        };
        Ok(Self {
            config,
            config_path: path,
            db,
        })
    }
}
