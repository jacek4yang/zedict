//! CLI 共用支撑：输入读取、导出选词、LLM 配置与进度输出。

use super::*;

pub(super) const MAX_TEXT_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// `--text` / `--file` 的共用读取。`label` 只进错误消息（"翻译"、"统计"）。
/// `Ok(Ok(文本))` 可用输入；`Ok(Err(消息))` 参数错误 (exit 2)；`Err` 读写失败 (exit 1)
pub(super) fn text_input(
    label: &str,
    text: Option<String>,
    file: Option<&Path>,
) -> Result<std::result::Result<String, String>> {
    let input = if let Some(path) = file {
        let context = format!("读取{label}文件: {}", path.display());
        let meta = std::fs::metadata(path).with_context(|| context.clone())?;
        if meta.len() > MAX_TEXT_FILE_BYTES {
            return Ok(Err(format!(
                "{label}文件超过 4 MiB（{} 字节）；请拆分后再用 --file",
                meta.len()
            )));
        }
        let raw = std::fs::read_to_string(path).with_context(|| context)?;
        // Windows 文本文件常见 BOM + CRLF：不归一化会把 \r 带进逐句对照的每一行
        raw.trim_start_matches('\u{feff}')
            .replace("\r\n", "\n")
            .replace('\r', "\n")
    } else {
        text.unwrap_or_default()
    };
    if input.trim().is_empty() {
        return Ok(Err(format!("{label}文本为空")));
    }
    Ok(Ok(input))
}

pub(super) fn translation_input(
    args: &TranslateArgs,
) -> Result<std::result::Result<String, String>> {
    text_input("翻译", args.text.clone(), args.file.as_deref())
}

/// 导出要取哪些词头。`store.words` 一页最多 `MAX_LIMIT` 条，所以按页取到够数为止；
/// `--all` 就是取到空页为止。随机抽样一次问完，本来就有 1000 条的上限。
/// 生词本里存的是查询命中的键：有独立词卡就用它自己（全量词库里 running 仍是 running），
/// 只有词形归并时才落到原形（ran 归到 run）；未收录的词退回 normalize。
/// 这样列表、复习和导出都能用同一个键对齐词卡，又不强求词库必须收录。
pub(super) fn resolve_study_word(store: &DictStore, word: &str) -> Result<(String, String)> {
    let response = store.query(&QueryRequest {
        query: word.to_string(),
        mode: SearchMode::Auto,
        limit: 1,
    })?;
    if let Some(hit) = response.hits.into_iter().next() {
        let headword = hit
            .cards
            .first()
            .map(|card| card.headword.clone())
            .unwrap_or_else(|| hit.key.clone());
        Ok((hit.key, headword))
    } else {
        let trimmed = word.trim();
        Ok((crate::data::store::normalize(trimmed), trimmed.to_string()))
    }
}

pub(super) fn state_label(state: &str) -> &'static str {
    match state {
        "new" => "新词",
        "learning" => "学习中",
        _ => "复习",
    }
}

pub(super) fn export_keys(store: &DictStore, args: &ExportArgs) -> Result<Vec<String>> {
    if args.random {
        return store.random(args.rows);
    }
    let want = if args.all { usize::MAX } else { args.rows };
    let mut keys = Vec::new();
    while keys.len() < want {
        let page = store.words(args.offset + keys.len(), want - keys.len())?;
        if page.is_empty() {
            break;
        }
        keys.extend(page);
    }
    Ok(keys)
}

/// 是否配置了大模型（auto 引擎与 explain 据此决定要不要请求）
pub(super) fn llm_config(
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    settings: &Settings,
) -> Option<crate::net::llm::LlmConfig> {
    crate::net::llm::LlmConfig::explicit(
        base_url
            .clone()
            .or_else(|| std::env::var("LLM_BASE_URL").ok())
            .or_else(|| settings.config.llm_base_url.clone()),
        api_key,
        model
            .clone()
            .or_else(|| std::env::var("LLM_MODEL").ok())
            .or_else(|| settings.config.llm_model.clone()),
    )
    .ok()
}

pub(super) fn configured_llm(
    args: &TranslateArgs,
    settings: &Settings,
) -> Option<crate::net::llm::LlmConfig> {
    llm_config(
        args.base_url.clone(),
        args.api_key.clone(),
        args.model.clone(),
        settings,
    )
}

pub(super) fn progress_printer() -> impl FnMut(usize, usize) {
    let show = io::stderr().is_terminal();
    move |done: usize, total: usize| {
        if show {
            let _ = writeln!(io::stderr().lock(), "[{done}/{total}] 块已对齐");
        }
    }
}

pub(super) fn warn_edge_ignores(args: &TranslateArgs) {
    let mut ignored: Vec<&str> = Vec::new();
    for (used, flag) in [
        (args.glossary.is_some(), "--glossary"),
        (args.glossary_auto.is_some(), "--glossary-auto"),
        (args.style_file.is_some(), "--style-file"),
        (args.no_prev_output, "--no-prev-output"),
        (args.context.is_some(), "--context"),
        (args.summary.is_some(), "--summary"),
        (args.model.is_some(), "--model"),
        (args.base_url.is_some(), "--base-url"),
        (args.chunk != 1200, "--chunk"),
        (
            args.style != crate::net::llm::TranslateMode::Natural,
            "--translation-mode",
        ),
    ] {
        if used {
            ignored.push(flag);
        }
    }
    if !ignored.is_empty() {
        let _ = writeln!(
            io::stderr().lock(),
            "提示: --engine edge 不使用 {}；需要术语表、上下文或风格控制请用 --engine llm",
            ignored.join(" ")
        );
    }
}

/// 对齐结果输出：--json 给结构化句对，--bilingual 给逐句对照，--out 原子落盘
pub(super) fn emit_doc(
    args: &TranslateArgs,
    json: bool,
    width: usize,
    doc: &crate::net::align::AlignedDoc,
    requests: usize,
    cache_hits: usize,
) -> Result<u8> {
    let rendered = if args.bilingual {
        doc.bilingual_text(width)
    } else {
        format!("{}\n", doc.plain())
    };
    if json {
        let mut value = serde_json::to_value(doc)?;
        let object = value
            .as_object_mut()
            .context("内部错误：对齐结果不是 JSON 对象")?;
        object.insert("text".into(), serde_json::json!(doc.plain()));
        object.insert("requests".into(), serde_json::json!(requests));
        object.insert("cache_hits".into(), serde_json::json!(cache_hits));
        output(&(serde_json::to_string(&value)? + "\n"))?;
    } else {
        output(&rendered)?;
    }
    if let Some(path) = &args.out {
        crate::app::cache::write_bytes(path, rendered.as_bytes())?;
    }
    Ok(0)
}
