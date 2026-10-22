//! 微软 Edge 翻译端点：欧陆整篇对照用的同一条通路，免密钥、返回逐句长度数组。
//!
//! 请求体是**裸 JSON 字符串数组** `["句1","句2"]`；响应每个输入项一个对象，
//! `translations[0].sentLen` 给出 `srcSentLen` / `transSentLen`（UTF-16 码元）。
//! 只有 `srcSentLen` 可信：`transSentLen` 实测会把句子边界算错（详 `align` 模块头），
//! 所以译文一侧由 `align` 按自己的句末标点切。两侧非空句数对不上就整块降级，绝不臆造 1:1。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::net::align::{self, AlignedDoc, Alignment, Block};
use crate::net::http::{percent_encode, truncate, valid_lang};

pub const EDGE_TRANSLATE: &str = "https://edge.microsoft.com/translate/translatetext";
const CACHE_NS: &str = "edge-translate";
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct EdgeClient {
    pub endpoint: String,
    /// None 表示省略 from，让服务端识别语种
    pub from: Option<String>,
    pub to: String,
}

impl EdgeClient {
    pub fn new(from: Option<String>, to: &str) -> Result<Self> {
        anyhow::ensure!(valid_lang(to), "非法目标语言代码: {to}");
        if let Some(from) = &from {
            anyhow::ensure!(valid_lang(from), "非法源语言代码: {from}");
        }
        Ok(Self {
            endpoint: default_endpoint()?,
            from,
            to: to.to_string(),
        })
    }

    /// 测试注入点：指向本地假 HTTP 服务
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.to_string();
        self
    }

    fn url(&self) -> String {
        let mut url = format!(
            "{}?api-version=3.0&includeSentenceLength=true&to={}",
            self.endpoint,
            percent_encode(&self.to)
        );
        if let Some(from) = &self.from {
            url.push_str("&from=");
            url.push_str(&percent_encode(from));
        }
        url
    }

    async fn post_once(&self, inputs: &[String]) -> std::result::Result<Vec<WireItem>, Failure> {
        let body = serde_json::to_vec(inputs).map_err(|err| Failure::Fatal(err.into()))?;
        let resp = crate::net::http::client()
            .post(self.url())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|err| Failure::Fatal(anyhow::anyhow!("Edge 翻译请求失败: {err}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::BAD_REQUEST {
            return Err(Failure::Oversize);
        }
        if matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504) {
            return Err(Failure::RetryLater(
                status.canonical_reason().unwrap_or("限流").into(),
            ));
        }
        if matches!(status.as_u16(), 401 | 403) {
            return Err(Failure::Fatal(anyhow::anyhow!(
                "Edge 翻译端点拒绝访问 (HTTP {})：该端点非公开接口，可能已变更。可改用 --engine llm。",
                status.as_u16()
            )));
        }
        if !status.is_success() {
            let code = status.as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(Failure::Fatal(anyhow::anyhow!(
                "Edge 翻译返回 HTTP {code}: {}",
                truncate(&text, 300)
            )));
        }
        let bytes = match resp.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => return Err(Failure::Fatal(anyhow::anyhow!("读取 Edge 翻译响应: {err}"))),
        };
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(Failure::Fatal(anyhow::anyhow!("Edge 翻译响应超过 32 MiB")));
        }
        let items: Vec<WireItem> = match serde_json::from_slice(&bytes) {
            Ok(items) => items,
            Err(err) => {
                let preview = truncate(&String::from_utf8_lossy(&bytes), 200);
                return Err(Failure::Fatal(anyhow::anyhow!(
                    "Edge 翻译响应格式改变: {err}; 响应开头: {preview}"
                )));
            }
        };
        if items.len() != inputs.len() {
            return Err(Failure::Fatal(anyhow::anyhow!(
                "Edge 翻译返回 {} 项，请求 {} 项：响应与请求不对应，拒绝猜测配对",
                items.len(),
                inputs.len()
            )));
        }
        Ok(items)
    }

    /// 400 折半重试（直到剩 1 条），429/5xx 只退避一次。
    /// 折半用显式栈而不是异步递归，免得整个 future 装箱。
    async fn post_batch(&self, inputs: &[String]) -> Result<Vec<WireItem>> {
        anyhow::ensure!(!inputs.is_empty(), "空批次");
        let mut stack: Vec<(usize, usize)> = vec![(0, inputs.len())];
        let mut out: Vec<Option<WireItem>> = (0..inputs.len()).map(|_| None).collect();
        'outer: while let Some((lo, hi)) = stack.pop() {
            let slice = &inputs[lo..hi];
            let mut backed_off = false;
            let items = loop {
                match self.post_once(slice).await {
                    Ok(items) => break items,
                    Err(Failure::Fatal(err)) => return Err(err),
                    Err(Failure::RetryLater(reason)) => {
                        anyhow::ensure!(!backed_off, "Edge 翻译持续限流: {reason}");
                        backed_off = true;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(Failure::Oversize) => {
                        if hi - lo > 1 {
                            let mid = lo + (hi - lo) / 2;
                            stack.push((mid, hi));
                            stack.push((lo, mid));
                            continue 'outer;
                        }
                        let units = inputs.get(lo).map(|s| align::units(s)).unwrap_or(0);
                        anyhow::bail!(
                            "端点拒绝 {units} UTF-16 码元的单条文本：请用 --batch-units 调小分段"
                        );
                    }
                }
            };
            for (offset, item) in items.into_iter().enumerate() {
                out[lo + offset] = Some(item);
            }
        }
        out.into_iter()
            .map(|item| item.context("内部错误：批次缺少响应"))
            .collect()
    }
}

enum Failure {
    /// 请求体过大（只能折半）
    Oversize,
    /// 限流/临时故障（退避一次）
    RetryLater(String),
    Fatal(anyhow::Error),
}

/// 端点可用 ZEDICT_EDGE_TRANSLATE_URL 覆盖（离线测试与自建代理）；只接受 http/https。
fn default_endpoint() -> Result<String> {
    let url = std::env::var("ZEDICT_EDGE_TRANSLATE_URL").unwrap_or_else(|_| EDGE_TRANSLATE.into());
    anyhow::ensure!(
        url.starts_with("http://") || url.starts_with("https://"),
        "ZEDICT_EDGE_TRANSLATE_URL 必须以 http:// 或 https:// 开头"
    );
    Ok(url)
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WireItem {
    #[serde(default)]
    translations: Vec<WireTranslation>,
    #[serde(default)]
    detected_language: Option<WireDetected>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct WireTranslation {
    text: String,
    #[serde(default)]
    sent_len: Option<WireSentLen>,
}

/// 只取 srcSentLen：transSentLen 在真实端点上会把句子边界算错（见 align 模块头）
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
struct WireSentLen {
    #[serde(default)]
    src_sent_len: Option<Vec<usize>>,
}

#[derive(Deserialize, Debug)]
struct WireDetected {
    language: String,
}

/// 缓存里存的是一块译文与原文句长（句长丢了就无法逐句对齐，所以必须一起存）
#[derive(Serialize, Deserialize, Debug)]
struct Cached {
    text: String,
    src: Vec<usize>,
    detected: Option<String>,
}

#[derive(Debug)]
pub struct Report {
    pub doc: AlignedDoc,
    pub requests: usize,
    pub cache_hits: usize,
}

/// 键只含请求要素：不含密钥，也不含本地绝对路径
fn cache_key(client: &EdgeClient, source: &str) -> Result<String> {
    crate::app::cache::key(
        CACHE_NS,
        &(
            client.endpoint.as_str(),
            client.from.as_deref(),
            client.to.as_str(),
            source,
        ),
    )
}

fn cache_file(client: &EdgeClient, source: &str) -> Result<PathBuf> {
    let key = cache_key(client, source)?;
    crate::app::cache::path(CACHE_NS, &key, "json")
}

/// 整篇逐句对照翻译。`on_progress(已处理块数, 总块数)` 只用于打印进度，不参与输出内容。
pub async fn translate_doc(
    client: &EdgeClient,
    text: &str,
    batch_units: usize,
    use_cache: bool,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<Report> {
    let sources = align::fit_blocks(text, batch_units);
    anyhow::ensure!(!sources.is_empty(), "没有可翻译的文本");
    let batches = align::plan_batches(&sources, batch_units, align::MAX_BATCH_ITEMS);

    let mut results: BTreeMap<usize, Cached> = BTreeMap::new();
    let mut requests = 0usize;
    let mut cache_hits = 0usize;
    let mut done = 0usize;
    for batch in batches {
        let mut missing: Vec<usize> = Vec::new();
        for &i in &batch {
            if use_cache {
                let file = cache_file(client, &sources[i])?;
                if let Some(hit) = crate::app::cache::read_json::<Cached>(&file)? {
                    results.insert(i, hit);
                    cache_hits += 1;
                    continue;
                }
            }
            missing.push(i);
        }
        if !missing.is_empty() {
            let inputs: Vec<String> = missing.iter().map(|&i| sources[i].clone()).collect();
            requests += 1;
            let items = client.post_batch(&inputs).await?;
            for (&i, item) in missing.iter().zip(items) {
                let WireItem {
                    translations,
                    detected_language,
                } = item;
                let translation = translations
                    .into_iter()
                    .next()
                    .context("Edge 翻译返回空 translations，未保存不完整结果")?;
                let WireTranslation { text, sent_len } = translation;
                let sent_len = sent_len.unwrap_or_default();
                let stored = Cached {
                    text,
                    src: sent_len.src_sent_len.unwrap_or_default(),
                    detected: detected_language.map(|d| d.language),
                };
                anyhow::ensure!(
                    !stored.text.trim().is_empty(),
                    "Edge 翻译返回空译文，未保存不完整结果"
                );
                if use_cache {
                    let file = cache_file(client, &sources[i])?;
                    crate::app::cache::write_json(&file, &stored)?;
                }
                results.insert(i, stored);
            }
        }
        done += batch.len();
        on_progress(done, sources.len());
    }

    let detected = results
        .values()
        .find_map(|c| c.detected.clone())
        .unwrap_or_else(|| client.from.clone().unwrap_or_else(|| "unknown".into()));
    let source_lang = client.from.clone().unwrap_or(detected);
    let mut doc = AlignedDoc::new("edge", &source_lang, client.from.is_none(), &client.to);
    for (i, source) in sources.iter().enumerate() {
        let cached = results
            .get(&i)
            .with_context(|| format!("内部错误：第 {} 块缺少译文", i + 1))?;
        let sentences = if cached.src.is_empty() {
            None
        } else {
            align::pair_by_source_units(source, &cached.text, &cached.src)
        };
        doc.push(match sentences {
            Some(sentences) => Block {
                paragraph: i,
                alignment: Alignment::Verified,
                sentences,
            },
            None => align::fallback_block(i, source, &cached.text),
        });
    }
    Ok(Report {
        doc,
        requests,
        cache_hits,
    })
}

/// 逐条翻译短句：输入与输出一一对应，不做句子对齐。
/// 用于查词时给英英释义补中文，调用方按原索引取用即可。
pub async fn translate_items(
    client: &EdgeClient,
    inputs: &[String],
    use_cache: bool,
) -> Result<Vec<String>> {
    anyhow::ensure!(!inputs.is_empty(), "没有可翻译的文本");
    let mut out: Vec<Option<String>> = (0..inputs.len()).map(|_| None).collect();
    let mut missing: Vec<usize> = Vec::new();
    for (index, source) in inputs.iter().enumerate() {
        if use_cache {
            let file = cache_file(client, source)?;
            if let Some(hit) = crate::app::cache::read_json::<Cached>(&file)? {
                out[index] = Some(hit.text);
                continue;
            }
        }
        missing.push(index);
    }

    if !missing.is_empty() {
        let sources: Vec<String> = missing.iter().map(|&index| inputs[index].clone()).collect();
        let mut done = 0;
        for batch in align::plan_batches(&sources, 3600, align::MAX_BATCH_ITEMS) {
            let batch_inputs: Vec<String> =
                batch.iter().map(|&index| sources[index].clone()).collect();
            let items = client.post_batch(&batch_inputs).await?;
            for (offset, item) in items.into_iter().enumerate() {
                let WireItem {
                    translations,
                    detected_language,
                } = item;
                let translation = translations
                    .into_iter()
                    .next()
                    .context("Edge 翻译返回空 translations，未保存不完整结果")?;
                let WireTranslation { text, sent_len: _ } = translation;
                let text = text.trim().to_string();
                anyhow::ensure!(!text.is_empty(), "Edge 翻译返回空译文，未保存不完整结果");
                let index = missing[done + offset];
                if use_cache {
                    let stored = Cached {
                        text: text.clone(),
                        src: Vec::new(),
                        detected: detected_language.map(|d| d.language),
                    };
                    crate::app::cache::write_json(&cache_file(client, &inputs[index])?, &stored)?;
                }
                out[index] = Some(text);
            }
            done += batch.len();
        }
    }

    out.into_iter()
        .map(|translation| translation.context("内部错误：缺少译文"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_carries_sentence_lengths_and_optional_source() {
        let auto = EdgeClient::new(None, "zh-Hans").unwrap();
        assert!(auto.url().contains("includeSentenceLength=true"));
        assert!(auto.url().ends_with("&to=zh-Hans"));
        assert!(!auto.url().contains("&from="));
        assert!(auto.url().starts_with(EDGE_TRANSLATE));
        let src = EdgeClient::new(Some("en".into()), "zh-Hans").unwrap();
        assert!(src.url().ends_with("&to=zh-Hans&from=en"));
        assert_eq!(
            auto.clone().with_endpoint("http://127.0.0.1:1/x").url(),
            "http://127.0.0.1:1/x?api-version=3.0&includeSentenceLength=true&to=zh-Hans"
        );
    }

    #[test]
    fn refuses_unsafe_language_codes() {
        assert!(EdgeClient::new(None, "zh-Hans;echo").is_err());
        assert!(EdgeClient::new(Some("../x".into()), "zh-Hans").is_err());
        assert!(EdgeClient::new(None, "").is_err());
    }

    #[test]
    fn cache_key_tracks_every_request_parameter() {
        let client = EdgeClient::new(Some("en".into()), "zh-Hans").unwrap();
        let key = cache_key(&client, "I have a pen.").unwrap();
        assert!(key.starts_with(CACHE_NS));
        assert!(!key.contains("I have a pen."), "键里不出现原文");
        assert_eq!(cache_key(&client, "I have a pen.").unwrap(), key);
        assert_ne!(cache_key(&client, "Other.").unwrap(), key);
        let other = EdgeClient::new(Some("fr".into()), "zh-Hans").unwrap();
        assert_ne!(cache_key(&other, "I have a pen.").unwrap(), key);
        let host = client.clone().with_endpoint("http://127.0.0.1:1/x");
        assert_ne!(cache_key(&host, "I have a pen.").unwrap(), key);
    }
}
