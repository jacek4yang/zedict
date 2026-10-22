//! 中英逐句对齐的纯逻辑层：不联网，可完整离线测试。
//!
//! Edge 翻译端点返回 `sentLen.srcSentLen / transSentLen`——每个句子的长度，
//! 单位是 UTF-16 码元。**只有 srcSentLen 可信**：实测 zh→en 会把下一句的第一个
//! 字母算进上一句（`transSentLen` 切出来是 "…March. R"），en→zh 会返回零长句
//! (`[6, 8, 0]`)，两种情况累加都仍然等于译文长度，所以按长度切译文会静默错位。
//! 这里改成：原文按服务端句长切，译文按自己的句末标点切，两侧非空句数必须相等，
//! 对不上就整块降级，决不能臆造对应关系。

use serde::Serialize;

/// 一个句对；无法对齐时整块作为一对。
#[derive(Debug, Clone, Serialize)]
pub struct SentencePair {
    pub source: String,
    pub target: String,
}

/// 对齐方式：verified = 端点给出的句长数组自洽；fallback = 整块一对，未拆句。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Alignment {
    Verified,
    Fallback,
}

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub paragraph: usize,
    pub alignment: Alignment,
    pub sentences: Vec<SentencePair>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlignedDoc {
    pub engine: String,
    pub source_lang: String,
    /// 源语言由服务端识别时为 true
    pub detected: bool,
    pub target_lang: String,
    /// 只要有一块降级，整体标注为 fallback
    pub alignment: Alignment,
    pub blocks: Vec<Block>,
}

impl AlignedDoc {
    pub fn new(engine: &str, source_lang: &str, detected: bool, target_lang: &str) -> Self {
        Self {
            engine: engine.into(),
            source_lang: source_lang.into(),
            detected,
            target_lang: target_lang.into(),
            alignment: Alignment::Verified,
            blocks: Vec::new(),
        }
    }

    pub fn push(&mut self, block: Block) {
        if block.alignment == Alignment::Fallback {
            self.alignment = Alignment::Fallback;
        }
        self.blocks.push(block);
    }

    /// 只要译文
    pub fn plain(&self) -> String {
        self.blocks
            .iter()
            .map(|b| {
                b.sentences
                    .iter()
                    .map(|p| p.target.as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// 中英逐句对照文本，块间空行
    pub fn bilingual_text(&self, width: usize) -> String {
        let opts = textwrap::Options::new(width.max(20));
        let mut out = String::new();
        for block in &self.blocks {
            for pair in &block.sentences {
                if !pair.source.is_empty() {
                    out.push_str(&textwrap::fill(&pair.source, &opts));
                    out.push('\n');
                }
                out.push_str(&textwrap::fill(&pair.target, &opts));
                out.push_str("\n\n");
            }
        }
        while out.ends_with('\n') {
            out.pop();
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}

/// 单请求条数上限：批次越大越容易撞上端点/模型的隐式限制，20 条足够摊薄连接开销
pub const MAX_BATCH_ITEMS: usize = 20;

/// UTF-16 码元数（端点计数用的就是这个单位）
pub fn units(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// 按 UTF-16 码元数切一刀，绝不切断代理对。want 超过总长时返回整串。
pub fn take_units(s: &str, want: usize) -> (&str, &str) {
    if want == 0 {
        return ("", s);
    }
    let mut seen = 0usize;
    for (idx, ch) in s.char_indices() {
        seen += ch.len_utf16();
        if seen >= want {
            return s.split_at(idx + ch.len_utf8());
        }
    }
    (s, "")
}

/// 句末标点，中英都算
fn is_terminator(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?' | '。' | '！' | '？')
}

/// 按句末标点切片；每段连同其后的引号/空白一起归属本句，`concat() == 输入`。
/// 与 `llm::split_sentences` 的区别是这里必须处理中文标点，且不能丢字。
pub fn segments(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        if !is_terminator(chars[i]) {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len()
            && (is_terminator(chars[end]) || matches!(chars[end], '"' | '\'' | ')' | '」' | '）'))
        {
            end += 1;
        }
        // 只有英文句点有歧义：小数 3.5、缩写 U.S. 不是句末。中文的！？。一律算句末，
        // 否则 "π大约等于3.14159。用它吧。" 会因为前一个是数字而不切。
        let before = if i > 0 { chars[i - 1] } else { '\0' };
        let after = chars.get(end).copied().unwrap_or('\0');
        let abbreviation = chars[i] == '.'
            && ((before.is_ascii_digit() && after.is_ascii_digit())
                || (before.is_ascii_uppercase() && after.is_alphabetic()));
        if !abbreviation {
            while end < chars.len() && chars[end].is_whitespace() {
                end += 1;
            }
            out.push(chars[start..end].iter().collect());
            start = end;
        }
        i = end.max(i + 1);
    }
    if start < chars.len() {
        out.push(chars[start..].iter().collect());
    }
    out
}

/// 唯一的对齐通路：原文按服务端 `srcSentLen` 切（实测自洽），译文按自身句末标点切。
///
/// 不用 `transSentLen` 切译文：它的累加即使等于译文长度，也可能把下一句的第一个
/// 字母算进上一句（见模块头）。这里改用**两侧非空句数必须相等**这一可自证的约束；
/// 数量对不上返回 None，由调用方整块降级，绝不臆造句对。
pub fn pair_by_source_units(
    source: &str,
    target: &str,
    src_lens: &[usize],
) -> Option<Vec<SentencePair>> {
    if src_lens.is_empty() || src_lens.contains(&0) {
        return None;
    }
    if src_lens.iter().sum::<usize>() != units(source) {
        return None;
    }
    let mut rest = source;
    let mut sources: Vec<String> = Vec::with_capacity(src_lens.len());
    for (i, len) in src_lens.iter().enumerate() {
        let piece = if i + 1 == src_lens.len() {
            rest
        } else {
            let (head, tail) = take_units(rest, *len);
            rest = tail;
            head
        };
        let piece = piece.trim();
        if !piece.is_empty() {
            sources.push(piece.to_string());
        }
    }
    let targets: Vec<String> = segments(target)
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if sources.is_empty() || sources.len() != targets.len() {
        return None;
    }
    Some(
        sources
            .into_iter()
            .zip(targets)
            .map(|(source, target)| SentencePair { source, target })
            .collect(),
    )
}

/// 整块一对（对齐不可用时唯一的诚实做法）
pub fn fallback_block(paragraph: usize, source: &str, target: &str) -> Block {
    Block {
        paragraph,
        alignment: Alignment::Fallback,
        sentences: vec![SentencePair {
            source: source.trim().to_string(),
            target: target.trim().to_string(),
        }],
    }
}

/// 按空行分段；分隔符归属于前一段（文档开头的空行归第一段），保证 `concat() == 原文`。
pub fn paragraphs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut leading = String::new();
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            if current.is_empty() {
                match out.last_mut() {
                    Some(last) => last.push_str(line),
                    None => leading.push_str(line),
                }
            } else {
                current.push_str(line);
            }
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push_str(&std::mem::take(&mut leading));
        current.push_str(line);
    }
    if !current.is_empty() {
        out.push(current);
    } else if !leading.is_empty() {
        match out.last_mut() {
            Some(last) => last.push_str(&leading),
            None => out.push(leading),
        }
    }
    out
}

/// 把超长段落按句子继续拆；一句就超预算时按码元硬切。输出完整覆盖输入。
pub fn fit_blocks(text: &str, max_units: usize) -> Vec<String> {
    let mut out = Vec::new();
    if max_units == 0 {
        return out;
    }
    for para in paragraphs(text) {
        let mut rest = para.as_str();
        while !rest.is_empty() {
            if units(rest) <= max_units {
                out.push(rest.to_string());
                break;
            }
            let (head, _) = take_units(rest, max_units);
            let cut = sentence_break(head).filter(|c| *c > 0 && *c < head.len());
            let end = cut.unwrap_or(head.len());
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

/// 返回从尾部看到的最后一个句末标点之后的字节偏移（含标点）
fn sentence_break(s: &str) -> Option<usize> {
    let mut idx = s.len();
    while idx > 0 {
        let ch = s[..idx].chars().next_back()?;
        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？' | '\n') {
            return Some(idx);
        }
        idx -= ch.len_utf8();
    }
    None
}

/// 先按句拆到预算内，再贪心合并相邻小块（合并后仍不超预算）。
/// 用于 LLM 分段：既保住段落结构，又把请求数压到最少。`concat() == 输入`。
pub fn pack_units(text: &str, max_units: usize) -> Vec<String> {
    if max_units == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for piece in fit_blocks(text, max_units) {
        if current.is_empty() {
            current = piece;
            continue;
        }
        if units(&current) + units(&piece) <= max_units {
            current.push_str(&piece);
        } else {
            out.push(std::mem::take(&mut current));
            current = piece;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 按码元预算与条数预算把 blocks 分成若干请求批次（批次内保持原顺序）。
pub fn plan_batches(items: &[String], max_units: usize, max_items: usize) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0usize;
    for (i, item) in items.iter().enumerate() {
        let cost = units(item).max(1);
        let need_new =
            !current.is_empty() && (used + cost > max_units || current.len() >= max_items);
        if need_new {
            batches.push(std::mem::take(&mut current));
            used = 0;
        }
        used += cost;
        current.push(i);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_utf16_units_not_bytes_or_chars() {
        assert_eq!(units("I have a pen."), 13);
        assert_eq!(units("我有笔。"), 4);
        assert_eq!(units("😀"), 2, "增补平面字符占两个码元");
        assert_eq!(units("\u{65}\u{301}"), 2, "e + 组合音标");
        assert_eq!(units("\u{e9}"), 1, "预组合字符占一个码元");
    }

    #[test]
    fn never_splits_a_surrogate_pair() {
        let s = "😀ok";
        for want in [0, 1, 2, 3, 4, 99] {
            let (head, tail) = take_units(s, want);
            assert_eq!(
                head.chars().count() + tail.chars().count(),
                s.chars().count()
            );
            assert!(head.is_char_boundary(head.len()));
        }
        assert_eq!(
            take_units(s, 1),
            ("😀", "ok"),
            "预算不足一个完整字符时至少吃掉整字符"
        );
        assert_eq!(take_units(s, 3), ("😀o", "k"));
        assert_eq!(take_units(s, 99), (s, ""));
    }

    #[test]
    fn refuses_to_slice_the_target_by_server_lengths() {
        // 实测 zh→en：transSentLen 把下一句的首字母算进上一句 (42 = "…March. R")，
        // 累加却仍然等于译文长度。所以只按 srcSentLen + 译文自己的标点配对。
        let src = "我们的团队在三月完成了测试。下个月发布。\n";
        let dst = "Our team completed the testing in March. Released next month.\n";
        let pairs = pair_by_source_units(src, dst, &[14, 7]).expect("服务端原文句长自洽");
        assert_eq!(
            pairs
                .iter()
                .map(|p| (p.source.as_str(), p.target.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "我们的团队在三月完成了测试。",
                    "Our team completed the testing in March."
                ),
                ("下个月发布。", "Released next month."),
            ]
        );
        assert!(pair_by_source_units(src, dst, &[]).is_none(), "空数组");
        assert!(
            pair_by_source_units("One. Two.", "一。二。", &[4, 9]).is_none(),
            "累加与原文不符"
        );
        assert!(
            pair_by_source_units("One. Two.", "一。", &[5, 4]).is_none(),
            "译文句数对不上"
        );
    }

    #[test]
    fn segments_never_lose_a_character() {
        for text in [
            "我有笔。我还有一个苹果。\n\n",
            "Yes.\n",
            "U.S. policy changed. Really?",
            "版本 3.5 发布了。是的。\n",
            "no terminator at all",
            "",
        ] {
            assert_eq!(segments(text).concat(), text, "切分必须无损");
        }
        assert_eq!(segments("我有笔。我还有一个苹果。\n\n").len(), 2);
        assert_eq!(
            segments("版本 3.5 发布了。是的。").len(),
            2,
            "小数点不该切句"
        );
        assert_eq!(
            segments("π大约等于3.14159。用它吧。").len(),
            2,
            "数字后面的中文句号必须切句"
        );
        assert_eq!(segments("no terminator at all").len(), 1);
        assert!(segments("").is_empty());
    }

    #[test]
    fn source_array_can_align_when_target_lengths_are_broken() {
        let src = "I have a pen. I also have an apple.\n\n";
        let dst = "我有笔。我还有一个苹果。\n\n";
        let pairs = pair_by_source_units(src, dst, &[14, 22, 1]).expect("srcSentLen 自洽即可");
        assert_eq!(
            pairs
                .iter()
                .map(|p| (p.source.as_str(), p.target.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("I have a pen.", "我有笔。"),
                ("I also have an apple.", "我还有一个苹果。"),
            ],
            "换行不该自成一对"
        );
        assert!(
            pair_by_source_units(src, dst, &[14, 22]).is_none(),
            "累加不等于原文"
        );
        assert!(
            pair_by_source_units(src, "没有标点的译文", &[14, 22, 1]).is_none(),
            "句数对不上就降级"
        );
        assert!(
            pair_by_source_units(src, dst, &[14, 22, 0]).is_none(),
            "零长句"
        );
    }

    #[test]
    fn paragraphs_cover_the_input_with_no_loss() {
        for text in [
            "a b.\n\nc d.",
            "one\ntwo\n\n\nthree\n",
            "no blank lines here",
            "\n\nonly trailing\n\n",
            "",
        ] {
            let joined = paragraphs(text).concat();
            assert_eq!(joined, text, "段落切分必须完整覆盖");
        }
        assert_eq!(paragraphs("a b.\n\nc d.").len(), 2);
    }

    #[test]
    fn oversized_paragraphs_are_split_and_still_cover() {
        let text = "x".repeat(9_000);
        let blocks = fit_blocks(&text, 1_000);
        assert!(blocks.iter().all(|b| units(b) <= 1_000));
        assert_eq!(blocks.concat(), text);

        let prose = "This is a sentence. ".repeat(400);
        let blocks = fit_blocks(&prose, 500);
        assert_eq!(blocks.concat(), prose);
        assert!(
            blocks.iter().all(|b| units(b) <= 500),
            "按句收口: {:?}",
            blocks.iter().map(|b| units(b)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn batches_respect_both_budgets() {
        let items: Vec<String> = (0..10).map(|i| format!("s{i}.")).collect();
        let batches = plan_batches(&items, 20, 3);
        let flat: Vec<usize> = batches.iter().flatten().copied().collect();
        assert_eq!(flat, (0..10).collect::<Vec<_>>(), "顺序与覆盖不变");
        assert!(batches.iter().all(|b| b.len() <= 3));
        assert!(plan_batches(&[], 100, 10).is_empty());
    }

    #[test]
    fn bilingual_render_has_source_then_target() {
        let mut doc = AlignedDoc::new("edge", "en", false, "zh-Hans");
        doc.push(Block {
            paragraph: 0,
            alignment: Alignment::Verified,
            sentences: vec![
                SentencePair {
                    source: "I have a pen.".into(),
                    target: "我有笔。".into(),
                },
                SentencePair {
                    source: "Yes.".into(),
                    target: "是的。".into(),
                },
            ],
        });
        let text = doc.bilingual_text(60);
        assert_eq!(text, "I have a pen.\n我有笔。\n\nYes.\n是的。\n");
        assert_eq!(doc.plain(), "我有笔。是的。");
        assert!(serde_json::to_string(&doc)
            .unwrap()
            .contains("\"verified\""));
    }
}
