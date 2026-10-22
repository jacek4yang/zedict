//! `zedict explain` — 大模型讲解，但只准基于本地词库的释义。
//!
//! 变量集对应欧陆 exe 的 AI 讲解模板 (`src_lang` / `target_lang` / `text` / `context`
//! / `style` / `reason`)；差别是 grounding 来自本地词库，而不是 `api.frdic.com`
//! （该端点需要账号，探测返回 401）。词库里没有的义项，模型必须回答"未收录"。

use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

use crate::core::model::WordCard;
use crate::data::store::{DictStore, QueryRequest, SearchMode};
use crate::net::llm::{chat_stream, LlmConfig};

/// 与翻译同一条 SSE 通路，但讲解要更稳的措辞
pub const TEMPERATURE: f32 = 0.3;
const CACHE_NS: &str = "explain";
/// 注入提示词的本地资料上限（字符）
pub const MAX_GROUNDING_CHARS: usize = 3000;
/// `--in-context` 句子长度上限
pub const MAX_CONTEXT_CHARS: usize = 2000;
const MAX_HITS: usize = 3;
const MAX_CARDS: usize = 3;
const MAX_SENSES: usize = 8;
const MAX_EXAMPLES: usize = 3;
const MAX_RELATIONS: usize = 8;

/// 讲解侧重
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// 逐义项解释
    Definition,
    /// 结合原句判定用的是哪个义项
    Context,
    /// 侧重搭配与例句
    Examples,
}

impl Style {
    fn task(self) -> &'static str {
        match self {
            Self::Definition => "逐条讲解 [本地词库] 列出的义项，每条一句简短说明",
            Self::Context => {
                "结合 [context] 的句子，指出这里用的是 [本地词库] 中的哪一条义项、为什么"
            }
            Self::Examples => "说明该词的常见搭配与用法，例句只能逐字引用 [本地词库] 里的例句",
        }
    }
}

/// 学习者水平档位。**词库没有逐词 CEFR 标注**，这只影响讲解用词的难易。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Cefr {
    A1,
    A2,
    B1,
    B2,
    C1,
    C2,
}

impl Cefr {
    fn code(self) -> &'static str {
        match self {
            Self::A1 => "A1",
            Self::A2 => "A2",
            Self::B1 => "B1",
            Self::B2 => "B2",
            Self::C1 => "C1",
            Self::C2 => "C2",
        }
    }
}

/// 本地词库能提供的讲解依据
#[derive(Debug, Clone)]
pub struct Grounding {
    /// 命中词卡的词头；有独立词卡时是原词，只有词形归并时才落到原形
    pub headword: String,
    /// exact / lemma / reverse
    pub match_type: String,
    /// 注入的义项条数
    pub senses: usize,
    /// 提示词里的资料块
    pub block: String,
}

/// 用 Auto 路由（精确 → 词形归并 → 释义反查）取本地依据；没命中就是 None。
pub fn ground(store: &DictStore, word: &str) -> Result<Option<Grounding>> {
    let response = store.query(&QueryRequest {
        query: word.to_string(),
        mode: SearchMode::Auto,
        limit: MAX_HITS,
    })?;
    let Some(first) = response.hits.first() else {
        return Ok(None);
    };
    let mut block = String::new();
    let mut senses = 0usize;
    let mut cards = 0usize;
    for hit in &response.hits {
        for card in &hit.cards {
            if cards >= MAX_CARDS {
                break;
            }
            append_card(card, &mut block, &mut senses);
            cards += 1;
        }
    }
    if block.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(Grounding {
        headword: first.key.clone(),
        match_type: first.match_type.clone(),
        senses,
        block: crate::net::http::truncate(&block, MAX_GROUNDING_CHARS),
    }))
}

fn append_card(card: &WordCard, out: &mut String, senses: &mut usize) {
    let mut head = format!("【{}】", card.headword);
    if let Some(lemma) = &card.lemma {
        head.push_str(&format!("（原形 {lemma}）"));
    }
    for phonetic in [
        card.phonetic.uk.as_deref(),
        card.phonetic.us.as_deref(),
        card.phonetic.en.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        head.push_str(&format!(" {phonetic}"));
    }
    if let Some(pinyin) = &card.pinyin {
        head.push_str(&format!(" 拼音 {pinyin}"));
    }
    if let Some(syllables) = &card.phonetic.syllables {
        head.push_str(&format!(" 音节 {}", syllables.join("·")));
    }
    out.push_str(&head);
    out.push('\n');
    for sense in card.senses.iter().take(MAX_SENSES) {
        let pos = sense.pos.map(|p| p.as_str()).unwrap_or("");
        let labels = sense
            .labels
            .iter()
            .map(|l| format!("〈{}〉", l.zh))
            .collect::<Vec<_>>()
            .join("");
        out.push_str(&format!("义项: {pos} {}{labels}\n", sense.gloss));
        *senses += 1;
    }
    if let Some(en) = &card.gloss_en {
        out.push_str(&format!("英英: {}\n", crate::net::http::truncate(en, 300)));
    }
    for form in card.forms.iter().take(6) {
        out.push_str(&format!(
            "词形: {} {}\n",
            form.kind.as_str(),
            form.words.join(" ")
        ));
    }
    for (label, list) in [
        ("近义", &card.synonyms),
        ("反义", &card.antonyms),
        ("相关", &card.related),
        ("短语", &card.phrases),
    ] {
        if list.is_empty() {
            continue;
        }
        let items = list
            .iter()
            .take(MAX_RELATIONS)
            .map(|r| match &r.gloss {
                Some(gloss) => format!("{}（{gloss}）", r.word),
                None => r.word.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("{label}: {items}\n"));
    }
    for example in card.examples.iter().take(MAX_EXAMPLES) {
        out.push_str(&format!("例句: {} | {}\n", example.en, example.zh));
    }
}

/// 硬性反编造约束：只认 [本地词库]，列表外的义项一律回答未收录。
/// 输出语言是这段里唯一由 `--to` 决定的一句。
fn system_prompt(to: &crate::net::llm::Target) -> String {
    format!(
        "你是离线词典的讲解助手。\
         只允许依据用户消息里 [本地词库] 列出的义项、词形、近反义词与例句进行讲解；\
         不得引入 [本地词库] 之外的义项，不得编造例句或搭配。\
         如果用户想知道的含义不在 [本地词库] 的义项列表里，必须直接回答“本地词库未收录该义项”。\
         如果 [本地词库] 标注为未收录该词，只能说明这一点；若要给出一般性提示，必须先写明“以下内容不在本地词库中”。\
         用{}输出，400 字以内，不要整段抄回义项列表。",
        to.name
    )
}

/// 讲解请求
pub struct ExplainJob<'a> {
    pub word: &'a str,
    pub style: Style,
    pub cefr: Option<Cefr>,
    pub context: Option<&'a str>,
    pub grounding: Option<Grounding>,
    /// 讲解用哪种语言写。词头恒为英文（`entries.key` 就是英文词头），所以
    /// `[src_lang]` 不需要跟着参数走。
    pub to: crate::net::llm::Target,
}

/// 按欧陆模板的变量分块；空变量不输出，免得模型把空值当成内容
fn user_prompt(job: &ExplainJob<'_>) -> String {
    let mut p = format!("[src_lang] en\n[target_lang] {}\n", job.to.code);
    p.push_str(&format!("[text] {}\n", job.word));
    p.push_str(&format!("[style] {}\n", job.style.task()));
    if let Some(context) = job.context {
        p.push_str(&format!("[context] {context}\n"));
    }
    if let Some(level) = job.cefr {
        p.push_str(&format!(
            "[reason] 学习者水平 CEFR {}，用该水平能懂的用词讲解\n",
            level.code()
        ));
    }
    match &job.grounding {
        Some(grounding) => p.push_str(&format!(
            "\n[本地词库] {}（{}）\n{}",
            grounding.headword, grounding.match_type, grounding.block
        )),
        None => p.push_str("\n[本地词库] （未收录该词）\n"),
    }
    p
}

pub fn validate_context(context: Option<&str>) -> Result<()> {
    let Some(context) = context else {
        return Ok(());
    };
    anyhow::ensure!(
        !context.trim().is_empty(),
        "--in-context 不能是空白；不需要语境就去掉这个参数"
    );
    anyhow::ensure!(
        context.chars().count() <= MAX_CONTEXT_CHARS,
        "--in-context 最多 {MAX_CONTEXT_CHARS} 个字符（当前 {} 个）",
        context.chars().count()
    );
    anyhow::ensure!(
        !context
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        "--in-context 含无效控制字符"
    );
    Ok(())
}

/// 缓存文件名。键里没有 api_key；换词、换风格、换语境、换输出语言、换命中词头或换
/// 模型都会重算——少了语言这一项，`--to fr` 会直接读到缓存里的中文讲解。
fn cache_path(job: &ExplainJob<'_>, model: &str) -> Result<PathBuf> {
    let key = crate::app::cache::key(
        CACHE_NS,
        &(
            job.word,
            &job.style,
            job.cefr.map(Cefr::code),
            job.context,
            job.to.code.as_str(),
            job.grounding
                .as_ref()
                .map(|g| (g.headword.as_str(), g.match_type.as_str())),
            model,
        ),
    )?;
    crate::app::cache::path(CACHE_NS, &key, "json")
}

#[derive(Debug, Clone)]
pub struct Explanation {
    pub text: String,
    /// 命中本地缓存 = 这次没有发请求
    pub cached: bool,
}

/// 讲解一条本地依据；`use_cache` 为 false 时跳过读取，但仍会写回缓存。
pub async fn explain(
    cfg: &LlmConfig,
    job: &ExplainJob<'_>,
    use_cache: bool,
    mut on_delta: impl FnMut(&str),
) -> Result<Explanation> {
    let file = cache_path(job, &cfg.model)?;
    if use_cache {
        if let Some(value) = crate::app::cache::read_json::<serde_json::Value>(&file)? {
            if let Some(text) = value.get("explanation").and_then(|v| v.as_str()) {
                let text = text.to_string();
                on_delta(&text);
                return Ok(Explanation { text, cached: true });
            }
        }
    }
    let text = chat_stream(
        cfg,
        &system_prompt(&job.to),
        &user_prompt(job),
        TEMPERATURE,
        &mut on_delta,
    )
    .await?;
    anyhow::ensure!(!text.trim().is_empty(), "模型没有返回讲解内容");
    crate::app::cache::write_json(&file, &serde_json::json!({"explanation": text}))?;
    Ok(Explanation {
        text,
        cached: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job<'a>(word: &'a str, grounding: Option<Grounding>) -> ExplainJob<'a> {
        ExplainJob {
            word,
            style: Style::Definition,
            cefr: None,
            context: None,
            grounding,
            to: crate::net::llm::Target::default(),
        }
    }

    #[test]
    fn prompt_blocks_follow_the_eudic_variable_set() {
        let grounding = Grounding {
            headword: "run".into(),
            match_type: "lemma".into(),
            senses: 2,
            block: "【run】\n义项: v. 跑\n".into(),
        };
        let prompt = user_prompt(&ExplainJob {
            word: "running",
            style: Style::Context,
            cefr: Some(Cefr::B2),
            context: Some("He is running a shop."),
            grounding: Some(grounding),
            ..job("running", None)
        });
        assert!(prompt.starts_with("[src_lang] en\n[target_lang] zh-Hans\n"));
        assert!(prompt.contains("[text] running"));
        assert!(prompt.contains("[context] He is running a shop."));
        assert!(prompt.contains("CEFR B2"));
        assert!(prompt.contains("[本地词库] run（lemma）"));
        assert!(prompt.contains("义项: v. 跑"));
    }

    /// `--to` 要同时改到 system 和用户消息里的 `[target_lang]`，只改一处就等于
    /// 继续用中文给用户讲一遍。
    #[test]
    fn the_explanation_language_follows_the_flag() {
        let french = ExplainJob {
            to: crate::net::llm::Target::parse("fr").unwrap(),
            ..job("robust", None)
        };
        assert!(user_prompt(&french).starts_with("[src_lang] en\n[target_lang] fr\n"));
        let system = system_prompt(&french.to);
        assert!(system.contains("用法语输出"), "{system}");
        assert!(!system.contains("用简体中文输出"));
        assert!(
            system.contains("本地词库未收录该义项"),
            "反编造约束不能跟着语言换掉"
        );
        // 缓存键里有输出语言：换 `--to` 不会读到上一条的缓存。
        assert_ne!(
            cache_path(&french, "m").unwrap(),
            cache_path(&job("robust", None), "m").unwrap()
        );
    }

    #[test]
    fn empty_variables_and_missing_grounding_are_stated_not_guessed() {
        let prompt = user_prompt(&job("robust", None));
        // 没给的语境/等级不能出现空标签，否则模型会把空值当内容
        assert!(!prompt.contains("[context]"));
        assert!(!prompt.contains("[reason]"));
        assert!(prompt.contains("[本地词库] （未收录该词）"));
        assert!(system_prompt(&crate::net::llm::Target::default()).contains("本地词库未收录该义项"));
    }

    #[test]
    fn context_validation_rejects_blank_and_overlong() {
        assert!(validate_context(None).is_ok());
        assert!(validate_context(Some("The economy remains robust.")).is_ok());
        assert!(validate_context(Some("   ")).is_err());
        assert!(validate_context(Some(&"字".repeat(MAX_CONTEXT_CHARS + 1))).is_err());
        assert!(validate_context(Some("a\u{0}b")).is_err());
    }

    #[test]
    fn cache_key_ignores_the_api_key_but_not_the_request_shape() {
        let first = cache_path(&job("run", None), "m1").unwrap();
        assert_eq!(first, cache_path(&job("run", None), "m1").unwrap());
        // 换模型、换词、加语境都算新请求
        assert_ne!(first, cache_path(&job("run", None), "m2").unwrap());
        assert_ne!(first, cache_path(&job("ran", None), "m1").unwrap());
        let with_context = ExplainJob {
            context: Some("He is running."),
            ..job("run", None)
        };
        assert_ne!(first, cache_path(&with_context, "m1").unwrap());
        let name = first.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("explain-"));
        // 键里只有哈希，绝不带入密钥或绝对路径
        assert!(!name.contains("m1"));
        assert!(!name.contains('\\') && !name.contains('/'));
    }
}
