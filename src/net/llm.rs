//! LLM 翻译 (OpenAI 兼容协议, SSE 流式)
//!
//! 提示词基于欧路逆向模板增强: system 隔离 + 上下文/摘要/术语表注入。
//! 除整段流式翻译外，还提供 `translate_block`：一次请求返回**逐项对应**的译文数组，
//! 用于逐句对照（条数不符即报错，绝不静默错位）。

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::net::http::{truncate, USER_AGENT};

#[derive(Clone)]
pub struct LlmConfig {
    pub base_url: String, // 如 https://api.deepseek.com/v1
    pub api_key: String,
    pub model: String,
}

impl LlmConfig {
    pub fn from_env() -> Result<Self> {
        let base_url =
            std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com/v1".into());
        let api_key =
            std::env::var("LLM_API_KEY").context("缺少 LLM_API_KEY 环境变量 (或用 --api-key)")?;
        let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
        Ok(Self {
            base_url,
            api_key,
            model,
        })
    }

    pub fn explicit(
        base: Option<String>,
        key: Option<String>,
        model: Option<String>,
    ) -> Result<Self> {
        let base_url = base
            .or_else(|| std::env::var("LLM_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.deepseek.com/v1".into());
        let api_key = key
            .or_else(|| std::env::var("LLM_API_KEY").ok())
            .context("缺少 API key (--api-key 或 LLM_API_KEY)")?;
        let model = model
            .or_else(|| std::env::var("LLM_MODEL").ok())
            .unwrap_or_else(|| "deepseek-chat".into());
        Ok(Self {
            base_url,
            api_key,
            model,
        })
    }

    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TranslateMode {
    /// 自然意译
    #[default]
    Natural,
    /// 直译对照
    Literal,
    /// 语法讲解 + 翻译 (长句拆解)
    Explain,
}

impl TranslateMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "literal" => Self::Literal,
            "explain" => Self::Explain,
            _ => Self::Natural,
        }
    }

    /// 这个风格要求模型做什么。目标语言是这段话里唯一必须由 `--to` 决定的部分：
    /// 以前它写死成"简体中文"，于是 `--engine llm --to fr` 会一边收下 `fr`
    /// 一边译回中文。
    fn task(&self, to: &str) -> String {
        match self {
            Self::Natural => format!("翻译成自然通顺的{to}"),
            Self::Literal => format!("逐词直译成{to}，尽量保持原句结构"),
            Self::Explain => format!("给出语法讲解（主干/从句/特殊结构）与{to}翻译"),
        }
    }
}

/// 提示词里要说出口的"目标语言/源语言"：代码给人看，名字给模型看。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub code: String,
    pub name: String,
}

/// 已知语言代码 → 中文名。表外的**合法**代码不算错误：原样写进提示词。
/// 只支持这十种、其余悄悄退回中文，才是 `--to` 失效的老毛病。
const LANGS: &[(&str, &str)] = &[
    ("zh-Hans", "简体中文"),
    ("zh-Hant", "繁体中文"),
    ("en", "英语"),
    ("fr", "法语"),
    ("de", "德语"),
    ("es", "西班牙语"),
    ("it", "意大利语"),
    ("ru", "俄语"),
    ("ja", "日语"),
    ("ko", "韩语"),
];

impl Target {
    /// 校验并解释一个语言代码。形状非法就失败——`--to` 从来不是一个可以忽略的参数。
    pub fn parse(code: &str) -> Result<Self> {
        anyhow::ensure!(crate::net::http::valid_lang(code), "非法语言代码: {code}");
        Ok(Self {
            name: LANGS
                .iter()
                .find(|(known, _)| *known == code)
                .map(|(_, name)| (*name).to_string())
                .unwrap_or_else(|| code.to_string()),
            code: code.to_string(),
        })
    }

    /// `--to` 的默认值，和 CLI 的 `default_value` 保持一致。
    pub fn chinese() -> Self {
        Self {
            code: "zh-Hans".into(),
            name: "简体中文".into(),
        }
    }
}

impl Default for Target {
    fn default() -> Self {
        Self::chinese()
    }
}

/// 术语表条目 (保证术语一致性)
#[derive(Debug, Clone, Default)]
pub struct Glossary(pub Vec<(String, String)>);

pub const MAX_GLOSSARY_ROWS: usize = 2000;

/// TSV 术语表：`英文<TAB>中文`，`#` 开头为注释。格式错误按参数错误处理。
pub fn load_glossary(path: &Path) -> Result<Glossary> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("读取术语表: {}", path.display()))?;
    let mut rows = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (source, target) = line
            .split_once('\t')
            .with_context(|| format!("术语表第 {} 行缺少制表符", number + 1))?;
        let (source, target) = (source.trim(), target.trim());
        anyhow::ensure!(
            !source.is_empty() && !target.is_empty(),
            "术语表第 {} 行两侧都不能为空",
            number + 1
        );
        anyhow::ensure!(
            rows.len() < MAX_GLOSSARY_ROWS,
            "术语表最多 {MAX_GLOSSARY_ROWS} 行"
        );
        rows.push((source.to_string(), target.to_string()));
    }
    Ok(Glossary(rows))
}

fn glossary_lines(glossary: Option<&Glossary>, out: &mut String) {
    if let Some(g) = glossary {
        if !g.0.is_empty() {
            out.push_str("[术语表] (必须采用以下译法)\n");
            for (src, dst) in &g.0 {
                out.push_str(&format!("  {src} → {dst}\n"));
            }
            out.push('\n');
        }
    }
}

/// 用户自带的提示词模板（`--style-file`）。它替换的是**用户消息**，system 消息仍由
/// 内置风格生成——一段不声明语言的模板会让 `--to` 第二次失效。
#[derive(Clone, Debug)]
pub struct Template(String);

/// 模板可用的变量。名字写进错误信息，用户不必去猜。
const TEMPLATE_VARS: &[&str] = &[
    "text",
    "src_lang",
    "src_lang_cn",
    "target_lang",
    "target_lang_cn",
    "context",
    "summary",
    "prev_output",
    "count",
];

const OPEN: &str = "{{";
const CLOSE: &str = "}}";

impl Template {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取提示词模板: {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("提示词模板 {}", path.display()))
    }

    /// 载入即校验：未知变量名和缺 `{{text}}` 都是配置错误，不该等到发请求时才炸。
    pub fn parse(text: &str) -> Result<Self> {
        anyhow::ensure!(!text.trim().is_empty(), "模板是空的");
        let used = variables(text)?;
        anyhow::ensure!(
            used.iter().any(|name| *name == "text"),
            "模板必须包含 {OPEN}text{CLOSE}——原文没有别的入口"
        );
        Ok(Self(text.to_string()))
    }

    /// 按变量表渲染。`render` 不再会失败：非法变量名在 `parse` 就该被拦下。
    pub fn render(&self, prompt: &Prompt<'_>, text: &str, count: usize) -> String {
        let value = |name: &str| -> String {
            match name {
                "text" => text.to_string(),
                "src_lang" => prompt
                    .from
                    .as_ref()
                    .map(|from| from.code.clone())
                    .unwrap_or_else(|| "unknown".into()),
                "src_lang_cn" => prompt
                    .from
                    .as_ref()
                    .map(|from| from.name.clone())
                    .unwrap_or_else(|| "未声明".into()),
                "target_lang" => prompt.to.code.clone(),
                "target_lang_cn" => prompt.to.name.clone(),
                "context" => prompt.context.unwrap_or_default().to_string(),
                "summary" => prompt.summary.unwrap_or_default().to_string(),
                "prev_output" => prompt.prev_output.unwrap_or_default().to_string(),
                "count" => count.to_string(),
                _ => String::new(),
            }
        };
        let mut out = String::new();
        let mut rest = self.0.as_str();
        while let Some(start) = rest.find(OPEN) {
            out.push_str(&rest[..start]);
            let tail = &rest[start + OPEN.len()..];
            match tail.find(CLOSE) {
                Some(end) if !tail[..end].is_empty() => {
                    out.push_str(&value(tail[..end].trim()));
                    rest = &tail[end + CLOSE.len()..];
                }
                _ => {
                    // 校验过的模板到这里只可能是落单的 `{{`，原样留着。
                    out.push_str(OPEN);
                    rest = tail;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// 扫出 `{{name}}` 里的变量名，顺手挡住拼错的名字和没闭合的花括号。
fn variables(text: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let tail = &rest[start + OPEN.len()..];
        let end = tail
            .find(CLOSE)
            .with_context(|| format!("{OPEN} 没有配对的 {CLOSE}"))?;
        let name = tail[..end].trim();
        anyhow::ensure!(!name.is_empty(), "{OPEN}{CLOSE} 里少了变量名");
        anyhow::ensure!(
            TEMPLATE_VARS.contains(&name),
            "模板含未知变量 {OPEN}{name}{CLOSE}；支持的变量：{}",
            TEMPLATE_VARS.join("、")
        );
        found.push(name.to_string());
        rest = &tail[end + CLOSE.len()..];
    }
    anyhow::ensure!(
        !rest.contains(OPEN),
        "模板里有落单的 {OPEN}；变量必须写成 {OPEN}name{CLOSE}"
    );
    Ok(found)
}

/// 一次翻译请求里"怎么说"和"就这些上下文"的全部输入。用结构体而不是继续加位置
/// 参数：`--to`、`--summary`、`--prev-output`、`--style-file` 每个都会多一个
/// `Option<&str>`，调用点迟早把两个同类型的参数写反。
#[derive(Clone, Default)]
pub struct Prompt<'a> {
    pub mode: TranslateMode,
    /// 源语言；`None` = `--from auto`，即不向模型声明源语言
    pub from: Option<Target>,
    pub to: Target,
    pub context: Option<&'a str>,
    pub summary: Option<&'a str>,
    /// 上一段译文的尾部。分段翻译要保证跨段连贯，光有摘要不够。
    pub prev_output: Option<&'a str>,
    pub glossary: Option<&'a Glossary>,
    pub template: Option<&'a Template>,
}

impl Prompt<'_> {
    /// 语言声明。`--from auto` 写的是"不声明"，不是"英语"——那是这个标志的含义。
    fn languages(&self) -> String {
        let mut s = format!("目标语言：{}（{}）。", self.to.name, self.to.code);
        match &self.from {
            Some(from) => s.push_str(&format!("源语言：{}（{}）。", from.name, from.code)),
            None => s.push_str("源语言不声明，由你判断。"),
        }
        s
    }
}

/// 摘要 / 上一段译文 / 上下文 / 术语表四项注入。内置模板和用户模板走同一个函数：
/// `--style-file` 换掉的应该只有措辞，不该还有注入顺序。
fn injections(prompt: &Prompt<'_>, out: &mut String) {
    if let Some(s) = prompt.summary {
        out.push_str(&format!("[前文摘要] {s}\n\n"));
    }
    if let Some(tail) = prompt.prev_output {
        out.push_str(&format!("[上一段译文] {tail}\n\n"));
    }
    if let Some(c) = prompt.context {
        out.push_str(&format!("[上下文] {c}\n\n"));
    }
    glossary_lines(prompt.glossary, out);
}

/// 构造 system prompt
fn system_prompt(prompt: &Prompt<'_>) -> String {
    let mut p = String::from("You are a professional translation engine. ");
    p.push_str(&prompt.languages());
    p.push_str(&prompt.mode.task(&prompt.to.name));
    p.push(' ');
    p.push_str(match prompt.mode {
        TranslateMode::Natural => {
            "不要解释、不加注释；保留原有换行、技术术语、引用标号 [12] 和数学符号。"
        }
        TranslateMode::Literal => "不要解释；保留原有换行。",
        TranslateMode::Explain => {
            "用 markdown 小节组织；难懂结构（倒装/分词/虚拟语气）用一句话说明。"
        }
    });
    p
}

/// 用户消息模板 (含上下文与术语)
fn user_prompt(prompt: &Prompt<'_>, text: &str) -> String {
    if let Some(template) = prompt.template {
        return template.render(prompt, text, 1);
    }
    let mut p = String::new();
    injections(prompt, &mut p);
    match prompt.mode {
        TranslateMode::Explain => p.push_str(&format!("[句子]\n{text}")),
        _ => p.push_str(&format!(
            "Translate the text to {}:\n{text}",
            prompt.to.name
        )),
    };
    p
}

/// 逐项翻译的 system prompt：把输出形状钉死成一个长度已知的 JSON 数组
fn pairs_prompt(prompt: &Prompt<'_>, count: usize) -> String {
    format!(
        "You are a professional translation engine. {}输入是一个 JSON 字符串数组，共 {count} 项。\n\
         输出必须是一个长度正好为 {count} 的 JSON 字符串数组：第 i 项是第 i 个输入的{}。\n\
         严格保持顺序一一对应；不要解释来源、不要 markdown 代码围栏、不要加键名。\n\
         某项确实无法翻译时用空字符串 \"\" 占位，绝不允许省略元素或合并多项。",
        prompt.languages(),
        prompt.mode.task(&prompt.to.name)
    )
}

fn pairs_request(prompt: &Prompt<'_>, inputs: &[String]) -> String {
    let array = serde_json::to_string(inputs).unwrap_or_else(|_| "[]".into());
    if let Some(template) = prompt.template {
        return template.render(prompt, &array, inputs.len());
    }
    let mut p = String::new();
    injections(prompt, &mut p);
    p.push_str(&array);
    p
}

// ─────────── 协议结构 ───────────
#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMsg>,
    stream: bool,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMsg {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatChunk {
    choices: Option<Vec<ChunkChoice>>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ChunkChoice {
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
}

/// 流式对话：translate_stream 与 explain 共用同一条 SSE 通路
pub async fn chat_stream(
    cfg: &LlmConfig,
    system: &str,
    user: &str,
    temperature: f32,
    mut on_delta: impl FnMut(&str),
) -> Result<String> {
    let url = cfg.endpoint();
    let req = ChatRequest {
        model: cfg.model.clone(),
        messages: vec![
            ChatMsg {
                role: "system".into(),
                content: system.to_string(),
            },
            ChatMsg {
                role: "user".into(),
                content: user.to_string(),
            },
        ],
        stream: true,
        temperature,
    };

    let client = crate::net::http::client();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header("Accept", "text/event-stream")
        .json(&req)
        .send()
        .await
        .with_context(|| format!("request {url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("LLM API {status}: {}", truncate(&body, 300));
    }

    let mut stream = resp.bytes_stream();
    let mut full = String::new();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.extend_from_slice(&chunk);
        anyhow::ensure!(buf.len() <= 1024 * 1024, "SSE 单个缓冲超过 1 MiB");
        // SSE 按空行分隔
        while let Some(p) = find_double_newline(&buf) {
            let event: Vec<u8> = buf.drain(..p).collect();
            let event = std::str::from_utf8(&event).context("SSE 包含无效 UTF-8")?;
            let joined = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim))
                .collect::<Vec<_>>()
                .join("\n");
            for data in std::iter::once(joined.as_str()).filter(|s| !s.is_empty()) {
                if data == "[DONE]" {
                    return Ok(full);
                }
                let parsed = serde_json::from_str::<ChatChunk>(data).context("SSE JSON 无效")?;
                if let Some(error) = parsed.error {
                    anyhow::bail!("翻译服务返回错误: {}", truncate(&error.to_string(), 300));
                }
                if let Some(choices) = parsed.choices {
                    if let Some(c) = choices.first() {
                        if let Some(reason) = &c.finish_reason {
                            anyhow::ensure!(reason == "stop", "翻译未完整完成: {reason}");
                        }
                        if let Some(d) = &c.delta {
                            if let Some(content) = &d.content {
                                full.push_str(content);
                                anyhow::ensure!(
                                    full.len() <= 8 * 1024 * 1024,
                                    "翻译输出超过 8 MiB"
                                );
                                on_delta(content);
                            }
                        }
                    }
                }
            }
        }
    }
    anyhow::ensure!(buf.len() <= 1024 * 1024, "SSE 单个事件超过 1 MiB");
    anyhow::bail!("翻译流提前关闭，未收到 [DONE]；已输出部分内容，不自动重试")
}

/// 流式翻译: 每收到一段增量就回调
pub async fn translate_stream(
    cfg: &LlmConfig,
    prompt: &Prompt<'_>,
    text: &str,
    on_delta: impl FnMut(&str),
) -> Result<String> {
    chat_stream(
        cfg,
        &system_prompt(prompt),
        &user_prompt(prompt, text),
        // 欧路逆向默认温度
        0.3,
        on_delta,
    )
    .await
}

/// 非流式包装 (HTTP server 用)
pub async fn translate_once(cfg: &LlmConfig, prompt: &Prompt<'_>, text: &str) -> Result<String> {
    let mut full = String::new();
    translate_stream(cfg, prompt, text, |d| full.push_str(d)).await
}

/// 一批输入 → 等长输出。条数对不上时退回按分句位置配对，再对不上就报错。
pub async fn translate_block(
    cfg: &LlmConfig,
    prompt: &Prompt<'_>,
    inputs: &[String],
) -> Result<Vec<String>> {
    anyhow::ensure!(!inputs.is_empty(), "空批次");
    let raw = chat_stream(
        cfg,
        &pairs_prompt(prompt, inputs.len()),
        &pairs_request(prompt, inputs),
        0.3,
        |_| {},
    )
    .await?;
    if let Some(items) = extract_json_array(&raw) {
        if items.len() == inputs.len() {
            return Ok(items);
        }
    }
    let parts = split_sentences(raw.trim());
    anyhow::ensure!(
        parts.len() == inputs.len(),
        "模型返回的条目数与请求不符 (请求 {} 条，可用 {} 条)；逐句对照请改用 --engine edge",
        inputs.len(),
        parts.len()
    );
    Ok(parts)
}

/// 整篇翻译的请求要素：语言、分段预算与可选上下文，打包成一个结构，
/// 免得调用点记十个位置参数的顺序。
pub struct DocJob<'a> {
    /// 提示词要素。其中的 `summary`/`prev_output` 由分段循环逐块改写，调用方给的
    /// 初值只当作第一块的起点。
    pub prompt: Prompt<'a>,
    pub text: &'a str,
    /// 对齐文档元数据里的语言代码：元数据要 `zh-Hans` 这种标签，提示词要
    /// "简体中文"这种人话，两者不是一回事。
    pub from: &'a str,
    pub to: &'a str,
    pub chunk: usize,
    /// 每段带上前一段译文的尾部，保证跨段连贯；`--no-prev-output` 关掉。
    pub prev_output: bool,
}

/// LLM 逐句对照：每个块一次请求，要求返回**等长**的译文数组。
///
/// 与 Edge 通路不同，这里没有服务端句长可校验，所以只认“条数一致”；
/// 一旦模型不配合就报错，不返回可能错位的句对。
pub async fn translate_doc(
    cfg: &LlmConfig,
    job: &DocJob<'_>,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<crate::net::align::AlignedDoc> {
    use crate::net::align::{fit_blocks, plan_batches, AlignedDoc, Alignment, Block, SentencePair};
    let blocks = fit_blocks(job.text, job.chunk.max(1));
    anyhow::ensure!(!blocks.is_empty(), "没有可翻译的文本");
    let mut rolling = RollingSummary::new();
    rolling.override_with(job.prompt.summary);
    let mut prev_output: Option<String> = None;
    let mut doc = AlignedDoc::new("llm", job.from, false, job.to);
    for (index, block) in blocks.iter().enumerate() {
        let mut sentences = split_sentences(block);
        if sentences.is_empty() {
            sentences.push(block.trim().to_string());
        }
        let prompt = Prompt {
            summary: rolling.get(),
            prev_output: job.prev_output.then_some(prev_output.as_deref()).flatten(),
            ..job.prompt.clone()
        };
        let mut translated: Vec<String> = Vec::with_capacity(sentences.len());
        for batch in plan_batches(
            &sentences,
            usize::MAX / 4,
            crate::net::align::MAX_BATCH_ITEMS,
        ) {
            let inputs: Vec<String> = batch.iter().map(|&i| sentences[i].clone()).collect();
            let items = translate_block(cfg, &prompt, &inputs).await?;
            anyhow::ensure!(items.len() == inputs.len(), "内部错误：译文条数与请求不符");
            translated.extend(items);
        }
        let pairs: Vec<SentencePair> = sentences
            .into_iter()
            .zip(translated.iter().cloned())
            .map(|(source, target)| SentencePair {
                source,
                target: target.trim().to_string(),
            })
            .collect();
        doc.push(Block {
            paragraph: index,
            alignment: Alignment::Verified,
            sentences: pairs,
        });
        let tail: String = translated.iter().map(|t| t.as_str()).collect();
        rolling.advance(&tail);
        // 首段没有"上一段"，所以这里宁可留 `None`：提示词里整行不出现，比写一个
        // 空占位更不容易被模型当成"上一段是空的"。
        prev_output = (!tail.trim().is_empty()).then(|| tail_of(&tail, SUMMARY_TAIL_CHARS));
        on_progress(index + 1, blocks.len());
    }
    Ok(doc)
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| p + 2);
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// 从模型输出里取一个 JSON 字符串数组：容忍围栏、前后说明文字与非字符串元素。
pub fn extract_json_array(text: &str) -> Option<Vec<String>> {
    if let Ok(items) = serde_json::from_str::<Vec<String>>(text.trim()) {
        return Some(items);
    }
    let start = text.find('[')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in text.as_bytes().iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    let slice = &text[start..=offset];
                    if let Ok(items) = serde_json::from_str::<Vec<String>>(slice) {
                        return Some(items);
                    }
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(slice) {
                        return value.as_array().map(|arr| {
                            arr.iter()
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .collect()
                        });
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

/// 滚动上下文：默认取上一块**译文尾部**，对应欧陆模板里的 summary 变量
#[derive(Debug, Default, Clone)]
pub struct RollingSummary {
    text: Option<String>,
    tail_chars: usize,
}

pub const SUMMARY_TAIL_CHARS: usize = 400;

impl RollingSummary {
    pub fn new() -> Self {
        Self {
            text: None,
            tail_chars: SUMMARY_TAIL_CHARS,
        }
    }

    /// `--summary` 显式覆盖后不再自动滚动
    pub fn override_with(&mut self, text: Option<&str>) {
        if let Some(text) = text {
            self.text = Some(text.to_string());
            self.tail_chars = 0;
        }
    }

    pub fn get(&self) -> Option<&str> {
        self.text.as_deref()
    }

    pub fn advance(&mut self, translated: &str) {
        if self.tail_chars == 0 || translated.trim().is_empty() {
            return;
        }
        self.text = Some(tail_of(translated, self.tail_chars));
    }
}

/// 取一段文本的尾部若干字符（按字符，不按字节，免得切进 UTF-8 中间）。
fn tail_of(text: &str, budget: usize) -> String {
    let total = text.chars().count();
    text.chars().skip(total.saturating_sub(budget)).collect()
}

/// 注入给下一段的"上一段译文"长度。`RollingSummary` 用的是同一个预算：跨段要接得
/// 上的只是结尾几句，把整段译文重复回去只会挤掉本轮真正要翻的内容。
pub fn translation_tail(text: &str) -> String {
    tail_of(text, SUMMARY_TAIL_CHARS)
}

/// 自动术语表一次最多探多少个候选短语：文档再长，提示词里的术语表也不能无限涨。
const MAX_AUTO_CANDIDATES: usize = 20_000;

/// 自动术语表：从原文里探测本库已收的词头（2–5 词 n-gram），取该键的第一条中文释义。
///
/// 证据阈值不是装饰。一个 E 档词头（挂在 `angle` 名下的"气骨导间距"那类）进了术语表，
/// 就等于把错译法钉在**每一段**译文里，比一张错词卡难查得多。所以缺 `key_evidence`
/// 表时直接报错，不退化成"任意键都算术语"。
///
/// 单词不算术语：一个词的"第一条释义"在多数语境里都是错的（`issue` 先讲"问题"还是
/// "流出"？），而自动注入没有人工过目来兜底。短语才有这个把握。
pub fn auto_glossary(
    store: &crate::data::store::DictStore,
    text: &str,
    evidence: crate::core::evidence::KeyEvidence,
) -> Result<Glossary> {
    anyhow::ensure!(
        store.has_key_evidence(),
        "自动术语表需要词库带 key_evidence 表（词头证据档位）；先重建一次，例如 zedict compile --input data/dict_full.jsonl --key-evidence some --install"
    );
    let words: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphabetic() || c == '-' || c == '\''))
        .filter(|word| !word.is_empty())
        .map(crate::data::store::normalize)
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    for start in 0..words.len() {
        for len in 2..=5usize.min(words.len() - start) {
            let gram = words[start..start + len].join(" ");
            if seen.insert(gram.clone()) {
                candidates.push(gram);
                if candidates.len() == MAX_AUTO_CANDIDATES {
                    break;
                }
            }
        }
        if candidates.len() == MAX_AUTO_CANDIDATES {
            break;
        }
    }
    let mut rows = store.evidenced_keys(&candidates, evidence)?;
    // 长短语排在前面：`as well as` 该整条进表，而不是被 `as` 挤掉位置。
    rows.sort_by(|a, b| {
        b.0.matches(' ')
            .count()
            .cmp(&a.0.matches(' ').count())
            .then_with(|| a.0.cmp(&b.0))
    });
    rows.truncate(MAX_GLOSSARY_ROWS);
    Ok(Glossary(rows))
}

/// 手工表优先：同一条术语用户写过的译法，自动表不能覆盖。
pub fn merge_glossary(manual: Option<Glossary>, auto: Glossary) -> Glossary {
    let mut rows = manual.unwrap_or_default().0;
    let known: std::collections::HashSet<String> =
        rows.iter().map(|(term, _)| term.to_lowercase()).collect();
    rows.extend(
        auto.0
            .into_iter()
            .filter(|(term, _)| !known.contains(&term.to_lowercase())),
    );
    rows.truncate(MAX_GLOSSARY_ROWS);
    Glossary(rows)
}

/// 长文本分段: 按段落贪心聚合到 max_chars；超长段落按句继续拆。
/// 不变量：`chunks.concat() == text`（绝不把句子拦腰切断，除非整句就超预算）。
pub fn split_paragraphs(text: &str, max_chars: usize) -> Vec<String> {
    crate::net::align::pack_units(text, max_chars)
}

/// 简易英文分句
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        if c == '.' || c == '!' || c == '?' {
            // 吞后续引号/括号
            while let Some(&n) = chars.peek() {
                if n == '"' || n == '\'' || n == ')' {
                    cur.push(n);
                    chars.next();
                } else {
                    break;
                }
            }
            // 缩写保护: 单字母 + '.'
            let trimmed = cur.trim_end();
            if trimmed.len() >= 2 {
                let prev = trimmed.chars().rev().nth(1);
                if matches!(prev, Some(c) if c.is_ascii_uppercase()) {
                    continue; // e.g. "U.S." 不切
                }
            }
            if cur.trim().len() > 1 {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_array_extraction_survives_fences_and_preamble() {
        assert_eq!(
            extract_json_array("[\"一\",\"二\"]").unwrap(),
            ["一".to_string(), "二".to_string()]
        );
        let wrapped = "好的，这是结果：\n```json\n[\"一\", \"二\"]\n```\n希望有帮助";
        assert_eq!(extract_json_array(wrapped).unwrap().len(), 2);
        assert_eq!(
            extract_json_array("[\"a[1]b\", \"c\\\"d\"]").unwrap(),
            ["a[1]b".to_string(), "c\"d".to_string()]
        );
        assert_eq!(extract_json_array("[]").unwrap().len(), 0);
        assert!(extract_json_array("没有数组").is_none());
        assert!(extract_json_array("[\"未闭合").is_none());
    }

    #[test]
    fn chunking_never_splits_the_text() {
        let text = "第一段。\n\n第二段。\n\n第三段。";
        for budget in [1, 2, 5, 40, 4000] {
            let chunks = split_paragraphs(text, budget);
            assert_eq!(chunks.concat(), text, "预算 {budget}");
            assert!(chunks.iter().all(|c| c.chars().count() <= budget.max(1)));
        }
        assert!(split_paragraphs("", 10).is_empty());
        assert!(split_paragraphs("abc", 0).is_empty());
        // 小段落会被贪心合并，减少请求数
        assert_eq!(split_paragraphs(text, 200), vec![text.to_string()]);
    }

    #[test]
    fn rolling_summary_tracks_translation_tail() {
        let mut summary = RollingSummary::new();
        assert_eq!(summary.get(), None);
        summary.advance(&"x".repeat(900));
        assert_eq!(summary.get().unwrap().chars().count(), SUMMARY_TAIL_CHARS);
        summary.override_with(Some("用户给定摘要"));
        summary.advance("新译文");
        assert_eq!(summary.get().unwrap(), "用户给定摘要");
    }

    #[test]
    fn glossary_rejects_loose_rows() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.tsv");
        std::fs::write(&good, "# 注释\n\nkernel\t内核\n  rain  \n").unwrap();
        let err = load_glossary(&good).unwrap_err().to_string();
        assert!(err.contains("第 4 行缺少制表符"), "{err}");
        std::fs::write(&good, "kernel\t内核\nrain\t雨\n").unwrap();
        assert_eq!(load_glossary(&good).unwrap().0.len(), 2);
        std::fs::write(&good, "\t空左边\n").unwrap();
        assert!(load_glossary(&good).is_err());
        assert!(load_glossary(&dir.path().join("missing.tsv")).is_err());
    }

    #[test]
    fn pairs_prompt_pins_the_array_length() {
        let prompt = pairs_prompt(&Prompt::default(), 7);
        assert!(prompt.contains("正好为 7"));
        let request = pairs_request(
            &Prompt {
                context: Some("论文"),
                summary: Some("前文"),
                glossary: Some(&Glossary(vec![("model".into(), "模型".into())])),
                ..Default::default()
            },
            &["A.".to_string(), "B.".to_string()],
        );
        assert!(request.contains("[\"A.\",\"B.\"]"));
        assert!(request.contains("model → 模型"));
        assert!(request.contains("[上下文] 论文"));
    }

    /// `--to` 必须同时改到三处提示词：system、单条 user、批量 pairs。只改一处
    /// 就等于继续对非中文用户撒谎。
    #[test]
    fn the_target_language_reaches_every_prompt() {
        let to = Target::parse("fr").unwrap();
        let prompt = Prompt {
            to,
            from: Some(Target::parse("en").unwrap()),
            ..Default::default()
        };
        for built in [
            system_prompt(&prompt),
            user_prompt(&prompt, "Hello"),
            pairs_prompt(&prompt, 2),
        ] {
            assert!(built.contains("法语"), "{built}");
            assert!(!built.contains("简体中文"), "{built}");
        }
        // 语言代码只在声明句里出现一次（system 与批量提示词都带声明），
        // 单条 user 消息只用中文名，避免同一句话里三种写法互相矛盾。
        assert!(system_prompt(&prompt).contains("（fr）"));
        assert!(pairs_prompt(&prompt, 2).contains("（fr）"));
        assert!(user_prompt(&prompt, "Hello").contains("Translate the text to 法语"));
        // 默认值仍然是原来那套措辞。
        let plain = Prompt::default();
        assert!(system_prompt(&plain).contains("简体中文"));
        assert!(user_prompt(&plain, "Hello").contains("Translate the text to 简体中文"));
    }

    /// `--from auto` 的含义是"别假定源语言"，不是"源语言就是英语"。
    #[test]
    fn an_unset_source_language_is_not_claimed_as_english() {
        let prompt = Prompt::default();
        assert!(system_prompt(&prompt).contains("源语言不声明"));
        let stated = Prompt {
            from: Some(Target::parse("fr").unwrap()),
            ..Default::default()
        };
        assert!(!system_prompt(&stated).contains("源语言：英语（en）"));
        assert!(system_prompt(&stated).contains("源语言：法语（fr）"));
    }

    #[test]
    fn a_language_code_is_validated_but_not_limited_to_the_known_names() {
        assert_eq!(Target::parse("zh-Hant").unwrap().name, "繁体中文");
        // 表外的合法代码原样写进提示词：悄悄退回中文才是真错。
        assert_eq!(Target::parse("mi").unwrap().name, "mi");
        for bad in ["", "en?to=zh", &"a".repeat(21)] {
            assert!(Target::parse(bad).is_err(), "{bad} 不该被接受");
        }
    }

    #[test]
    fn a_template_renders_only_the_variables_it_advertises() {
        let template = Template::parse(
            "把下面这段翻成 {{target_lang_cn}}（{{count}} 条）：\n{{text}}\n语境：{{context}}\n上一段：{{prev_output}}",
        )
        .unwrap();
        let prompt = Prompt {
            to: Target::parse("de").unwrap(),
            from: Some(Target::parse("en").unwrap()),
            prev_output: Some("上一页的结尾"),
            ..Default::default()
        };
        let rendered = template.render(&prompt, "Ein text", 3);
        assert!(rendered.contains("翻成 德语"), "{rendered}");
        assert!(rendered.contains("（3 条）"), "{rendered}");
        assert!(rendered.contains("Ein text"));
        assert!(rendered.contains("上一段：上一页的结尾"));
        // 没有值的变量渲染成空串，而不是留下花括号。
        assert!(rendered.contains("语境：\n"), "{rendered}");
        assert!(!rendered.contains("{{"));
    }

    #[test]
    fn a_template_that_cannot_work_is_rejected_at_load() {
        let missing_text = Template::parse("只要 {{target_lang}}")
            .unwrap_err()
            .to_string();
        assert!(missing_text.contains("{{text}}"), "{missing_text}");
        let unknown = Template::parse("{{text}} {{style}}")
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("未知变量 {{style}}"), "{unknown}");
        assert!(Template::parse("{{text}} 没闭合 {{").is_err());
        assert!(Template::parse("{{text}} {{}}")
            .unwrap_err()
            .to_string()
            .contains("变量名"));
        assert!(Template::parse("   ").is_err());
    }
}
