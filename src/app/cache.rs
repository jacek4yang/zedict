//! 网络结果的本地缓存 (词库以只读打开，任何联网数据都不落 dict.sqlite)

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub fn dir(namespace: &str) -> Result<PathBuf> {
    let path = crate::app::config::cache_dir()?.join(namespace);
    std::fs::create_dir_all(&path).with_context(|| format!("创建缓存目录: {}", path.display()))?;
    Ok(path)
}

/// 键只包含命名空间与可公开的请求要素；调用方不得把 api_key 或绝对路径放进 payload。
pub fn key<T: Serialize>(namespace: &str, payload: &T) -> Result<String> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!(
        "{namespace}-{}",
        crate::data::ingest::hash_bytes(&bytes)
    ))
}

pub fn path(namespace: &str, key: &str, ext: &str) -> Result<PathBuf> {
    Ok(dir(namespace)?.join(format!("{key}.{ext}")))
}

pub fn read_json<T: DeserializeOwned>(file: &Path) -> Result<Option<T>> {
    match std::fs::read(file) {
        Ok(bytes) => {
            Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("缓存格式错误: {}", file.display())
            })?))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("读取缓存: {}", file.display())),
    }
}

/// 与 tts 保存音频同样的原子写：临时文件 → sync → rename。
pub fn write_json<T: Serialize>(file: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建缓存目录: {}", parent.display()))?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(file).map_err(|e| e.error)?;
    Ok(())
}

pub fn write_bytes(file: &Path, bytes: &[u8]) -> Result<()> {
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建输出目录: {}", parent.display()))?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(file).map_err(|e| e.error)?;
    Ok(())
}
