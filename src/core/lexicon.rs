//! 纯本地的"这段话里有哪些超纲词"分析：`zedict hint` 的实现，全程不联网。
//!
//! 词级数据来自 `tools/word_levels.py`：它从欧陆的 `level.bin` 里解出 10 个 COCA
//! 频次分带和 24 份大纲词表，**按词形逐条复制源表**，不是我们的推断。所以这里的
//! `level` 是源表的带号（1 = 最常见；源表每带标称 2000 个词族，实际是这些词族展开
//! 出的全部词形），`tags` 是大纲归属（`cet4`、`junior`、`collins1` …）。
//! 词形自己没登记带号时才退回原形，并在输出里注明 `via_lemma`。

use crate::data::store::{normalize, DictStore};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// 一个词元：文本里连续的一段拉丁字母，允许词内的 `-` 和 `'`（`well-known`、`don't`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Token {
    /// 原文里的样子，保留大小写；查表用的键由它 `normalize` 得到。
    pub text: String,
    /// 在原文里的字节区间 `[start, end)`，`--annotate` 用它把标记插回原句。
    pub start: usize,
    pub end: usize,
}

/// 拉丁字母才算词的一部分。中日韩文字虽然也 `is_alphabetic`，但不算：否则一段中文
/// 会被切成几十个"英文生词"，统计结果毫无意义。
fn is_letter(c: char) -> bool {
    c.is_alphabetic()
        && !matches!(
            c as u32,
            0x2e80..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x323af
        )
}

/// 词内连接符。只有后面还接字母时才算词的一部分，所以 `well-known` 是一个词，
/// 而结尾的 `self-`、`'87` 这类前置撇号都不会被吸进词里。
fn is_joiner(c: char) -> bool {
    matches!(c, '-' | '\'' | '\u{2019}')
}

pub fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_letter(chars[i].1) {
            i += 1;
            continue;
        }
        let start = chars[i].0;
        let mut word = chars[i].1.to_string();
        let mut end = start + chars[i].1.len_utf8();
        i += 1;
        while i < chars.len() {
            let c = chars[i].1;
            if is_letter(c) {
                word.push(c);
                end = chars[i].0 + c.len_utf8();
                i += 1;
            } else if is_joiner(c) && i + 1 < chars.len() && is_letter(chars[i + 1].1) {
                // 弯引号先换成直引号，和 store::normalize 建键的方式一致
                word.push(if c == '\u{2019}' { '\'' } else { c });
                end = chars[i].0 + c.len_utf8();
                i += 1;
            } else {
                break;
            }
        }
        out.push(Token {
            text: word,
            start,
            end,
        });
    }
    out
}

/// 一个统计单位：一个词，或者 `--include-phrase` 从相邻词元合并出来的短语。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    key: String,
    start: usize,
    end: usize,
    words: usize,
}

/// `phrases` 为 `None` 时逐词成单位；否则按**最长优先**贪心匹配 `entries.key` 里的
/// 多词条目，`take into account` 因此算一个熟词，而不是四个生词。
fn units(tokens: &[Token], phrases: Option<(&HashSet<String>, usize)>) -> Vec<Unit> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let matched = phrases.and_then(|(keys, max_words)| {
            (2..=max_words.min(tokens.len() - i))
                .rev()
                .map(|size| {
                    (
                        tokens[i..i + size]
                            .iter()
                            .map(|t| normalize(&t.text))
                            .collect::<Vec<_>>()
                            .join(" "),
                        size,
                    )
                })
                .find(|(joined, _)| keys.contains(joined))
        });
        let unit = match matched {
            Some((key, size)) => Unit {
                end: tokens[i + size - 1].end,
                start: tokens[i].start,
                key,
                words: size,
            },
            None => Unit {
                key: normalize(&tokens[i].text),
                start: tokens[i].start,
                end: tokens[i].end,
                words: 1,
            },
        };
        i += unit.words;
        out.push(unit);
    }
    out
}

/// 一个词为什么算"生词"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// 目标档之内（含），不进生词表
    Below,
    /// 源表有带号，且高于目标档
    Above,
    /// 源表只把它列进某份大纲词表，没有带频率的带号
    Tagged,
    /// 词级表没有，但本地词库有释义
    Unranked,
    /// 词级表和词库都查不到（专有名词、生僻词、拼错的词都落在这里）
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Below => "below",
            Status::Above => "above",
            Status::Tagged => "tagged",
            Status::Unranked => "unranked",
            Status::Unknown => "unknown",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::Below => "在纲",
            Status::Above => "超纲",
            Status::Tagged => "仅大纲",
            Status::Unranked => "词库有无档",
            Status::Unknown => "两处都无",
        }
    }
}

/// 生词表里的一行。
#[derive(Debug, Clone, Serialize)]
pub struct WordStat {
    /// 规范化后的词形或短语
    pub word: String,
    pub count: usize,
    /// 带号来自原形时给出原形；来自词形本身时为 `None`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_lemma: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    pub status: Status,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub gloss: String,
}

/// 每个状态有多少个**不同**的词形（不是出现次数）。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Counts {
    pub below: usize,
    pub above: usize,
    pub tagged: usize,
    pub unranked: usize,
    pub unknown: usize,
}

impl Counts {
    fn add(&mut self, status: Status) {
        match status {
            Status::Below => self.below += 1,
            Status::Above => self.above += 1,
            Status::Tagged => self.tagged += 1,
            Status::Unranked => self.unranked += 1,
            Status::Unknown => self.unknown += 1,
        }
    }

    fn reported(self) -> usize {
        self.above + self.tagged + self.unranked + self.unknown
    }
}

/// 一次分析的完整结果；`--json` 直接序列化它。
#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub threshold: u8,
    /// 目标档在源表里的自述（如 `COCA等级 6000 - 8000`）；老词库或档位越界时为 `None`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub band: Option<crate::data::ingest::LevelBand>,
    pub tokens: usize,
    pub units: usize,
    pub distinct: usize,
    pub phrase_units: usize,
    pub counts: Counts,
    pub items: Vec<WordStat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotated: Option<String>,
}

impl Analysis {
    /// `--only-unknown`：只留下词级表给不出档位的词（词库有释义但无档，以及两处都无）。
    pub fn keep_without_level(&mut self) {
        self.items
            .retain(|item| matches!(item.status, Status::Unranked | Status::Unknown));
    }
}

/// 词形拿到的档位与大纲归属，以及它是不是从原形借来的。
#[derive(Debug, Clone)]
struct Grade {
    level: Option<u8>,
    tags: Vec<String>,
    via_lemma: Option<String>,
}

/// 统计一段文本相对目标档的生词。
///
/// SQL 用量按**不同词形**算，不按出现次数算：档位一次 `IN` 问完，原形只对没档的词
/// 问，释义只对要上报的词问。一篇文章几千词元也只有一两百次查询。
pub fn analyze(
    store: &DictStore,
    text: &str,
    threshold: u8,
    phrases: bool,
    annotate: bool,
) -> Result<Analysis> {
    let tokens = tokenize(text);
    let phrase_keys: Option<HashSet<String>> = if phrases {
        let keys: HashSet<String> = store.phrase_keys()?.into_iter().collect();
        (!keys.is_empty()).then_some(keys)
    } else {
        None
    };
    let max_words = phrase_keys
        .as_ref()
        .and_then(|keys| keys.iter().map(|key| key.matches(' ').count() + 1).max())
        .unwrap_or(1);
    let list = units(&tokens, phrase_keys.as_ref().map(|keys| (keys, max_words)));

    // 保序去重：同一个词在生词表里只出现一次，出现次数累加到 count。
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    for unit in &list {
        if seen.insert(unit.key.clone()) {
            order.push(unit.key.clone());
        }
        *occurrences.entry(unit.key.clone()).or_default() += 1;
    }

    let form_levels = store.levels(&order)?;
    let unranked_forms: Vec<String> = order
        .iter()
        .filter(|key| !form_levels.contains_key(key.as_str()))
        .cloned()
        .collect();
    // 词形自己没有带号，才去问原形；原形再批量问一次带号。短语没有原形可问。
    let mut lemma_of: HashMap<String, String> = HashMap::new();
    for key in &unranked_forms {
        if key.contains(' ') {
            continue;
        }
        if let Some(lemma) = store.lemma_of(key)? {
            if lemma != *key {
                lemma_of.insert(key.clone(), lemma);
            }
        }
    }
    let lemma_keys: Vec<String> = lemma_of.values().cloned().collect();
    let lemma_levels = store.levels(&lemma_keys)?;

    let mut grades: HashMap<String, Grade> = HashMap::new();
    for key in &order {
        let grade = match form_levels.get(key) {
            Some(found) => Grade {
                level: found.level,
                tags: found.tags.clone(),
                via_lemma: None,
            },
            None => {
                let via = lemma_of.get(key).cloned();
                match via.as_ref().and_then(|lemma| lemma_levels.get(lemma)) {
                    Some(found) => Grade {
                        level: found.level,
                        tags: found.tags.clone(),
                        via_lemma: via,
                    },
                    None => Grade {
                        level: None,
                        tags: Vec::new(),
                        via_lemma: via,
                    },
                }
            }
        };
        grades.insert(key.clone(), grade);
    }

    let mut counts = Counts::default();
    let mut items: Vec<WordStat> = Vec::new();
    for key in &order {
        let grade = &grades[key];
        // 没档也没大纲归属：这时才知道该不该算"词库有但无档"，顺带取释义。
        let (status, gloss) = match (grade.level, grade.tags.is_empty()) {
            (Some(level), _) if level <= threshold => {
                counts.add(Status::Below);
                continue;
            }
            (Some(_), _) => {
                let status = Status::Above;
                counts.add(status);
                (
                    status,
                    lookup_gloss(store, key, grade.via_lemma.as_deref())?.0,
                )
            }
            (None, false) => {
                let status = Status::Tagged;
                counts.add(status);
                (
                    status,
                    lookup_gloss(store, key, grade.via_lemma.as_deref())?.0,
                )
            }
            (None, true) => {
                let (gloss, in_library) = lookup_gloss(store, key, grade.via_lemma.as_deref())?;
                let status = if in_library {
                    Status::Unranked
                } else {
                    Status::Unknown
                };
                counts.add(status);
                (status, gloss)
            }
        };
        items.push(WordStat {
            word: key.clone(),
            count: occurrences[key],
            via_lemma: grade.via_lemma.clone(),
            level: grade.level,
            tags: grade.tags.clone(),
            status,
            gloss,
        });
    }

    // 先按"为什么算生词"分组（超档最该看，两处都无的可能是拼错），组内难的排前面：
    // 带号大的先，其次出现次数多的先，最后按字母序，保证输出可复现。
    let rank = |status: Status| match status {
        Status::Above => 0,
        Status::Tagged => 1,
        Status::Unranked => 2,
        Status::Unknown => 3,
        Status::Below => 4,
    };
    items.sort_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then(b.level.cmp(&a.level))
            .then(b.count.cmp(&a.count))
            .then(a.word.cmp(&b.word))
    });

    let annotated = annotate.then(|| {
        let mut out = String::with_capacity(text.len() + list.len() * 4);
        let mut last = 0;
        for unit in &list {
            out.push_str(&text[last..unit.start]);
            out.push_str(&text[unit.start..unit.end]);
            out.push_str(&marker(&grades[&unit.key]));
            last = unit.end;
        }
        out.push_str(&text[last..]);
        out
    });

    Ok(Analysis {
        threshold,
        band: store
            .levels_meta()?
            .and_then(|meta| meta.bands.into_iter().find(|band| band.level == threshold)),
        tokens: tokens.len(),
        units: list.len(),
        distinct: order.len(),
        phrase_units: list.iter().filter(|unit| unit.words > 1).count(),
        counts,
        items,
        annotated,
    })
}

/// 标记用 `〖〗`（U+3016/3017）：它们在 GBK 里，而 `⟪⟫` 不在。Windows 控制台默认
/// 代码页是 GBK，选后者会让整行标注变成乱码。
fn marker(grade: &Grade) -> String {
    match (grade.level, grade.tags.first()) {
        (Some(level), _) => format!("〖{level}〗"),
        (None, Some(tag)) => format!("〖{tag}〗"),
        (None, None) => "〖?〗".into(),
    }
}

/// 中文释义与"词库到底有没有这个词"。查不到就留空，绝不编造。
fn lookup_gloss(store: &DictStore, word: &str, lemma: Option<&str>) -> Result<(String, bool)> {
    if let Some(hit) = store.lookup(word)? {
        return Ok((first_gloss(&hit), true));
    }
    // 词形查不到时再看原形：alias 表只覆盖我们收录的词形，两边都试一次
    // 才敢说"词库没有"。
    if let Some(lemma) = lemma {
        if let Some(hit) = store.lookup(lemma)? {
            return Ok((first_gloss(&hit), true));
        }
    }
    Ok((String::new(), false))
}

/// 第一条义项的中文，超长截断：生词表要能一眼扫完，详细释义交给 `zedict query`。
fn first_gloss(hit: &crate::data::store::SearchHit) -> String {
    hit.cards
        .first()
        .and_then(|card| card.senses.first())
        .map(|sense| {
            let pos = sense.pos.map(|p| p.as_str()).unwrap_or("");
            let text = if pos.is_empty() {
                sense.gloss.clone()
            } else {
                format!("{pos} {}", sense.gloss)
            };
            truncate(&text, 60)
        })
        .unwrap_or_default()
}

fn truncate(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    let mut out: String = chars.iter().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 首行统计 + 生词表；`render::hint` 用它，`--json` 不用。
pub fn summary_line(analysis: &Analysis) -> String {
    let band = analysis
        .band
        .as_ref()
        .map(|band| format!("（源表标称 {}）", band.desc))
        .unwrap_or_default();
    format!(
        "目标档 ≤{}{band}，1 = 最常见；{} 词元 / {} 个统计单位 / {} 个不同词（短语 {}）\n在纲 {}；生词 {}：超档 {}、仅大纲 {}、词库有无档 {}、两处都无 {}",
        analysis.threshold,
        analysis.tokens,
        analysis.units,
        analysis.distinct,
        analysis.phrase_units,
        analysis.counts.below,
        analysis.counts.reported(),
        analysis.counts.above,
        analysis.counts.tagged,
        analysis.counts.unranked,
        analysis.counts.unknown,
    )
}

/// 一行一个生词。
pub fn item_line(item: &WordStat) -> String {
    let level = match item.level {
        Some(level) => format!("档 {level}"),
        None => "无档".into(),
    };
    let via = item
        .via_lemma
        .as_ref()
        .map(|lemma| format!("←{lemma}"))
        .unwrap_or_default();
    let tags = if item.tags.is_empty() {
        String::new()
    } else {
        format!(" {}", item.tags.join(","))
    };
    format!(
        "{:<20} {:<7} ×{}{via}{tags}  [{}] {}\n",
        item.word,
        level,
        item.count,
        item.status.label(),
        item.gloss
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        tokenize(text).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn keeps_inner_hyphens_and_apostrophes_but_not_trailing_ones() {
        assert_eq!(words("well-known don't"), ["well-known", "don't"]);
        assert_eq!(words("self- made"), ["self", "made"]);
        assert_eq!(words("rock'n'roll"), ["rock'n'roll"]);
        assert_eq!(words("'87"), Vec::<String>::new());
    }

    #[test]
    fn accented_letters_stay_but_cjk_is_not_english() {
        assert_eq!(words("café abbé"), ["café", "abbé"]);
        assert_eq!(words("经济 economy 学家"), ["economy"]);
    }

    #[test]
    fn curly_apostrophe_becomes_the_key_shape() {
        // 源表的键是 don't（直引号），弯引号必须先换掉才可能命中。
        let tokens = tokenize("don\u{2019}t");
        assert_eq!(normalize(&tokens[0].text), "don't");
    }

    #[test]
    fn spans_point_at_the_original_text() {
        let text = "a robust idea";
        assert_eq!(
            tokenize(text)
                .iter()
                .map(|t| &text[t.start..t.end])
                .collect::<Vec<_>>(),
            ["a", "robust", "idea"]
        );
    }

    #[test]
    fn phrases_greedily_take_the_longest_key() {
        let tokens = tokenize("take into account it");
        let keys: HashSet<String> = ["take into account", "take into"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let built = units(&tokens, Some((&keys, 3)));
        assert_eq!(built.len(), 2);
        assert_eq!(built[0].key, "take into account");
        assert_eq!(built[0].words, 3);
        assert_eq!(built[1].key, "it");
    }

    #[test]
    fn without_phrases_the_same_text_splits_word_by_word() {
        let tokens = tokenize("take into account");
        assert_eq!(units(&tokens, None).len(), 3);
    }

    #[test]
    fn marker_prefers_the_band_then_the_syllabus() {
        let band = Grade {
            level: Some(7),
            tags: vec!["cet6".into()],
            via_lemma: None,
        };
        let tagged = Grade {
            level: None,
            tags: vec!["gre".into()],
            via_lemma: None,
        };
        let none = Grade {
            level: None,
            tags: vec![],
            via_lemma: None,
        };
        assert_eq!(marker(&band), "〖7〗");
        assert_eq!(marker(&tagged), "〖gre〗");
        assert_eq!(marker(&none), "〖?〗");
    }

    #[test]
    fn truncate_never_cuts_a_character() {
        assert_eq!(truncate("强健的", 2), "强…");
        assert_eq!(truncate("short", 60), "short");
    }
}
