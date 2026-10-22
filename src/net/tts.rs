//! Edge TTS (微软免费语音合成) — 复刻欧路 exe 内嵌协议
//!
//! 协议逆向自 eudic.exe:
//!   wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1
//!     ?TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4&ConnectionId={uuid}
//!   消息: [headers + 空行 + body], Path: speech.config / ssml / audio / turn.end
//!   音频: audio-24khz-48kbitrate-mono-mp3
//!
//! 兜底后端 `--engine youdao` 走 `dict.youdao.com/dictvoice`：免密钥、只要 GET，
//! 但**查询串里带着原文**，中间环节可能留档；默认仍是 Edge。

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;

use crate::net::sha256::sha256_hex_upper;

pub const EDGE_TTS_WSS: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
pub const EDGE_TTS_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
pub const EDGE_TTS_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
/// Edge 浏览器扩展 Origin (欧路 exe 内嵌同款)
pub const EDGE_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";
/// DRM 版本串 (随 Chromium 版本演进)
pub const GEC_VERSION: &str = "1-143.0.3650.75";
pub const CHROMIUM_MAJOR: &str = "143";

/// 有道发音端点 (免密钥 GET, 文本进查询串)
pub const YOUDAO_ENDPOINT: &str = "https://dict.youdao.com/dictvoice";
/// 有道单条文本上限：它是单词/短句发音端点，长段会被截断或拒绝
pub const YOUDAO_MAX_CHARS: usize = 1000;
/// Edge 语音合成的 SSML 文本上限
pub const EDGE_MAX_CHARS: usize = 10000;
const YOUDAO_MAX_BYTES: usize = 16 * 1024 * 1024;

/// 有道口音：`type=1` 美音、`type=2` 英音（实测两条返回的音频时长不同）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    Us,
    Uk,
}

impl Accent {
    fn youdao_type(self) -> u8 {
        match self {
            Self::Us => 1,
            Self::Uk => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Us => "美音",
            Self::Uk => "英音",
        }
    }

    /// 缓存键用的稳定 ASCII 码
    fn code(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::Uk => "uk",
        }
    }
}

/// 发音后端。CLI 用 `--engine` 选择，缓存键与校验规则都按它分支。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeechSource {
    Edge { voice: String, rate: String },
    Youdao { accent: Accent },
}

impl SpeechSource {
    /// 输出行与 `--json` 共用，免得用户猜默认值
    pub fn describe(&self) -> String {
        match self {
            Self::Edge { voice, rate } => format!("Edge {voice} 语速 {rate}"),
            Self::Youdao { accent } => format!("有道 {}", accent.label()),
        }
    }
}

/// Sec-MS-GEC: (unix+WIN_EPOCH) 下取整到 5 分钟 → ×10_000_000 (100ns 刻度) → SHA256(token) 大写
fn sec_ms_gec() -> String {
    const WIN_EPOCH: u64 = 11644473600;
    const TICKS_PER_SEC: u64 = 10_000_000;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut ticks = now + WIN_EPOCH;
    ticks -= ticks % 300;
    ticks *= TICKS_PER_SEC;
    sha256_hex_upper(format!("{ticks}{EDGE_TTS_TOKEN}").as_bytes())
}

/// 常用声音 (语音名可按需扩展, 服务端 AppTTSConfig 亦可下发)
#[allow(dead_code)]
pub const VOICES: &[(&str, &str)] = &[
    ("en-US-AriaNeural", "美音 女"),
    ("en-US-GuyNeural", "美音 男"),
    ("en-US-AnaNeural", "美音 女童"),
    ("en-GB-SoniaNeural", "英音 女"),
    ("en-GB-RyanNeural", "英音 男"),
    ("zh-CN-XiaoxiaoNeural", "中文 女"),
    ("zh-CN-YunxiNeural", "中文 男"),
];

/// SSML (与 exe 内嵌格式一致)
pub fn build_ssml(text: &str, voice: &str, rate: &str) -> String {
    let voice = html_escape::encode_double_quoted_attribute(voice).replace('\'', "&apos;");
    let rate = html_escape::encode_double_quoted_attribute(rate).replace('\'', "&apos;");
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;");
    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
         <voice name='{voice}'><prosody pitch='+0Hz' rate='{rate}' volume='+0%'>\
         {escaped}</prosody></voice></speak>"
    )
}

/// speech.config 请求块 (与 exe 内嵌字符串一致)
fn speech_config() -> String {
    let timestamp = httpdate::fmt_http_date(SystemTime::now());
    format!(
        "X-Timestamp:{timestamp}\r\n\
         Content-Type:application/json; charset=utf-8\r\n\
         Path:speech.config\r\n\r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\
         \"sentenceBoundaryEnabled\":false,\"wordBoundaryEnabled\":false}},\
         \"outputFormat\":\"{EDGE_TTS_FORMAT}\"}}}}}}}}\r\n"
    )
}

fn ssml_message(ssml: &str, request_id: &str) -> String {
    let timestamp = httpdate::fmt_http_date(SystemTime::now());
    format!(
        "X-RequestId:{request_id}\r\nContent-Type:application/ssml+xml\r\n\
         X-Timestamp:{timestamp}\r\nPath:ssml\r\n\r\n{ssml}"
    )
}

/// 合成并保存 mp3. 返回写入字节数.
async fn synthesize_inner(
    text: &str,
    voice: &str,
    rate: &str,
    out_path: &std::path::Path,
) -> Result<u64> {
    let conn_id: String = uuid::Uuid::new_v4().simple().to_string();
    let base = edge_endpoint()?;
    let sep = if base.contains('?') { '&' } else { '?' };
    let url = format!(
        "{base}{sep}TrustedClientToken={EDGE_TTS_TOKEN}&ConnectionId={conn_id}\
         &Sec-MS-GEC={}&Sec-MS-GEC-Version={GEC_VERSION}",
        sec_ms_gec()
    );

    let mut request = url.as_str().into_client_request()?;
    let headers = request.headers_mut();
    let muid: String = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
    let h: [(&str, String); 6] = [
        ("Origin", EDGE_ORIGIN.to_string()),
        (
            "User-Agent",
            format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROMIUM_MAJOR}.0.0.0 Safari/537.36 Edg/{CHROMIUM_MAJOR}.0.0.0"
            ),
        ),
        ("Pragma", "no-cache".into()),
        ("Cache-Control", "no-cache".into()),
        ("Accept-Encoding", "gzip, deflate, br".into()),
        ("Cookie", format!("muid={muid};")),
    ];
    for (name, value) in h {
        headers.insert(
            HeaderName::from_lowercase(name.to_lowercase().as_bytes()).unwrap(),
            HeaderValue::from_str(&value)?,
        );
    }

    let (ws, _) = tokio::time::timeout(std::time::Duration::from_secs(10), connect_async(request))
        .await
        .context("语音连接超时")?
        .with_context(|| format!("connect {url}"))?;
    let (mut tx, mut rx) = ws.split();

    // 1. speech.config
    tx.send(Message::Text(speech_config())).await?;

    // 2. ssml
    let request_id: String = uuid::Uuid::new_v4().simple().to_string();
    tx.send(Message::Text(ssml_message(
        &build_ssml(text, voice, rate),
        &request_id,
    )))
    .await?;

    // 3. 收音频直到 turn.end
    // 帧格式: [u16 BE header_len][header][audio body]
    let mut mp3: Vec<u8> = Vec::new();
    let mut finished = false;
    while let Some(msg) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.next())
        .await
        .context("语音响应超时")?
    {
        match msg? {
            Message::Binary(data) => {
                mp3.extend_from_slice(audio_payload(&data)?);
                anyhow::ensure!(mp3.len() <= 32 * 1024 * 1024, "音频超过 32 MiB");
            }
            Message::Text(t) => {
                if t.contains("Path:turn.end") {
                    finished = true;
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = tx.send(Message::Close(None)).await;

    anyhow::ensure!(finished, "语音流提前关闭，未保存不完整音频");
    if mp3.is_empty() {
        anyhow::bail!("未收到音频数据");
    }
    tokio::fs::write(out_path, &mp3).await?;
    Ok(mp3.len() as u64)
}

fn audio_payload(data: &[u8]) -> Result<&[u8]> {
    anyhow::ensure!(data.len() >= 2, "音频帧缺少头部长度");
    let end = 2 + u16::from_be_bytes([data[0], data[1]]) as usize;
    anyhow::ensure!(end <= data.len(), "音频帧头部长度越界");
    let header = std::str::from_utf8(&data[2..end]).context("音频头部 UTF-8 无效")?;
    anyhow::ensure!(
        header.lines().any(|l| l.trim() == "Path:audio"),
        "不是音频帧"
    );
    Ok(&data[end..])
}

pub fn validate_for(text: &str, src: &SpeechSource) -> Result<()> {
    match src {
        SpeechSource::Edge { voice, rate } => validate_edge(text, voice, rate),
        SpeechSource::Youdao { .. } => validate_text(text, YOUDAO_MAX_CHARS),
    }
}

fn validate_text(text: &str, max_chars: usize) -> Result<()> {
    anyhow::ensure!(
        !text.trim().is_empty() && text.chars().count() <= max_chars,
        "语音文本须为 1–{max_chars} 字符"
    );
    anyhow::ensure!(
        !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        "语音文本含无效控制字符"
    );
    Ok(())
}

fn validate_edge(text: &str, voice: &str, rate: &str) -> Result<()> {
    validate_text(text, EDGE_MAX_CHARS)?;
    anyhow::ensure!(
        !voice.is_empty()
            && voice.len() <= 100
            && voice.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "非法语音名称"
    );
    let number = rate
        .strip_suffix('%')
        .context("语速必须是百分比，例如 -30%")?
        .parse::<i32>()
        .context("非法语速")?;
    anyhow::ensure!((-90..=200).contains(&number), "语速范围 -90%..+200%");
    Ok(())
}

/// 有道发音 URL：文本整体百分号编码，`type` 决定口音
pub fn youdao_url(text: &str, accent: Accent) -> Result<String> {
    Ok(format!(
        "{}?audio={}&type={}",
        youdao_endpoint()?,
        crate::net::http::percent_encode(text),
        accent.youdao_type()
    ))
}

/// 端点可用 ZEDICT_YOUDAO_URL 覆盖（离线测试与自建代理）；只接受 http/https。
fn youdao_endpoint() -> Result<String> {
    let url = std::env::var("ZEDICT_YOUDAO_URL").unwrap_or_else(|_| YOUDAO_ENDPOINT.into());
    anyhow::ensure!(
        url.starts_with("http://") || url.starts_with("https://"),
        "ZEDICT_YOUDAO_URL 必须以 http:// 或 https:// 开头"
    );
    Ok(url)
}

/// 端点可用 ZEDICT_EDGE_TTS_URL 覆盖（离线测试与自建代理）；只接受 ws/wss。
fn edge_endpoint() -> Result<String> {
    let url = std::env::var("ZEDICT_EDGE_TTS_URL").unwrap_or_else(|_| EDGE_TTS_WSS.into());
    anyhow::ensure!(
        url.starts_with("ws://") || url.starts_with("wss://"),
        "ZEDICT_EDGE_TTS_URL 必须以 ws:// 或 wss:// 开头"
    );
    Ok(url)
}

/// 只按头部判断：ID3v2 或 MPEG 帧同步才算 MP3；HTML 错误页一律否。
pub fn looks_like_mp3(data: &[u8]) -> bool {
    let body = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    let Some(start) = body
        .iter()
        .position(|&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    else {
        return false;
    };
    let body = &body[start..];
    if body.len() < 4 {
        return false;
    }
    if body.starts_with(b"ID3") {
        return true;
    }
    if body[0] == b'<' {
        return false;
    }
    // MPEG 帧头：11 位同步字全 1，且层 ID (bit 2-1) 不是保留值 00
    body[0] == 0xFF && (body[1] & 0xE0) == 0xE0 && (body[1] & 0x06) != 0
}

async fn fetch_youdao(text: &str, accent: Accent, out_path: &std::path::Path) -> Result<u64> {
    let url = youdao_url(text, accent)?;
    let resp = crate::net::http::client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("请求 {url}"))?;
    let status = resp.status();
    anyhow::ensure!(
        status.is_success(),
        "有道发音返回 {status}；这个端点没有密钥也没有配额，可稍后重试或改用 --engine edge"
    );
    let bytes = resp.bytes().await.context("读取有道音频")?;
    anyhow::ensure!(bytes.len() <= YOUDAO_MAX_BYTES, "音频超过 16 MiB");
    anyhow::ensure!(
        looks_like_mp3(&bytes),
        "有道返回的不是 MP3 (多半是被限流或回了网页)；可改用 --engine edge"
    );
    crate::app::cache::write_bytes(out_path, &bytes)?;
    Ok(bytes.len() as u64)
}

/// 合成/下载并保存 mp3，返回写入字节数。失败不会在 `out_path` 留下半个文件。
pub async fn synthesize(text: &str, src: &SpeechSource, out_path: &std::path::Path) -> Result<u64> {
    validate_for(text, src)?;
    let parent = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建输出目录: {}", parent.display()))?;
    match src {
        SpeechSource::Edge { voice, rate } => {
            // 临时文件与目标同目录，sync 后 rename
            let tmp = tempfile::NamedTempFile::new_in(parent)?;
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                synthesize_inner(text, voice, rate, tmp.path()),
            )
            .await
            .context("语音请求总时长超限")??;
            tmp.as_file().sync_all()?;
            tmp.persist(out_path).map_err(|e| e.error)?;
            Ok(n)
        }
        SpeechSource::Youdao { accent } => fetch_youdao(text, *accent, out_path).await,
    }
}

/// 音频缓存键。Edge 分支必须与历史 `(text, voice, rate)` 的序列化结果逐字节相同，
/// 否则升级一次就把用户已经攒下的 mp3 全部作废。
pub fn cache_key(text: &str, src: &SpeechSource) -> Result<String> {
    let bytes = match src {
        SpeechSource::Edge { voice, rate } => serde_json::to_vec(&(text, voice, rate))?,
        SpeechSource::Youdao { accent } => serde_json::to_vec(&("youdao", text, accent.code()))?,
    };
    Ok(crate::data::ingest::hash_bytes(&bytes))
}

/// 取缓存里或新合成的音频路径，不启动播放器。
pub async fn cached_audio(text: &str, src: &SpeechSource) -> Result<PathBuf> {
    validate_for(text, src)?;
    let file = crate::app::cache::path("audio", &cache_key(text, src)?, "mp3")?;
    // Playback is asynchronous: cache rather than delete a file the player still uses.
    if !file.is_file() {
        synthesize(text, src, &file).await?;
    }
    Ok(file)
}

pub async fn speak(text: &str, src: &SpeechSource) -> Result<u64> {
    let file = cached_audio(text, src).await?;
    let bytes = std::fs::metadata(&file)?.len();
    open::that(&file).with_context(|| {
        format!(
            "无法启动系统播放器；音频保存在 {}，可使用 speak --out 保存",
            file.display()
        )
    })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(voice: &str, rate: &str) -> SpeechSource {
        SpeechSource::Edge {
            voice: voice.into(),
            rate: rate.into(),
        }
    }

    #[test]
    fn rejects_ssml_injection_and_accepts_negative_rate() {
        assert!(validate_edge("test", "en-US-AriaNeural", "-30%").is_ok());
        assert!(validate_edge("test", "x' /><voice name='y", "0%").is_err());
        assert!(validate_edge("test", "en-US-AriaNeural", "-100%").is_err());
        assert!(!build_ssml("<tag>", "x'", "0%").contains("name='x''"));
    }

    #[test]
    fn youdao_accepts_a_short_phrase_and_rejects_a_long_text() {
        assert!(validate_for("robust", &SpeechSource::Youdao { accent: Accent::Uk }).is_ok());
        let long = "word ".repeat(YOUDAO_MAX_CHARS);
        assert!(validate_for(&long, &edge("en-US-AriaNeural", "+0%")).is_ok());
        assert!(validate_for(&long, &SpeechSource::Youdao { accent: Accent::Us }).is_err());
        assert!(validate_for("  ", &SpeechSource::Youdao { accent: Accent::Us }).is_err());
    }

    #[test]
    fn youdao_url_escapes_the_text_and_picks_the_accent() {
        assert!(youdao_url("apple", Accent::Us)
            .unwrap()
            .ends_with("&type=1"));
        assert!(youdao_url("apple", Accent::Uk)
            .unwrap()
            .ends_with("&type=2"));
        let url = youdao_url("a b&c+dé😀", Accent::Us).unwrap();
        assert!(
            url.contains("audio=a%20b%26c%2Bd%C3%A9%F0%9F%98%80"),
            "{url}"
        );
        assert!(url.starts_with(YOUDAO_ENDPOINT), "{url}");
        assert!(!url.contains(' '));
    }

    #[test]
    fn mp3_sniffing_accepts_real_frames_and_refutes_pages() {
        assert!(looks_like_mp3(b"ID3\x03\x00\x00\x00\x00\x00\x21"));
        assert!(looks_like_mp3(&[0xFF, 0xFB, 0x64, 0xC3, 0x00]));
        assert!(looks_like_mp3(b"\xEF\xBB\xBF\xFF\xFA\x90\x44"));
        assert!(!looks_like_mp3(b"<html><body>error</body></html>"));
        assert!(!looks_like_mp3(b"  <!DOCTYPE html>\n"));
        // 层 ID 为保留值 00 不是 MPEG 音频
        assert!(!looks_like_mp3(&[0xFF, 0xF0, 0x00, 0x00]));
        assert!(!looks_like_mp3(b""));
        assert!(!looks_like_mp3(b"\xFF\xFB"));
        assert!(!looks_like_mp3(b"plain text body"));
    }

    #[test]
    fn edge_cache_key_is_byte_compatible_with_the_old_shape() {
        let src = edge("en-US-AriaNeural", "+0%");
        assert_eq!(
            cache_key("apple", &src).unwrap(),
            crate::data::ingest::hash_bytes(
                &serde_json::to_vec(&("apple", "en-US-AriaNeural", "+0%")).unwrap()
            )
        );
        let us = SpeechSource::Youdao { accent: Accent::Us };
        let uk = SpeechSource::Youdao { accent: Accent::Uk };
        // 换后端、换口音都得是新键；同口音重复请求才算命中
        assert_ne!(
            cache_key("apple", &src).unwrap(),
            cache_key("apple", &us).unwrap()
        );
        assert_ne!(
            cache_key("apple", &us).unwrap(),
            cache_key("apple", &uk).unwrap()
        );
        assert_eq!(
            cache_key("apple", &us).unwrap(),
            cache_key("apple", &us).unwrap()
        );
    }

    #[test]
    fn describe_shows_the_backend_actually_used() {
        assert_eq!(
            edge("en-GB-RyanNeural", "-30%").describe(),
            "Edge en-GB-RyanNeural 语速 -30%"
        );
        assert_eq!(
            SpeechSource::Youdao { accent: Accent::Uk }.describe(),
            "有道 英音"
        );
    }
}

#[cfg(test)]
mod frame_tests {
    #[test]
    fn preserves_first_audio_bytes_and_rejects_truncation() {
        let header = b"Path:audio\r\nContent-Type:audio/mpeg\r\n";
        let mut frame = Vec::from((header.len() as u16).to_be_bytes());
        frame.extend_from_slice(header);
        frame.extend_from_slice(&[0xff, 0xfb, 1, 2]);
        assert_eq!(super::audio_payload(&frame).unwrap(), [0xff, 0xfb, 1, 2]);
        assert!(super::audio_payload(&[255, 255, 0]).is_err());
        assert!(super::audio_payload(&[]).is_err());
    }
}
