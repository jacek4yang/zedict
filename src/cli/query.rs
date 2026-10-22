//! 查询输出与自动翻译：终端词卡、JSON 输出、逐句译文。

use super::*;

pub(super) fn validate_query(query: &str) -> Result<()> {
    let norm = crate::data::store::normalize(query);
    ensure!(
        !norm.is_empty()
            && norm.chars().count() <= crate::data::store::MAX_QUERY_CHARS
            && !norm.chars().any(char::is_control),
        "查询须为 1–256 个有效字符"
    );
    Ok(())
}

pub(super) struct QueryOutput<'a> {
    pub(super) store: &'a DictStore,
    pub(super) mode: SearchMode,
    pub(super) limit: usize,
    pub(super) json: bool,
    pub(super) settings: &'a Settings,
    pub(super) brief: bool,
    pub(super) width: usize,
    pub(super) theme: crate::render::Theme,
    pub(super) translate: bool,
    /// 自动翻译（而非显式 --en-zh）：失败和超时都静默，只显示英文原文
    pub(super) auto_translate: bool,
}

/// 自动翻译是"有网就补"的增益：断网时不能为了等连接把查词卡在 HTTP 超时上。
/// 显式 `--en-zh` 不受这个预算限制，失败原因也照常报告。
pub(super) const AUTO_TRANSLATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// 文本查询是否补英英翻译。`--en-zh` 强制开启，`--no-en-zh` 强制关闭；
/// 否则看配置，配置缺省时只在交互终端自动开启。`--json` 永远不带译文，
/// `--stdin` 批量默认不开——脚本输出不能随网络状况变形。
pub(super) fn should_translate(
    en_zh: bool,
    no_en_zh: bool,
    configured: Option<bool>,
    stdin: bool,
    json: bool,
    stdout_tty: bool,
) -> bool {
    if json || no_en_zh {
        return false;
    }
    if en_zh {
        return true;
    }
    configured.unwrap_or(stdout_tty && !stdin)
}

pub(super) async fn emit_query(query: String, ctx: &QueryOutput<'_>, code: &mut u8) -> Result<()> {
    validate_query(&query)?;
    let response = ctx.store.query(&QueryRequest {
        query,
        mode: ctx.mode,
        limit: ctx.limit,
    })?;
    if !response.found() {
        *code = 3;
    }
    if ctx.json {
        output(&(serde_json::to_string(&response)? + "\n"))?;
        return Ok(());
    }

    let hints = if ctx.brief {
        HashMap::new()
    } else {
        let mut map: HashMap<String, Vec<AffixHint>> = HashMap::new();
        for hit in &response.hits {
            let list = ctx.store.affix_hints(&hit.key, 1)?;
            if !list.is_empty() {
                map.insert(hit.key.clone(), list);
            }
        }
        map
    };
    let translations = if ctx.translate {
        let sentences = gloss_sentences(&response);
        if sentences.is_empty() {
            HashMap::new()
        } else {
            let attempt = translate_gloss_sentences(&sentences, ctx.settings);
            let outcome = if ctx.auto_translate {
                match tokio::time::timeout(AUTO_TRANSLATE_TIMEOUT, attempt).await {
                    Ok(outcome) => outcome,
                    Err(_) => Err(anyhow::anyhow!("联网翻译超过等待时限")),
                }
            } else {
                attempt.await
            };
            match outcome {
                Ok(translations) => translations,
                Err(err) => {
                    // 自动模式断网是常态：安静退回英文原文，不让每次查词都刷错误。
                    if !ctx.auto_translate {
                        let _ = writeln!(
                            io::stderr().lock(),
                            "英英翻译失败，以下仍显示英文原文: {err:#}"
                        );
                    }
                    HashMap::new()
                }
            }
        }
    } else {
        HashMap::new()
    };
    let rendered = crate::render::response_themed_with_hints_and_translations(
        &response,
        &hints,
        &translations,
        ctx.brief,
        ctx.width,
        ctx.theme,
    );
    output(&rendered)?;
    // 例句反查：命中这个词在**别的**卡的例句里出现过，就把那几句端上来。
    // 它是 --full 的增益而不是用户请求的功能，所以缺索引时静默跳过：
    // 旧词库的查询输出必须和今天一模一样，更不能因为多个侧表而报错。
    if !ctx.brief && response.found() && ctx.store.has_examples() {
        let own: Vec<String> = response.hits.iter().map(|hit| hit.key.clone()).collect();
        let needle = crate::data::store::normalize(&response.query);
        let rows: Vec<_> = ctx
            .store
            .examples_scan(&needle, ctx.limit + own.len())?
            .into_iter()
            .filter(|hit| hit.headwords.iter().any(|key| !own.contains(key)))
            .take(ctx.limit)
            .collect();
        if !rows.is_empty() {
            if !rendered.ends_with("\n\n") {
                output("\n")?;
            }
            output(&crate::render::section_title(
                &format!("其他词条中的例句（含 {needle}）"),
                ctx.theme,
            ))?;
            output(&crate::render::examples_themed(
                &rows, true, ctx.width, ctx.theme,
            ))?;
        }
    }
    // 未命中不等于用户写错了词：库里没有这个词条，可能只是因为源语料里
    // 那条记录的词头没恢复到。参考词表在的时候这句话能把两种情况分开。
    // 判据是"没有词卡命中"而不是"整个响应是空的"——一个真的常见词即便
    // 只带着一串模糊建议回来，用户也该听见"这个词库没有它"。它追加在
    // 渲染之后，只走文本，不动 --json 的对外形状。
    if response.hits.is_empty()
        && ctx.store.has_vocab()
        && ctx
            .store
            .vocab_contains(&crate::data::store::normalize(&response.query))?
    {
        output(&format!(
            "“{}”是很常见的英文词，但本地词库暂未收录。\n",
            response.query
        ))?;
    }
    Ok(())
}

pub(super) fn gloss_sentences(response: &QueryResponse) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for hit in &response.hits {
        for card in &hit.cards {
            if let Some(en) = &card.gloss_en {
                for sentence in crate::render::translatable_gloss_sentences(en) {
                    if !out.contains(&sentence) {
                        out.push(sentence);
                    }
                }
            }
        }
    }
    out
}

/// 英英释义没有随库附带中文，只能借用翻译通路补上：
/// 配了 LLM 密钥就先走大模型，失败或没配再走免密钥 Edge——只要有一条通路上网可用，
/// 译文就不该缺席。
pub(super) async fn translate_gloss_sentences(
    sentences: &[String],
    settings: &Settings,
) -> Result<HashMap<String, String>> {
    if sentences.is_empty() {
        return Ok(HashMap::new());
    }
    let llm_error = match llm_config(None, None, None, settings) {
        Some(cfg) => match translate_gloss_with_llm(&cfg, sentences).await {
            Ok(translated) => return pair_translations(sentences, translated),
            Err(err) => Some(err),
        },
        None => None,
    };
    match translate_gloss_with_edge(sentences).await {
        Ok(translated) => pair_translations(sentences, translated),
        Err(edge_error) => Err(match llm_error {
            Some(llm_error) => {
                anyhow::anyhow!("大模型翻译失败: {llm_error:#}；改用 Edge 也失败: {edge_error:#}")
            }
            None => edge_error,
        }),
    }
}

pub(super) async fn translate_gloss_with_llm(
    cfg: &crate::net::llm::LlmConfig,
    sentences: &[String],
) -> Result<Vec<String>> {
    let prompt = crate::net::llm::Prompt {
        mode: crate::net::llm::TranslateMode::Natural,
        from: Some(crate::net::llm::Target::parse("en")?),
        to: crate::net::llm::Target::chinese(),
        context: Some(
            "以下条目摘自英文词典，包含释义与例句；释义用简洁准确的中文，例句译得自然通顺。",
        ),
        ..Default::default()
    };
    let mut out = Vec::with_capacity(sentences.len());
    for batch in crate::net::align::plan_batches(
        sentences,
        usize::MAX / 4,
        crate::net::align::MAX_BATCH_ITEMS,
    ) {
        let inputs: Vec<String> = batch
            .iter()
            .map(|&index| sentences[index].clone())
            .collect();
        let items = crate::net::llm::translate_block(cfg, &prompt, &inputs).await?;
        ensure!(
            items.len() == inputs.len(),
            "模型返回 {} 条译文，请求 {} 条",
            items.len(),
            inputs.len()
        );
        out.extend(items.into_iter().map(|item| item.trim().to_string()));
    }
    Ok(out)
}

pub(super) async fn translate_gloss_with_edge(sentences: &[String]) -> Result<Vec<String>> {
    let client = crate::net::edge::EdgeClient::new(Some("en".into()), "zh-Hans")?;
    let requests: Vec<String> = sentences.iter().map(|line| edge_fragment(line)).collect();
    crate::net::edge::translate_items(&client, &requests, true).await
}

pub(super) fn pair_translations(
    sentences: &[String],
    translated: Vec<String>,
) -> Result<HashMap<String, String>> {
    ensure!(translated.len() == sentences.len(), "译文条数与请求不符");
    Ok(sentences.iter().cloned().zip(translated).collect())
}

/// Edge 逐条独立翻译，没有句末标点的词典碎片常被截短
/// （`have sexual intercourse with` → `与`）。补句点只改送翻译的文本，
/// 译文仍按原句归档，词卡显示的字句不变。
pub(super) fn edge_fragment(sentence: &str) -> String {
    let sentence = sentence.trim_end();
    match sentence.chars().last() {
        Some(c) if c.is_alphanumeric() || matches!(c, ')' | ']' | '"' | '\'') => {
            format!("{sentence}.")
        }
        _ => sentence.to_string(),
    }
}
