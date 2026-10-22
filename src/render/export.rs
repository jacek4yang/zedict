//! `zedict export`：把本地词库导出成 Anki 能直接导入的 CSV / TSV 文本。
//!
//! 输出是 Anki 文本导入格式的"指令头 + 数据行"（`#separator`、`#html:false`、
//! `#columns`），所以导入时不用再手工选分隔符。带学习状态的 `.apkg` 由
//! `zedict study export`（`src/render/anki.rs`）负责，这里只做无状态快照。
//!
//! CSV 的字段可以带换行（RFC4180 的引号包裹，Anki 认），Back 里每条义项一行；
//! TSV 做不到——分隔符就是制表符，所以那边把字段里的制表符和换行压成空格。
//! 两种格式给的信息量相同，只是断行方式不同。

use crate::core::model::WordCard;
use crate::data::store::{DictStore, WordLevel};
use anyhow::Result;
use std::collections::HashMap;

/// 导出的列；`#columns` 与数据行都按这个顺序。
pub const COLUMNS: [&str; 3] = ["Front", "Back", "Tags"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Csv,
    Tsv,
}

impl Format {
    /// 与 `--format` 取值一致的写法，给人看的行用它。
    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
        }
    }

    fn separator(self) -> char {
        match self {
            Format::Csv => ',',
            Format::Tsv => '\t',
        }
    }

    /// Anki 的 `#separator:` 指令认的是名字，不是字符本身。
    fn directive(self) -> &'static str {
        match self {
            Format::Csv => "comma",
            Format::Tsv => "tab",
        }
    }
}

/// 一行卡片：正面词头、背面释义、标签。
#[derive(Clone, Debug)]
pub struct Row {
    pub front: String,
    pub back: String,
    pub tags: Vec<String>,
}

/// 渲染成完整文本：指令头三行，然后每行一张卡。
pub fn render(rows: &[Row], format: Format) -> String {
    let separator = format.separator().to_string();
    let mut out = format!(
        "#separator:{}\n#html:false\n#columns:{}\n",
        format.directive(),
        COLUMNS.join(separator.as_str())
    );
    for row in rows {
        let joined = row.tags.join(" ");
        let fields = [row.front.as_str(), row.back.as_str(), joined.as_str()];
        out.push_str(
            &fields
                .iter()
                .map(|field| escape(field, &separator, format))
                .collect::<Vec<_>>()
                .join(separator.as_str()),
        );
        out.push('\n');
    }
    out
}

/// CSV 按 RFC4180 用双引号包裹并成对转义引号；TSV 没有转义机制可依赖，
/// 所以把分隔符和换行直接压成空格，宁可丢排版也不能丢字段边界。
fn escape(field: &str, separator: &str, format: Format) -> String {
    match format {
        Format::Tsv => field
            .chars()
            .map(|c| {
                if c == '\t' || c == '\n' || c == '\r' {
                    ' '
                } else {
                    c
                }
            })
            .collect(),
        Format::Csv => {
            if field.contains(separator) || field.contains('"') || field.contains('\n') {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field.to_string()
            }
        }
    }
}

/// 按 `keys` 的顺序取词卡并组装行。`store.has_levels()` 时顺带把难度档写进标签，
/// 没有词级表的旧词库照样能导出，只是少一个标签。
pub fn rows(store: &DictStore, keys: &[String]) -> Result<Vec<Row>> {
    let levels: HashMap<String, WordLevel> = if store.has_levels() {
        store.levels(keys)?
    } else {
        HashMap::new()
    };
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        // 取不到卡片就跳过：导出的每一行都必须是能翻面的卡，宁可少一行。
        let Some(hit) = store.lookup(key)? else {
            continue;
        };
        let Some(card) = hit.cards.first() else {
            continue;
        };
        out.push(Row {
            front: card.headword.clone(),
            back: back(card),
            tags: tags(card, levels.get(key)),
        });
    }
    Ok(out)
}

/// 背面：音标一行，然后最多五条义项。例句和联想词留给 `zedict query --full`，
/// 一张卡塞满反而背不动。
pub(crate) fn back(card: &WordCard) -> String {
    let mut lines = Vec::new();
    let phonetics = [
        card.phonetic.uk.as_ref().map(|p| format!("UK {p}")),
        card.phonetic.us.as_ref().map(|p| format!("US {p}")),
        card.phonetic.en.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ");
    if !phonetics.is_empty() {
        lines.push(phonetics);
    }
    if let Some(pinyin) = &card.pinyin {
        lines.push(format!("拼音: {pinyin}"));
    }
    for (index, sense) in card.senses.iter().take(5).enumerate() {
        let pos = sense.pos.map(|p| p.as_str()).unwrap_or("");
        lines.push(format!("{}. {} {}", index + 1, pos, sense.gloss));
    }
    lines.join("\n")
}

/// 标签：`zedict` 标记来源，`band-N` 是词级表的带号，大纲归属取前两个（Collins
/// 五档之类全塞进去会把标签栏挤满），最后是词性。
fn tags(card: &WordCard, level: Option<&WordLevel>) -> Vec<String> {
    let mut tags = vec!["zedict".to_string()];
    if let Some(level) = level.and_then(|found| found.level.filter(|level| *level > 0)) {
        tags.push(format!("band-{level}"));
    }
    for tag in level.iter().flat_map(|found| found.tags.iter()).take(2) {
        tags.push(format!("list-{tag}"));
    }
    for pos in &card.pos {
        // 词性自带句点（`n.`），标签里不要：Anki 会把它读成标记的一部分。
        tags.push(format!("pos-{}", pos.as_str().trim_end_matches('.')));
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            front: "take into account".into(),
            back: "1. 考虑\n2. 把…计入".into(),
            tags: vec!["zedict".into(), "band-4".into()],
        }
    }

    #[test]
    fn csv_quotes_multiline_fields_and_doubles_inner_quotes() {
        let text = render(
            &[Row {
                back: "1. \"考虑\"\n2. 计入".into(),
                ..row()
            }],
            Format::Csv,
        );
        assert!(text.starts_with("#separator:comma\n#html:false\n#columns:Front,Back,Tags\n"));
        assert!(
            text.contains("\"1. \"\"考虑\"\"\n2. 计入\""),
            "引号要成对转义、换行要留在引号里：{text}"
        );
    }

    #[test]
    fn tsv_collapses_field_breaks_it_cannot_represent() {
        let text = render(&[row()], Format::Tsv);
        assert!(text.starts_with("#separator:tab\n#html:false\n#columns:Front\tBack\tTags\n"));
        // 数据区只有 3 行：指令头 3 行 + 一行卡片，Back 里的换行已被压平。
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(text.contains("1. 考虑 2. 把…计入"));
    }

    #[test]
    fn plain_fields_need_no_quotes() {
        let text = render(
            &[Row {
                front: "apple".into(),
                back: "1. n 苹果".into(),
                tags: vec!["zedict".into()],
            }],
            Format::Csv,
        );
        assert!(text.ends_with("apple,1. n 苹果,zedict\n"), "{text}");
    }
}
