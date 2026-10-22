//! Source record conversion and bounded, compressed WordCard payloads.
//! SQLite storage and the review gate are implemented in ingest.rs.

use anyhow::{Context, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use crate::core::model::*;
use crate::data::store::normalize;

/// 欧陆原始 JSON 行 (输入格式, 仅编译期使用)
#[derive(Deserialize)]
pub struct RawEntry {
    #[serde(default)]
    headword: Option<String>,
    #[serde(default)]
    pos: Vec<String>,
    phon_uk: Option<String>,
    phon_us: Option<String>,
    /// 不分英美时的通用音标（欧陆 `<en>`）
    #[serde(default)]
    phon_en: Option<String>,
    /// 中文词条拼音（欧陆 `PY` 块）
    #[serde(default)]
    pinyin: Option<String>,
    #[serde(default)]
    senses: Vec<String>,
    english: Option<String>,
    #[serde(default)]
    synonyms: Vec<String>,
    #[serde(default)]
    antonyms: Vec<String>,
    #[serde(default)]
    related: Vec<String>,
    #[serde(default)]
    forms: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    examples: Vec<String>,
    /// 词条内短语（欧陆 `<SPEC>`）
    #[serde(default)]
    phrases: Vec<RawPhrase>,
}

#[derive(Deserialize)]
pub struct RawPhrase {
    phrase: String,
    #[serde(default)]
    gloss: Option<String>,
}

/// 清洗例句: "<LJ>内容" → 多对 (en, zh, keys)
///
/// 与 `example_sides` 是同一趟扫描的两个视角（词卡用这份、例句索引用那份），
/// 改过滤条件或切分规则必须同时改两处，否则索引会指向词卡里没有的句子。
pub fn parse_examples(raw: &[String]) -> Vec<Example> {
    let mut out = Vec::new();
    for seg in raw {
        let seg = RegexLite::remove_ljid(seg);
        for chunk in seg.split('²') {
            let chunk = chunk.trim();
            if let Some(at) = chunk.find('@') {
                let en = strip_html(&chunk[..at]);
                let zh = strip_html(&chunk[at + 1..]);
                if en.len() >= 4 && !zh.is_empty() {
                    // 关键词已在 strip 时丢失; 从原文提取
                    let keys = RegexLite::extract_keys(chunk);
                    out.push(Example { en, zh, keys });
                }
            }
        }
    }
    out
}

/// 例句的两侧和两侧的关键词：`example_link` 表按这个形状建索引。
///
/// 词卡里的 `Example.keys` 是对整段扫描得到的，不区分关键词落在英文半区还是
/// 中文半区，所以它没法用来自例句库。这里把同一句话切成两半各扫一次，才能把
/// "病假条" 这种中文关键词单独拿来查。切法和过滤条件必须与 `parse_examples`
/// 逐字相同，否则索引和词卡会指向不同的句子。
#[derive(Clone)]
pub struct Sides {
    pub en: String,
    pub zh: String,
    pub en_keys: Vec<String>,
    pub zh_keys: Vec<String>,
}

/// 与 `parse_examples` 同一趟、同一过滤，只多了"关键词属于哪一半"这一点信息。
pub fn example_sides(raw: &[String]) -> Vec<Sides> {
    let mut out = Vec::new();
    for seg in raw {
        let seg = RegexLite::remove_ljid(seg);
        for chunk in seg.split('²') {
            let chunk = chunk.trim();
            if let Some(at) = chunk.find('@') {
                let en = strip_html(&chunk[..at]);
                let zh = strip_html(&chunk[at + 1..]);
                if en.len() >= 4 && !zh.is_empty() {
                    out.push(Sides {
                        en_keys: RegexLite::extract_keys(&chunk[..at]),
                        zh_keys: RegexLite::extract_keys(&chunk[at + 1..]),
                        en,
                        zh,
                    });
                }
            }
        }
    }
    out
}

/// 极简正则替代 (避免 regex 依赖膨胀): 手工扫描
struct RegexLite;
impl RegexLite {
    fn remove_ljid(s: &str) -> String {
        const OPEN: &str = "<span class=\"ljid\"";
        const CLOSE: &str = "</span>";
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(p) = rest.find(OPEN) {
            out.push_str(&rest[..p]);
            match rest[p..].find(CLOSE) {
                Some(e) => rest = &rest[p + e + CLOSE.len()..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    fn extract_keys(s: &str) -> Vec<String> {
        const OPEN: &str = "<span class=\"key\">";
        const CLOSE: &str = "</span>";
        let mut keys = Vec::new();
        let mut rest = s;
        while let Some(p) = rest.find(OPEN) {
            let after = &rest[p + OPEN.len()..];
            match after.find(CLOSE) {
                Some(e) => {
                    let k = strip_html(&after[..e]);
                    if !k.is_empty() {
                        keys.push(k);
                    }
                    rest = &after[e + CLOSE.len()..];
                }
                None => break,
            }
        }
        keys
    }
}

fn parse_forms(forms: &std::collections::HashMap<String, String>) -> Vec<WordForm> {
    let mut out = Vec::new();
    for (label, words) in forms {
        let kind = FormKind::parse(label);
        let list: Vec<String> = words
            .split([',', '，', ';'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !list.is_empty() {
            out.push(WordForm { kind, words: list });
        }
    }
    out.sort_by(|a, b| a.kind.as_str().cmp(b.kind.as_str()));
    out
}

fn dedup_push<T: PartialEq>(v: &mut Vec<T>, item: T) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// `--related` 的释义表：一行 `词<TAB>中文`，用来给近义/反义/联想词补中文。
///
/// 表里五十多万行只有两万多个不同的词，同一个词重复出现时取**第一条**非空释义：
/// 后面的行是同一关系的重复，没有依据判定哪个义项更新。键按词库的 normalize 规则
/// 存，所以表里的大写词（`ASD`、`Hiroshima`）也能命中。
pub struct RelationGlosses {
    map: HashMap<String, String>,
    pub rows: usize,
}

impl RelationGlosses {
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path).context("读取联想词释义表")?;
        let mut map = HashMap::new();
        let mut rows = 0usize;
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            // 先按制表符切，再 trim 两半：整行 trim_end 会把行尾那个制表符也吃掉，
            // 而表里确实有「词+制表符+空释义」的行（Zune），它们该跳过而不是报错。
            let Some((word, gloss)) = line.split_once('\t') else {
                if line.trim().is_empty() {
                    continue;
                }
                anyhow::bail!("联想词释义表第 {} 行缺少制表符", index + 1);
            };
            rows += 1;
            let (word, gloss) = (word.trim(), gloss.trim());
            if word.is_empty() || gloss.is_empty() {
                continue;
            }
            map.entry(normalize(word)).or_insert(gloss.to_string());
        }
        Ok(Self { map, rows })
    }

    pub fn keys(&self) -> usize {
        self.map.len()
    }

    fn gloss(&self, word: &str) -> Option<String> {
        self.map.get(&normalize(word)).cloned()
    }
}

/// 把欧陆 `<FF>` 英英 HTML 整理成行式纯文本。
///
/// 原始 HTML 用 `<li>` 分义项、`<div class="eg">` 分例句、`<i>synonymous：</i>`
/// 分同义词；直接 `strip_html` 会把三者挤成一整段。这里保留成
/// `n.` / `1. ...` / `> ...` / `syn: ...` 四类行，渲染层据此排版。
/// 结构不符合预期时退回原来的纯文本清洗，保证旧输入仍可用。
pub fn parse_english_gloss(html: &str) -> String {
    #[derive(Default)]
    struct SenseBuf {
        definition: String,
        examples: Vec<String>,
        synonyms: Vec<String>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Idle,
        Pos,
        Definition,
        Example,
        Synonym,
    }

    fn clean_text(raw: &str) -> String {
        html_escape::decode_html_entities(raw)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn append_text(buf: &mut String, raw: &str) {
        let text = clean_text(raw);
        if text.is_empty() {
            return;
        }
        if !buf.is_empty() && !buf.ends_with(' ') {
            buf.push(' ');
        }
        buf.push_str(&text);
    }

    fn flush_synonyms(current: &mut Option<SenseBuf>, buf: &mut String) {
        if let Some(sense) = current.as_mut() {
            for word in buf.split(',') {
                let word = word.trim();
                if !word.is_empty() && !sense.synonyms.iter().any(|w| w == word) {
                    sense.synonyms.push(word.to_string());
                }
            }
        }
        buf.clear();
    }

    fn flush_item(mode: Mode, current: &mut Option<SenseBuf>, buf: &mut String) {
        let text = buf.trim().to_string();
        if !text.is_empty() {
            if let Some(sense) = current.as_mut() {
                match mode {
                    Mode::Definition => sense.definition = text,
                    Mode::Example => sense.examples.push(text),
                    _ => {}
                }
            }
        }
        buf.clear();
    }

    fn finish_sense(senses: &mut Vec<SenseBuf>, current: &mut Option<SenseBuf>) {
        if let Some(sense) = current.take() {
            if !sense.definition.is_empty()
                || !sense.examples.is_empty()
                || !sense.synonyms.is_empty()
            {
                senses.push(sense);
            }
        }
    }

    fn emit_group(lines: &mut Vec<String>, pos: &str, senses: &mut Vec<SenseBuf>) {
        if pos.is_empty() && senses.is_empty() {
            return;
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        if !pos.is_empty() {
            lines.push(pos.to_string());
        }
        let mut number = 0usize;
        for sense in senses.iter() {
            if sense.definition.is_empty() && sense.examples.is_empty() && sense.synonyms.is_empty()
            {
                continue;
            }
            if !sense.definition.is_empty() {
                number += 1;
                lines.push(format!("{number}. {}", sense.definition));
            }
            for example in &sense.examples {
                lines.push(format!("> {example}"));
            }
            if !sense.synonyms.is_empty() {
                lines.push(format!("syn: {}", sense.synonyms.join(", ")));
            }
        }
        senses.clear();
    }

    static TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)<[^>]*>|[^<]+").expect("constant regex")
    });

    let mut lines: Vec<String> = Vec::new();
    let mut senses: Vec<SenseBuf> = Vec::new();
    let mut current: Option<SenseBuf> = None;
    let mut pos = String::new();
    let mut mode = Mode::Idle;
    let mut buf = String::new();
    let mut in_examples = false;
    // true = 词性组 `<div class="exp">`，false = 例句组 `<div class="eg">`
    let mut divs: Vec<bool> = Vec::new();

    for token in TOKEN.find_iter(html) {
        let token = token.as_str();
        if token.starts_with('<') {
            let lower = token.to_ascii_lowercase();
            if lower.starts_with("<div") {
                if lower.contains("class=\"exp\"") {
                    pos.clear();
                    divs.push(true);
                } else if lower.contains("class=\"eg\"") {
                    in_examples = true;
                    divs.push(false);
                }
            } else if lower.starts_with("</div>") {
                match divs.pop() {
                    Some(true) => {
                        if mode == Mode::Synonym {
                            flush_synonyms(&mut current, &mut buf);
                        } else {
                            flush_item(mode, &mut current, &mut buf);
                        }
                        finish_sense(&mut senses, &mut current);
                        emit_group(&mut lines, &pos, &mut senses);
                        pos.clear();
                        mode = Mode::Idle;
                    }
                    Some(false) => {
                        flush_item(mode, &mut current, &mut buf);
                        in_examples = false;
                        mode = Mode::Idle;
                    }
                    None => {}
                }
            } else if lower.starts_with("<span") && lower.contains("class=\"cara\"") {
                mode = Mode::Pos;
                buf.clear();
            } else if lower.starts_with("</span>") {
                if mode == Mode::Pos {
                    pos = clean_text(&buf);
                    buf.clear();
                    mode = Mode::Idle;
                }
            } else if lower.starts_with("<ul") {
                in_examples = true;
            } else if lower.starts_with("</ul>") {
                if mode == Mode::Example {
                    flush_item(mode, &mut current, &mut buf);
                    mode = Mode::Idle;
                }
                in_examples = false;
            } else if lower.starts_with("<ol") {
                // 义项列表开始，无需额外状态
            } else if lower.starts_with("</ol>") {
                if mode == Mode::Synonym {
                    flush_synonyms(&mut current, &mut buf);
                } else {
                    flush_item(mode, &mut current, &mut buf);
                }
                mode = Mode::Idle;
                finish_sense(&mut senses, &mut current);
            } else if lower.starts_with("<li") {
                if mode == Mode::Synonym {
                    flush_synonyms(&mut current, &mut buf);
                } else if matches!(mode, Mode::Definition | Mode::Example) {
                    flush_item(mode, &mut current, &mut buf);
                }
                if in_examples {
                    mode = Mode::Example;
                    buf.clear();
                } else {
                    finish_sense(&mut senses, &mut current);
                    current = Some(SenseBuf::default());
                    mode = Mode::Definition;
                    buf.clear();
                }
            } else if lower.starts_with("</li") {
                if matches!(mode, Mode::Definition | Mode::Example) {
                    flush_item(mode, &mut current, &mut buf);
                }
                mode = Mode::Idle;
            }
        } else if token
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("synonymous")
        {
            if mode != Mode::Synonym {
                flush_item(mode, &mut current, &mut buf);
                mode = Mode::Synonym;
                buf.clear();
            }
        } else {
            match mode {
                Mode::Pos | Mode::Definition | Mode::Example | Mode::Synonym => {
                    append_text(&mut buf, token)
                }
                Mode::Idle => {}
            }
        }
    }

    // 缺少闭合标签时也要把最后一条留下的内容收进来。
    if mode == Mode::Synonym {
        flush_synonyms(&mut current, &mut buf);
    } else {
        flush_item(mode, &mut current, &mut buf);
    }
    finish_sense(&mut senses, &mut current);
    emit_group(&mut lines, &pos, &mut senses);

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return strip_html(html);
    }
    lines.join("\n")
}

/// 把一条欧陆 RawEntry 转换为自有 WordCard。`related_glosses` 只补中文，不增删词条。
pub fn convert(id: u32, raw: RawEntry, glosses: Option<&RelationGlosses>) -> WordCard {
    let headword = raw.headword.unwrap_or_default();
    let syllables = syllabify(&headword);
    let mut card = WordCard {
        source: None,
        id,
        headword,
        lemma: None,
        pinyin: raw
            .pinyin
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        phonetic: Phonetic {
            uk: raw.phon_uk.filter(|s| !s.is_empty()),
            us: raw.phon_us.filter(|s| !s.is_empty()),
            en: raw.phon_en.filter(|s| !s.is_empty()),
            syllables,
        },
        pos: Vec::new(),
        senses: Vec::new(),
        gloss_en: raw
            .english
            .filter(|s| !s.trim().is_empty())
            .map(|s| parse_english_gloss(&s)),
        forms: raw.forms.as_ref().map(parse_forms).unwrap_or_default(),
        synonyms: Vec::new(),
        antonyms: Vec::new(),
        related: Vec::new(),
        phrases: Vec::new(),
        examples: parse_examples(&raw.examples),
        labels: Vec::new(),
    };

    static ITEMS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?is)<li[^>]*>(.*?)</li>").expect("constant regex")
    });
    static POS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(vt\.\s*&\s*vi\.|vt\.|vi\.|adj\.|adv\.|prep\.|pron\.|conj\.|interj\.|int\.|abbr\.|n\.|v\.)\s*").expect("constant regex")
    });
    for s in &raw.senses {
        // Morphology lives in its own structural section, not in the definition.
        let definition = s.split("<div id=\"trans\">").next().unwrap_or(s);
        let captures: Vec<_> = ITEMS
            .captures_iter(definition)
            .map(|c| c[1].to_string())
            .collect();
        let fragments = if captures.is_empty() {
            vec![definition.to_string()]
        } else {
            let mut parts = captures;
            if let Some(end) = definition.rfind("</ol>") {
                let trailing = &definition[end + 5..];
                if !strip_html(trailing).is_empty() {
                    parts.push(trailing.to_string());
                }
            }
            parts
        };
        let mut inherited = None;
        for fragment in fragments {
            let text = strip_html(&fragment);
            let (pos, gloss) = if let Some(c) = POS.captures(&text) {
                let pos = PartOfSpeech::parse(c.get(1).unwrap().as_str());
                (pos, text[c.get(0).unwrap().end()..].to_string())
            } else {
                (inherited, text)
            };
            if gloss.is_empty() {
                continue;
            }
            inherited = pos;
            let labels = extract_labels(&fragment);
            for label in &labels {
                dedup_push(&mut card.labels, label.clone());
            }
            card.senses.push(Sense {
                pos,
                gloss,
                labels,
                raw_html: None,
            });
        }
    }

    for p in &raw.pos {
        if let Some(ps) = PartOfSpeech::parse(p) {
            dedup_push(&mut card.pos, ps);
        }
    }

    // dic:// 链接词 → synonyms/antonyms/related
    let self_key = normalize(&card.headword);
    let relation = |words: &[String], list: &mut Vec<Relation>| {
        for s in words {
            let word = s.replace("%20", " ");
            if word.is_empty() || normalize(&word) == self_key {
                continue;
            }
            let gloss = glosses.and_then(|g| g.gloss(&word));
            dedup_push(
                list,
                Relation {
                    word,
                    gloss: gloss.filter(|g| !g.is_empty()),
                },
            );
        }
    };
    relation(&raw.synonyms, &mut card.synonyms);
    relation(&raw.antonyms, &mut card.antonyms);
    relation(&raw.related, &mut card.related);

    for phrase in &raw.phrases {
        let word = phrase.phrase.trim().to_string();
        if word.is_empty() {
            continue;
        }
        dedup_push(
            &mut card.phrases,
            Relation {
                word,
                gloss: phrase
                    .gloss
                    .as_ref()
                    .map(|g| g.trim().to_string())
                    .filter(|g| !g.is_empty()),
            },
        );
    }

    card
}

/// Pack a bounded card with its compressed length.
pub fn pack_compressed(json: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(json.len() <= 8 * 1024 * 1024, "词卡超过 8 MiB");
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(6));
    enc.write_all(json)?;
    let compressed = enc.finish()?;
    let mut v = Vec::with_capacity(compressed.len() + 4);
    v.extend_from_slice(&u32::try_from(compressed.len())?.to_le_bytes());
    v.extend_from_slice(&compressed);
    Ok(v)
}
pub fn decompress_card(data: &[u8]) -> Result<WordCard> {
    anyhow::ensure!(data.len() >= 4, "词卡已截断");
    let size = u32::from_le_bytes(data[..4].try_into()?) as usize;
    anyhow::ensure!(size == data.len() - 4, "词卡长度不符");
    let dec = ZlibDecoder::new(&data[4..]);
    let mut out = Vec::new();
    dec.take(8 * 1024 * 1024 + 1).read_to_end(&mut out)?;
    anyhow::ensure!(out.len() <= 8 * 1024 * 1024, "词卡解压超限");
    Ok(serde_json::from_slice(&out)?)
}

#[cfg(test)]
mod tests {
    use super::parse_english_gloss;

    const LOVE: &str = r#"<div class="exp"><span class="cara">n.</span><ol><li>a strong positive emotion of regard and affection</li><div class="eg"><ul><li>his love for his work</li><li>children need a lot of love</li></ul></div><br /><i>synonymous：</i> <a href="dic://passion">passion</a><li>a beloved person; used as terms of endearment</li><br /><i>synonymous：</i> <a href="dic://beloved">beloved</a>, <a href="dic://dear">dear</a></ol></div>"#;

    #[test]
    fn english_gloss_keeps_definition_example_and_synonym_boundaries() {
        assert_eq!(
            parse_english_gloss(LOVE),
            "n.\n\
             1. a strong positive emotion of regard and affection\n\
             > his love for his work\n\
             > children need a lot of love\n\
             syn: passion\n\
             2. a beloved person; used as terms of endearment\n\
             syn: beloved, dear"
        );
    }

    #[test]
    fn english_gloss_separates_pos_groups() {
        let html = concat!(
            r#"<div class="exp"><span class="cara">n.</span><ol><li>a thing</li></ol></div>"#,
            r#"<div class="exp"><span class="cara">v.</span><ol><li>to do</li></ol></div>"#
        );
        assert_eq!(parse_english_gloss(html), "n.\n1. a thing\n\nv.\n1. to do");
    }

    #[test]
    fn english_gloss_falls_back_to_plain_text() {
        assert_eq!(
            parse_english_gloss("plain <b>english</b> text"),
            "plain english text"
        );
    }
}
