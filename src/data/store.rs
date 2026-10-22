//! Read-only, self-contained dictionary and one query contract for all front ends.
use crate::core::model::{AffixHint, PartOfSpeech, WordCard};
use anyhow::{ensure, Context, Result};
use clap::ValueEnum;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

pub const SCHEMA_VERSION: u32 = 2;
pub const APPLICATION_ID: u32 = 0x5a454449;
pub const MAX_QUERY_CHARS: usize = 256;
pub const MAX_LIMIT: usize = 1000;

/// 常见前缀，以及它们在中文释义里通常留下的语义标记。标记只用来判断
/// “拆出来的基础词和整词意思是否对得上”，不是词源数据；词缀释义本身
/// 仍只从本地词库读取，词条缺失就跳过。
const PREFIX_RULES: &[(&str, &[&str])] = &[
    ("anti-", &["反", "抗", "防", "杀"]),
    ("auto-", &["自", "自动"]),
    ("be-", &["使", "加强"]),
    ("bi-", &["二", "双", "两"]),
    ("bio-", &["生物", "生命"]),
    ("by-", &["副", "旁", "侧"]),
    ("chrono-", &["时间", "年代"]),
    ("circum-", &["环绕", "周围", "周"]),
    ("co-", &["共同", "一起", "同", "合"]),
    ("contra-", &["反", "相反", "逆"]),
    ("counter-", &["反", "抗", "逆"]),
    ("cyber-", &["网络", "电脑", "赛博"]),
    (
        "de-",
        &["除", "去", "解", "脱", "降", "下", "停", "反", "贬", "逐"],
    ),
    (
        "dis-",
        &["不", "无", "非", "未", "没", "反", "失", "断", "停"],
    ),
    ("eco-", &["生态", "环保"]),
    ("em-", &["使", "进入"]),
    ("en-", &["使", "进入"]),
    ("ex-", &["出", "外", "前", "离"]),
    ("extra-", &["外", "额外", "超"]),
    ("fore-", &["前", "预", "先"]),
    ("geo-", &["地", "地质", "地理"]),
    ("giga-", &["吉", "千兆", "十亿"]),
    ("hetero-", &["异", "不同"]),
    ("homo-", &["同", "相同"]),
    ("hydro-", &["水", "氢"]),
    ("hyper-", &["超", "过度", "极", "高"]),
    ("il-", &["不", "无", "非", "未", "没"]),
    ("im-", &["不", "无", "非", "未", "没"]),
    ("in-", &["不", "无", "非", "未", "没"]),
    ("inter-", &["之间", "际", "相互", "互"]),
    ("intra-", &["内", "内部"]),
    ("ir-", &["不", "无", "非", "未", "没"]),
    ("iso-", &["等", "同"]),
    ("macro-", &["大", "宏", "宏观"]),
    ("mega-", &["大", "兆", "百万"]),
    ("micro-", &["微", "小"]),
    ("mid-", &["中", "中间", "期中"]),
    ("mini-", &["小", "迷你"]),
    ("mis-", &["错", "误", "坏", "不"]),
    ("mono-", &["单", "独"]),
    ("morpho-", &["形态"]),
    ("multi-", &["多"]),
    ("nano-", &["纳", "纳米", "微小"]),
    ("neo-", &["新"]),
    ("neuro-", &["神经"]),
    ("non-", &["非", "不", "无"]),
    ("omni-", &["全", "遍"]),
    ("out-", &["出", "外", "超", "比", "胜", "多", "过"]),
    ("over-", &["过", "超", "上", "越", "覆", "盖", "溢", "高"]),
    ("pan-", &["全", "泛"]),
    ("photo-", &["光", "照片", "照相"]),
    ("poly-", &["多", "聚"]),
    ("post-", &["后", "迟", "附"]),
    ("pre-", &["预", "先", "前", "早"]),
    ("pro-", &["前", "先", "领先", "支持", "赞成"]),
    ("proto-", &["原始", "初", "原型"]),
    ("pseudo-", &["假", "伪"]),
    ("psycho-", &["心理"]),
    ("quasi-", &["准", "半", "类似"]),
    ("re-", &["再", "又", "重", "回", "返", "改", "新"]),
    ("retro-", &["复古", "向后"]),
    ("self-", &["自我", "自己", "自身"]),
    ("semi-", &["半"]),
    (
        "sub-",
        &["下", "低", "次", "亚", "副", "潜", "地", "子", "底"],
    ),
    ("super-", &["超", "上", "过", "极"]),
    ("techno-", &["技术", "科技"]),
    ("tele-", &["远", "电"]),
    ("thermo-", &["热", "温度"]),
    ("trans-", &["横", "跨", "越", "穿", "转", "变"]),
    ("tri-", &["三"]),
    ("ultra-", &["超", "极", "外"]),
    ("un-", &["不", "无", "非", "未", "没"]),
    ("under-", &["下", "低", "不足", "少", "次", "内"]),
    ("uni-", &["单"]),
    ("up-", &["上", "升", "提高", "支持"]),
];

/// 这些前缀的意思在中文里常被整词吞掉（`overcoat` 的“大衣”看不出“过”），
/// 所以只靠“基础词和整词共享一个实义字”来放行，条件更严。
const SHARED_ONLY_PREFIXES: &[&str] = &["over-"];

/// 释义里出现频率极高、但不携带区分度的字。共享字统计要先剔除它们，
/// 否则“的/是/在/有/一”会让任何两个释义都像有关系。
const GLOSS_STOP_CHARS: &[char] = &[
    '的', '了', '着', '是', '在', '有', '和', '与', '及', '或', '之', '其', '这', '那', '个', '们',
    '我', '你', '他', '她', '它', '把', '被', '使', '地', '得', '能', '会', '可', '以', '为', '于',
    '而', '并', '也', '都', '很', '更', '最', '某', '等', '每', '些', '种', '吗', '呢', '吧', '啊',
    '所', '该', '此', '本', '应', '将', '已', '还', '一',
];

fn is_han_char(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af)
}

/// 第一条非空中文释义。词卡的 `senses` 已去 HTML，直接取用。
fn first_sense_gloss(card: &WordCard) -> Option<&str> {
    card.senses
        .iter()
        .map(|sense| sense.gloss.as_str())
        .find(|gloss| !gloss.trim().is_empty())
}

/// 去掉释义开头的括号注释：`（使）转动`、`（英）谷物` 都只留后面的正文。
fn strip_leading_note(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        let close = if trimmed.starts_with('（') {
            '）'
        } else if trimmed.starts_with('(') {
            ')'
        } else {
            return trimmed;
        };
        let open_len = trimmed.chars().next().map_or(0, char::len_utf8);
        let rest = &trimmed[open_len..];
        let Some(end) = rest.find(close) else {
            return trimmed;
        };
        text = &rest[end + close.len_utf8()..];
    }
}

/// 把一段文字压到 `max` 个字，超出时补省略号。
fn clip_gloss(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// 取第一个分句并压到 `max` 个字。构词提示只需要一个短标签，不复制整段释义。
fn brief_gloss(text: &str, max: usize) -> String {
    let text = strip_leading_note(text.trim());
    let cut = text
        .find(['；', ';', '，', ',', '。', '\n'])
        .unwrap_or(text.len());
    let out = text[..cut].split_whitespace().collect::<Vec<_>>().join(" ");
    clip_gloss(&out, max)
}

/// 把一条释义拆成可比较的短片段：按中英文标点断开，去掉开头的“（使）”，
/// 丢弃没有实义汉字（只剩“的/了/和”之类）的碎片。
fn gloss_fragments(gloss: &str) -> Vec<String> {
    gloss
        .split(['；', ';', '，', ',', '。', '、', '\n'])
        .filter_map(|part| {
            let text = strip_leading_note(part.trim());
            let meaningful = text
                .chars()
                .any(|c| is_han_char(c) && !GLOSS_STOP_CHARS.contains(&c));
            meaningful.then(|| text.to_string())
        })
        .collect()
}

fn card_fragments(card: &WordCard) -> Vec<String> {
    card.senses
        .iter()
        .flat_map(|sense| gloss_fragments(&sense.gloss))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum PosFamily {
    Noun,
    Verb,
    Adjective,
    Adverb,
    Preposition,
    Pronoun,
    Conjunction,
    Interjection,
    Article,
    Numeral,
    Other,
}

fn pos_family(pos: PartOfSpeech) -> PosFamily {
    match pos {
        PartOfSpeech::Noun => PosFamily::Noun,
        PartOfSpeech::Verb
        | PartOfSpeech::TransitiveVerb
        | PartOfSpeech::IntransitiveVerb
        | PartOfSpeech::AmbitransitiveVerb
        | PartOfSpeech::Auxiliary => PosFamily::Verb,
        PartOfSpeech::Adjective => PosFamily::Adjective,
        PartOfSpeech::Adverb => PosFamily::Adverb,
        PartOfSpeech::Preposition => PosFamily::Preposition,
        PartOfSpeech::Pronoun => PosFamily::Pronoun,
        PartOfSpeech::Conjunction => PosFamily::Conjunction,
        PartOfSpeech::Interjection => PosFamily::Interjection,
        PartOfSpeech::Article => PosFamily::Article,
        PartOfSpeech::Numeral => PosFamily::Numeral,
        PartOfSpeech::Abbreviation => PosFamily::Other,
    }
}

/// 一张词卡上所有实义词性的家族；缩写不算。
fn content_pos_families(card: &WordCard) -> HashSet<PosFamily> {
    card.pos
        .iter()
        .copied()
        .chain(card.senses.iter().filter_map(|sense| sense.pos))
        .filter(|pos| *pos != PartOfSpeech::Abbreviation)
        .map(pos_family)
        .collect()
}

/// 两个短片段共享多少个不同的实义汉字。虚字不算，避免“的/是/一”制造假联系。
fn shared_chars(a: &str, b: &str) -> usize {
    let mut seen = HashSet::new();
    a.chars()
        .filter(|c| is_han_char(*c) && !GLOSS_STOP_CHARS.contains(c))
        .filter(|c| b.contains(*c))
        .filter(|c| seen.insert(*c))
        .count()
}

/// 从基础词的所有义项里选出和整词最贴近的一条。先看整词释义是否直接
/// 包含它（最强证据），再看有没有共享的实义字；同分时保留词典里的第一条。
fn pick_base_gloss(
    base_fragments: &[String],
    word_fragments: &[String],
) -> Option<(String, bool, bool, bool)> {
    let mut best: Option<(usize, String, bool, bool, bool)> = None;
    for fragment in base_fragments {
        let contained = word_fragments
            .iter()
            .any(|word| word != fragment && word.contains(fragment.as_str()));
        let equal = word_fragments.iter().any(|word| word == fragment);
        let shared = word_fragments
            .iter()
            .any(|word| shared_chars(word, fragment) > 0);
        let score = usize::from(contained || equal) * 4 + usize::from(shared) * 2;
        let replace = match &best {
            Some((best_score, ..)) => score > *best_score,
            None => true,
        };
        if replace {
            best = Some((score, fragment.clone(), contained, equal, shared));
        }
    }
    best.map(|(_, fragment, contained, equal, shared)| (fragment, contained, equal, shared))
}

/// 从整词的义项里选一条来展示：优先带前缀语义标记的，其次和基础词共享实义字的。
fn pick_word_gloss(
    word_fragments: &[String],
    base_gloss: &str,
    markers: &[&str],
) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for fragment in word_fragments {
        let marker_hits = markers
            .iter()
            .filter(|marker| fragment.contains(**marker))
            .count();
        let shared = shared_chars(fragment, base_gloss);
        let contained = usize::from(fragment != base_gloss && fragment.contains(base_gloss));
        let score = marker_hits * 3 + shared * 2 + contained * 2;
        let replace = match &best {
            Some((best_score, _)) => score > *best_score,
            None => true,
        };
        if replace {
            best = Some((score, fragment.clone()));
        }
    }
    best.map(|(_, fragment)| fragment)
}

/// 把词缀词条的释义压成提示里能用的短义，例如
/// “pref. 表示‘又,再’之义” → “又,再”。
fn affix_meaning(gloss: &str) -> String {
    let mut text = gloss.trim();
    for label in ["pref.", "pref", "comb.", "comb", "suf.", "suf"] {
        if let Some(rest) = text.strip_prefix(label) {
            text = rest.trim_start_matches([' ', '：', ':']);
            break;
        }
    }
    if let Some(start) = text.find('“') {
        let after = &text[start + '“'.len_utf8()..];
        if let Some(end) = after.find('”') {
            let quoted = after[..end].trim();
            if !quoted.is_empty() {
                let mut out = quoted.chars().take(18).collect::<String>();
                if quoted.chars().count() > 18 {
                    out.push('…');
                }
                return out;
            }
        }
    }
    let text = text.strip_prefix("表示").unwrap_or(text);
    let text = text.strip_suffix("之义").unwrap_or(text);
    brief_gloss(text, 18)
}

pub fn normalize(s: &str) -> String {
    s.nfc()
        .collect::<String>()
        .replace(['’', '‘'], "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn search_tokens(s: &str) -> String {
    normalize(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_han(s: &str) -> bool {
    s.chars().any(is_han_char)
}

/// 一条拼音读音在检索表里的两种形状：无声调连写与按音节分开。
///
/// 中文词条的 `pinyin` 是空格分音节的带调文本（`xiàn`），少数条目写成
/// `jun1` 这种数字调。两种写法都折叠成 ASCII，`ü` 归一到 `v`，所以
/// `lü`、`lv`、`lǜ` 查到同一批词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinKey {
    pub flat: String,
    pub syllables: String,
}

fn pinyin_char(c: char) -> Option<char> {
    Some(match c {
        'a'..='z' => c,
        'ā' | 'á' | 'ǎ' | 'à' => 'a',
        'ē' | 'é' | 'ě' | 'è' => 'e',
        'ī' | 'í' | 'ǐ' | 'ì' => 'i',
        'ō' | 'ó' | 'ǒ' | 'ò' => 'o',
        'ū' | 'ú' | 'ǔ' | 'ù' => 'u',
        'ü' | 'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' => 'v',
        'ń' | 'ň' | 'ǹ' => 'n',
        'ḿ' => 'm',
        'ê' => 'e',
        _ => return None,
    })
}

fn pinyin_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '_' | '-' | '\'' | '’' | '·' | '/')
}

/// 把 `pinyin` 拆成读音变体，每条写出连写与分音节两种键。
///
/// 源数据用 `，`/`；` 分隔多音字（`shé，zhé，zhē`），音节的其它分隔符
/// （空格、下划线、连字符、隔音撇号）只影响 `syllables` 的边界。
pub fn pinyin_keys(pinyin: &str) -> Vec<PinyinKey> {
    let text: String = pinyin.nfc().collect();
    let mut keys = Vec::new();
    for variant in text.split(['，', ',', '；', ';']) {
        let mut tokens = Vec::new();
        for token in variant.split(pinyin_separator) {
            let syllable: String = token
                .chars()
                .flat_map(char::to_lowercase)
                .filter_map(pinyin_char)
                .collect();
            if !syllable.is_empty() {
                tokens.push(syllable);
            }
        }
        if tokens.is_empty() {
            continue;
        }
        let flat = tokens.concat();
        if keys.iter().any(|k: &PinyinKey| k.flat == flat) {
            continue;
        }
        keys.push(PinyinKey {
            syllables: tokens.join(" "),
            flat,
        });
    }
    keys
}

/// Auto 模式里什么样的查询可以交给拼音索引。带汉字的查询归释义反查。
fn is_pinyin_like(s: &str) -> bool {
    !has_han(s)
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| {
            c.is_alphabetic()
                || c.is_ascii_digit()
                || c.is_whitespace()
                || matches!(c, '_' | '-' | '\'' | '’' | '·' | '/')
        })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    #[default]
    Auto,
    Exact,
    Prefix,
    Contains,
    Fuzzy,
    Reverse,
    Pinyin,
}

#[derive(Debug, Clone)]
pub struct QueryRequest {
    pub query: String,
    pub mode: SearchMode,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub key: String,
    pub match_type: String,
    pub cards: Vec<WordCard>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub matched_senses: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub relations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub word: String,
    pub distance: usize,
    /// 距离是相对哪个**词形**量出来的：拼错的屈折形式（`runing`→`running`）只把
    /// 原形 `run` 端上来时，命中的那个词形记在这里。`word` 自己就是命中词形时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub via: Option<String>,
}

/// 可选表 `word_level` 的一行。等级来自 `tools/word_levels.py`，按**词形**记录。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordLevel {
    /// 1 = 最常用。`None` 表示该词形只在某份大纲词表里，源表没给频率等级。
    pub level: Option<u8>,
    /// 大纲归属的短名，如 `cet4`、`junior`、`collins1`。
    pub tags: Vec<String>,
}

/// 可选表 `example` / `example_link` 的一行：一句英中对照，加上它挂在哪些键下。
///
/// 句子本体只存在这里，词卡 BLOB 不会被改动；`headwords` 就是"这句是从哪几张卡
/// 抽出来的"，`--full` 和例句反查都靠它分组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExampleHit {
    pub en: String,
    pub zh: String,
    /// 英文半区的关键词（`side=0`）
    pub keys: Vec<String>,
    /// 中文半区的关键词（`side=1`）
    pub zh_keys: Vec<String>,
    /// 这句挂在哪些词头下（`side=2`）
    pub headwords: Vec<String>,
}

/// 一次 `IN` 查询最多带多少个键：一批 500 个，远低于 SQLite 的绑定变量上限，
/// 长文的词数再多的也是一页一页问完。
const LEVEL_PAGE: usize = 500;

/// 一句例句挂着的三类键：英文关键词、中文关键词、所属词头。
type LinkSides = (Vec<String>, Vec<String>, Vec<String>);

#[derive(Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub schema_version: u32,
    pub query: String,
    pub normalized_query: String,
    pub status: String,
    pub hits: Vec<SearchHit>,
    pub suggestions: Vec<Suggestion>,
}

impl QueryResponse {
    pub fn found(&self) -> bool {
        !self.hits.is_empty() || (self.status == "suggestions" && !self.suggestions.is_empty())
    }
}

pub struct DictStore {
    conn: Connection,
    /// 可选表 `word_level` 在本次打开时是否存在。只记一个布尔值，不去 eager
    /// prepare 这张表——没有它构建的老词库必须照样能开。
    has_levels: bool,
    /// 可选表 `example`/`example_link` 是否存在，同样是探测出来的。
    has_examples: bool,
    /// 可选表 `spell_key` 是否存在——没有它 `fuzzy` 就按今天的行为走。
    has_spell: bool,
    /// 可选表 `key_evidence` 是否存在（`--key-evidence some|strong` 才建）。
    /// 它记的是"每个源行号的词头有多少自证"，`--glossary-auto` 只认它等于
    /// 例句高亮词的那些键。
    has_key_evidence: bool,
    /// 可选表 `vocab` 是否存在（`--vocab <参考词表>` 才建）。
    has_vocab: bool,
    /// 可选表 `pinyin` 是否存在（`--pinyin` 才建）。
    has_pinyin: bool,
}

/// 一张可选表在不在。只查 `sqlite_master`：`prepare` 一张不存在的表会让老词库
/// 连打开都失败，而可选表按定义就是"老词库没有它也得能用"。
fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1")?;
    Ok(stmt.exists([name])?)
}

impl DictStore {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = std::fs::File::open(path).with_context(|| format!(
            "无法读取词库 {}；运行 zedict compile --input <源.jsonl> --reviews <核验.json> --out <目录>，然后配置 db 或 ZEDICT_DB", path.display()))?;
        let mut magic = [0u8; 16];
        file.read_exact(&mut magic).context("词库已截断")?;
        ensure!(
            !magic.starts_with(b"ZEDICT01"),
            "旧 ZEDICT01 词库不能直接迁入；请从已核验源数据重新 compile，旧文件不会改动"
        );
        ensure!(&magic == b"SQLite format 3\0", "不是 SQLite 词库");
        drop(file);
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            16 * 1024 * 1024,
        );
        conn.execute_batch(
            "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-4096;",
        )?;
        let app: u32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            app == APPLICATION_ID && version == SCHEMA_VERSION,
            "词库身份或版本不兼容，请重新构建"
        );
        // Prepare every required relation without scanning the whole dictionary at startup.
        conn.prepare("SELECT e.id,e.key,e.char_len,e.card,a.form,a.lemma,a.kind,a.evidence FROM entries e LEFT JOIN aliases a ON 0 LIMIT 0")?;
        conn.prepare("SELECT entry_id,gloss,tokens FROM sense_search LIMIT 0")?;
        let _: String = conn.query_row("SELECT value FROM meta WHERE key='report'", [], |r| {
            r.get(0)
        })?;
        // 可选表只探测存在性：不带对应标志构建的词库没有它们，也必须能正常打开。
        let has_levels = table_exists(&conn, "word_level")?;
        let has_examples = table_exists(&conn, "example")?;
        let has_spell = table_exists(&conn, "spell_key")?;
        let has_key_evidence = table_exists(&conn, "key_evidence")?;
        let has_vocab = table_exists(&conn, "vocab")?;
        let has_pinyin = table_exists(&conn, "pinyin")?;
        Ok(Self {
            conn,
            has_levels,
            has_examples,
            has_spell,
            has_key_evidence,
            has_vocab,
            has_pinyin,
        })
    }

    /// 词库里有没有词级表（`zedict compile --levels` 才会建）。
    pub fn has_levels(&self) -> bool {
        self.has_levels
    }

    /// 词库里有没有例句索引（`zedict compile --examples` 才会建）。
    pub fn has_examples(&self) -> bool {
        self.has_examples
    }

    /// 词库里有没有删字符拼写索引（`zedict compile --spell` 才会建）。
    pub fn has_spell(&self) -> bool {
        self.has_spell
    }

    /// 词库里有没有拼音索引（`zedict compile --pinyin` 才会建）。
    pub fn has_pinyin(&self) -> bool {
        self.has_pinyin
    }

    /// 词库里有没有词头证据表（`zedict compile --key-evidence some|strong` 才会建）。
    pub fn has_key_evidence(&self) -> bool {
        self.has_key_evidence
    }

    /// 词库里有没有参考词表（`zedict compile --vocab <文件>` 才会建）。
    pub fn has_vocab(&self) -> bool {
        self.has_vocab
    }

    /// 这个词在不在参考词表里。表不存在时返回 `false`，调用方据此什么也不说——
    /// 提示是增益，不是一个缺表就报错的新前提。
    pub fn vocab_contains(&self, word: &str) -> Result<bool> {
        if !self.has_vocab {
            return Ok(false);
        }
        let mut stmt = self.conn.prepare("SELECT 1 FROM vocab WHERE word=?1")?;
        Ok(stmt.exists([word])?)
    }

    /// 一个键在库里的证据档位集合。同一个键可能来自多个源行（多条卡），所以要看
    /// 全部档位：只要有一行是例句高亮词，这个键就是自证的。
    /// 没有 `key_evidence` 表时返回空——老词库不假装自己有这个信息。
    fn key_tiers(&self, key: &str) -> Result<Vec<u8>> {
        if !self.has_key_evidence {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT k.tier FROM key_evidence k JOIN entries e ON e.id = k.line WHERE e.key = ?1",
        )?;
        let rows = stmt.query_map([key], |row| row.get::<_, u8>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 自动术语表的候选：`terms` 里哪些是本库的词头，以及它们的第一条中文释义。
    ///
    /// 只收**证据通过阈值**的键，释义才敢钉进提示词——一个 E 档词头（比如挂在
    /// `angle` 名下的"气骨导间距"）一旦进了术语表，错译法就被写死在每一段译文里了。
    /// 缺 `key_evidence` 表时返回空：调用方必须报错而不是退化成"任意键都算术语"。
    pub fn evidenced_keys(
        &self,
        terms: &[String],
        evidence: crate::core::evidence::KeyEvidence,
    ) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        let mut gloss = self
            .conn
            .prepare("SELECT card FROM entries WHERE key=?1 ORDER BY id LIMIT 1")?;
        for term in terms {
            let accepted = self
                .key_tiers(term)?
                .iter()
                .filter_map(|raw| crate::core::evidence::Tier::from_u8(*raw))
                .any(|tier| evidence.accepts(tier));
            if !accepted {
                continue;
            }
            let Some(bytes) = gloss
                .query_row([term], |row| row.get::<_, Vec<u8>>(0))
                .optional()?
            else {
                continue;
            };
            let card = crate::data::compile::decompress_card(&bytes)?;
            if let Some(gloss_text) = card.senses.first().map(|sense| sense.gloss.trim()) {
                if !gloss_text.is_empty() {
                    out.push((term.clone(), gloss_text.to_string()));
                }
            }
        }
        Ok(out)
    }

    /// 一个串的"删掉恰好一个字符"邻域，外加它自己（删零次）。构建侧对每个词头做
    /// 同样的事，所以两边在这组串里的那一个上会师：查询少打一个字母时，查询自己
    /// 就等于词头的某个删字符形式。空串不算邻域——它对任何词都成立。
    fn delete_neighbors(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = vec![text.to_string()];
        for skip in 0..chars.len() {
            let del: String = chars
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != skip)
                .map(|(_, c)| *c)
                .collect();
            if !del.is_empty() {
                out.push(del);
            }
        }
        out
    }

    /// 穷举扫描之后仍然没排满 `want` 条时，从删字符索引里补一批候选。
    /// 距离用 `max+1` 重新算过——补位候选也要真距离，不能假设就是一步。
    /// 返回的候选一定不在 `skip` 里，所以调用方可以安心接在已排序结果之后。
    fn spell_fill(
        &self,
        word: &str,
        max: usize,
        skip: &HashSet<String>,
    ) -> Result<Vec<Suggestion>> {
        if !self.has_spell {
            return Ok(Vec::new());
        }
        let keys = Self::delete_neighbors(word);
        let mut matched: HashMap<String, usize> = HashMap::new();
        for chunk in keys.chunks(LEVEL_PAGE) {
            let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT DISTINCT word FROM spell_key WHERE delkey IN ({marks})");
            let mut stmt = self.conn.prepare_cached(&sql)?;
            for row in stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                r.get::<_, String>(0)
            })? {
                let candidate = row?;
                if skip.contains(&candidate) {
                    continue;
                }
                let Some(distance) = edit_distance(word, &candidate, max) else {
                    continue;
                };
                match matched.get(&candidate) {
                    Some(best) if *best <= distance => {}
                    _ => {
                        matched.insert(candidate, distance);
                    }
                }
            }
        }
        let levels = self.levels(&matched.keys().cloned().collect::<Vec<_>>())?;
        let query_len = word.chars().count();
        let mut scored: Vec<_> = matched
            .into_iter()
            .map(|(candidate, distance)| {
                let level = levels
                    .get(&candidate)
                    .and_then(|found| found.level)
                    .unwrap_or(u8::MAX);
                (
                    distance,
                    level,
                    candidate.chars().count().abs_diff(query_len),
                    candidate,
                )
            })
            .collect();
        scored.sort();
        Ok(scored
            .into_iter()
            .map(|(distance, _, _, word)| Suggestion {
                word,
                distance,
                via: None,
            })
            .collect())
    }

    /// 一批键的例句，按关键词精确命中（`example_link.key`）。表不存在返回空。
    /// 命中的句子按英文长度短的在前——短句的上下文最好读。
    pub fn examples_for(&self, keys: &[String], limit: usize) -> Result<Vec<ExampleHit>> {
        if !self.has_examples || keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for chunk in keys.chunks(LEVEL_PAGE) {
            let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql =
                format!("SELECT DISTINCT example_id FROM example_link WHERE key IN ({marks})");
            let mut stmt = self.conn.prepare_cached(&sql)?;
            ids.extend(
                stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                    r.get::<_, i64>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        ids.sort_unstable();
        ids.dedup();
        self.examples_by_ids(&ids, limit)
    }

    /// 全表子串扫描（`instr`，不用 `LIKE`，因此没有通配符和转义面）。不分两侧：
    /// 中文词几乎不会出现在英文半区，而中文句里夹的英文词本来就是想要的命中。
    /// 英文半区折大小写，因为例句正文保留原样而查询已经 `normalize` 过。
    pub fn examples_scan(&self, needle: &str, limit: usize) -> Result<Vec<ExampleHit>> {
        if !self.has_examples || needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM example WHERE instr(lower(en),?1)>0 OR instr(zh,?1)>0 ORDER BY length(en),id LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(params![needle, limit.min(MAX_LIMIT)], |r| {
                r.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        self.examples_by_ids(&ids, limit)
    }

    /// 关键词精确命中的总句数（不受 `limit` 影响），用来说"命中 N 句，显示 M 句"。
    pub fn example_key_matches(&self, key: &str) -> Result<usize> {
        if !self.has_examples {
            return Ok(0);
        }
        Ok(self.conn.query_row(
            "SELECT count(DISTINCT example_id) FROM example_link WHERE key=?1",
            [key],
            |r| r.get::<_, usize>(0),
        )?)
    }

    /// 全文扫描命中的总句数（不受 `limit` 影响）。例句库只有六万多句，这一趟和
    /// 检索本身一样是必须扫的，多问一个 count 不改变数量级。
    pub fn example_scan_matches(&self, needle: &str) -> Result<usize> {
        if !self.has_examples || needle.is_empty() {
            return Ok(0);
        }
        Ok(self.conn.query_row(
            "SELECT count(*) FROM example WHERE instr(lower(en),?1)>0 OR instr(zh,?1)>0",
            [needle],
            |r| r.get::<_, usize>(0),
        )?)
    }

    /// 取这些例句的正文和关键词链，排序后截到 `limit` 条。
    fn examples_by_ids(&self, ids: &[i64], limit: usize) -> Result<Vec<ExampleHit>> {
        let mut rows: Vec<(i64, String, String)> = Vec::new();
        for chunk in ids.chunks(LEVEL_PAGE) {
            let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id,en,zh FROM example WHERE id IN ({marks})");
            let mut stmt = self.conn.prepare_cached(&sql)?;
            rows.extend(
                stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        rows.sort_by_key(|(id, en, _)| (en.chars().count(), *id));
        rows.truncate(limit.min(MAX_LIMIT));
        let kept: Vec<i64> = rows.iter().map(|(id, _, _)| *id).collect();
        let mut links: HashMap<i64, LinkSides> = HashMap::new();
        for chunk in kept.chunks(LEVEL_PAGE) {
            let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT example_id,side,key FROM example_link WHERE example_id IN ({marks}) ORDER BY key"
            );
            let mut stmt = self.conn.prepare_cached(&sql)?;
            for row in stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (id, side, key) = row?;
                let (keys, zh_keys, headwords) = links.entry(id).or_default();
                match side {
                    0 => keys.push(key),
                    1 => zh_keys.push(key),
                    _ => headwords.push(key),
                }
            }
        }
        Ok(rows
            .into_iter()
            .map(|(id, en, zh)| {
                let (keys, zh_keys, headwords) = links.remove(&id).unwrap_or_default();
                ExampleHit {
                    en,
                    zh,
                    keys,
                    zh_keys,
                    headwords,
                }
            })
            .collect())
    }

    /// 词级表的来源与逐表统计；老词库返回 `None`。
    pub fn levels_meta(&self) -> Result<Option<crate::data::ingest::LevelsMeta>> {
        let text: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='levels'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match text {
            Some(text) => Some(serde_json::from_str(&text)?),
            None => None,
        })
    }

    /// 一批键的词级信息。一次 `IN` 最多带 `LEVEL_PAGE` 个键，长文才不会把
    /// SQLite 的变量上限问出来；表不存在时返回空表，调用方按“无词级数据”处理。
    pub fn levels(&self, keys: &[String]) -> Result<HashMap<String, WordLevel>> {
        let mut out = HashMap::new();
        if !self.has_levels {
            return Ok(out);
        }
        for chunk in keys.chunks(LEVEL_PAGE) {
            let marks = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT key,level,tags FROM word_level WHERE key IN ({marks})");
            let mut stmt = self.conn.prepare_cached(&sql)?;
            for row in stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<u8>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (key, level, tags) = row?;
                out.insert(
                    key,
                    WordLevel {
                        level,
                        tags: tags
                            .split(',')
                            .filter(|tag| !tag.is_empty())
                            .map(str::to_string)
                            .collect(),
                    },
                );
            }
        }
        Ok(out)
    }

    /// 词形的原形；`aliases` 里一个词形可能挂多个原形，取字典序第一个，
    /// 这样同一份词库每次给出的结果都相同。
    pub fn lemma_of(&self, word: &str) -> Result<Option<String>> {
        let key = normalize(word);
        let mut stmt = self.conn.prepare_cached(
            "SELECT lemma FROM aliases WHERE form=?1 AND lemma<>?1 ORDER BY lemma LIMIT 1",
        )?;
        let lemma: Option<String> = stmt.query_row([key.as_str()], |r| r.get(0)).optional()?;
        Ok(lemma)
    }

    /// 词库里的多词短语键，`hint --include-phrase` 用它避免把一个短语数成一串生词。
    pub fn phrase_keys(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT key FROM entries WHERE instr(key,' ')>0 ORDER BY key",
        )?;
        let keys = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
    }

    pub fn check_integrity(&self) -> Result<()> {
        let mut check = self.conn.prepare("PRAGMA integrity_check")?;
        for result in check.query_map([], |r| r.get::<_, String>(0))? {
            let result = result?;
            ensure!(result == "ok", "词库完整性错误: {result}");
        }
        let mut stmt = self
            .conn
            .prepare("SELECT key,card FROM entries ORDER BY id")?;
        for row in stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })? {
            let (key, bytes) = row?;
            let card = crate::data::compile::decompress_card(&bytes)?;
            ensure!(
                key == normalize(&card.headword) && card.source.is_some(),
                "词卡身份/来源不一致"
            );
        }
        let dangling: usize = self.conn.query_row("SELECT count(*) FROM aliases a WHERE NOT EXISTS(SELECT 1 FROM entries e WHERE e.key=a.lemma)", [], |r| r.get(0))?;
        ensure!(dangling == 0, "存在悬空词形索引");
        // 例句索引是可选表，所以这两项检查也只在它有的时候才做：老词库照常通过。
        if self.has_examples {
            let dangling: usize = self.conn.query_row("SELECT count(*) FROM example_link l WHERE NOT EXISTS(SELECT 1 FROM example e WHERE e.id=l.example_id)", [], |r| r.get(0))?;
            ensure!(dangling == 0, "存在悬空例句索引");
            let unlinked: usize = self.conn.query_row("SELECT count(*) FROM example e WHERE NOT EXISTS(SELECT 1 FROM example_link l WHERE l.example_id=e.id)", [], |r| r.get(0))?;
            ensure!(unlinked == 0, "存在没有任何索引的例句");
        }
        // 删字符索引也一样：补位候选得真有词卡，否则就是把用户引向一个查不到的词。
        if self.has_spell {
            let dangling: usize = self.conn.query_row("SELECT count(*) FROM spell_key s WHERE NOT EXISTS(SELECT 1 FROM entries e WHERE e.key=s.word)", [], |r| r.get(0))?;
            ensure!(dangling == 0, "存在悬空拼写候选索引");
        }
        // `key_evidence` 承诺"每一条卡都知道自己的词头有多少自证"，而 `entries.id`
        // 就是源行号，所以缺一行就是这张表在骗人：要么漏写，要么写给了别的行。
        if self.has_key_evidence {
            let missing: usize = self.conn.query_row("SELECT count(*) FROM entries e WHERE NOT EXISTS(SELECT 1 FROM key_evidence k WHERE k.line=e.id)", [], |r| r.get(0))?;
            ensure!(missing == 0, "存在没有词头证据档位的词卡");
            let bogus: usize = self.conn.query_row(
                "SELECT count(*) FROM key_evidence WHERE tier NOT IN (0,1,2,3)",
                [],
                |r| r.get(0),
            )?;
            ensure!(bogus == 0, "存在无法识别的词头证据档位");
        }
        Ok(())
    }

    pub fn report(&self) -> Result<crate::data::ingest::BuildReport> {
        let text: String =
            self.conn
                .query_row("SELECT value FROM meta WHERE key='report'", [], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&text)?)
    }

    /// 这次构建实际附加的可选资料表；老词库没有这条记录，返回空对象。
    pub fn datasets(&self) -> Result<serde_json::Value> {
        let text: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='datasets'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match text {
            Some(text) => serde_json::from_str(&text)?,
            None => serde_json::json!({}),
        })
    }

    pub fn lookup(&self, word: &str) -> Result<Option<SearchHit>> {
        let key = normalize(word);
        let mut stmt = self
            .conn
            .prepare_cached("SELECT card FROM entries WHERE key=?1 AND priority=(SELECT max(priority) FROM entries WHERE key=?1) ORDER BY id")?;
        let rows = stmt.query_map([&key], |r| r.get::<_, Vec<u8>>(0))?;
        let mut cards = Vec::new();
        for row in rows {
            cards.push(crate::data::compile::decompress_card(&row?)?);
        }
        Ok((!cards.is_empty()).then_some(SearchHit {
            key,
            cards,
            match_type: "exact".into(),
            matched_senses: Vec::new(),
            relations: Vec::new(),
        }))
    }

    /// 用本地词库里的词缀词条给一个单词做构词联想：常见前缀 + 基础词。
    /// 只在前缀、基础词都能在库里找到，并且整词释义能和基础词对上时给提示；
    /// 对不上就安静地不给，不猜、不补外部数据。返回值只用于文本展示，
    /// 不进入 JSON 契约。
    pub fn affix_hints(&self, word: &str, limit: usize) -> Result<Vec<AffixHint>> {
        let key = normalize(word);
        if key.len() < 4 || !key.chars().all(|c| c.is_ascii_lowercase()) {
            return Ok(Vec::new());
        }
        let Some(word_card) = self
            .lookup(&key)?
            .and_then(|hit| hit.cards.into_iter().next())
        else {
            return Ok(Vec::new());
        };
        let word_fragments = card_fragments(&word_card);
        if word_fragments.is_empty() {
            return Ok(Vec::new());
        }
        let word_pos = content_pos_families(&word_card);
        let mut candidates: Vec<(&str, &[&str], String)> = PREFIX_RULES
            .iter()
            .filter_map(|(prefix, markers)| {
                let stem = prefix.trim_end_matches('-');
                (key.len() > stem.len() + 1 && key.starts_with(stem))
                    .then(|| (*prefix, *markers, key[stem.len()..].to_string()))
            })
            .collect();
        // 长前缀优先：inter- 要排在 in- 前面，否则 internet 会被拆成 in- + ternet。
        candidates.sort_by_key(|(prefix, ..)| std::cmp::Reverse(prefix.len()));
        let bases: Vec<String> = candidates.iter().map(|(_, _, base)| base.clone()).collect();
        let levels = self.levels(&bases)?;
        let mut out = Vec::new();
        for (prefix, markers, base) in candidates {
            if base.len() < 3 {
                continue;
            }
            let Some(base_hit) = self.lookup(&base)? else {
                continue;
            };
            let Some(base_card) = base_hit.cards.first() else {
                continue;
            };
            // 只接受有实义词性的基础词；缩写（est、ly）和纯符号不是词根。
            // 词性可能挂在词卡上，也可能只挂在单条义项上，两处都要看。
            let base_pos = content_pos_families(base_card);
            if base_pos.is_empty() {
                continue;
            }
            let base_fragments = card_fragments(base_card);
            if base_fragments.is_empty() {
                continue;
            }
            let Some((base_gloss, strict_contained, equal_contained, shared)) =
                pick_base_gloss(&base_fragments, &word_fragments)
            else {
                continue;
            };
            let marker_hit = word_fragments
                .iter()
                .any(|fragment| markers.iter().any(|marker| fragment.contains(*marker)));
            // 基础词常用程度：带词级表时认 1–5 级；老词库没有表，只能退而求其次，
            // 把 4 个字母以上的基础词当作学习者大概率认识的词。
            let base_common = if self.has_levels {
                levels
                    .get(&base)
                    .and_then(|entry| entry.level)
                    .is_some_and(|level| level <= 5)
            } else {
                base.chars().count() >= 4
            };
            // “标记 + 常见基础词”是最弱的一档证据，再加一道词性检查：
            // 整词至少有一个实义词性和基础词重合，否则 trans- + late 也会蒙混过关。
            let compatible_pos = base_pos.iter().any(|family| word_pos.contains(family));
            let accepted = strict_contained
                || (equal_contained && marker_hit)
                || (marker_hit && shared)
                || (marker_hit && base_common && compatible_pos)
                || (shared && base_common && SHARED_ONLY_PREFIXES.contains(&prefix));
            if !accepted {
                continue;
            }
            let Some(affix_hit) = self.lookup(prefix)? else {
                continue;
            };
            let Some(affix_card) = affix_hit.cards.first() else {
                continue;
            };
            let Some(raw) = first_sense_gloss(affix_card) else {
                continue;
            };
            let meaning = affix_meaning(raw);
            if meaning.is_empty() {
                continue;
            }
            out.push(AffixHint {
                prefix: prefix.to_string(),
                meaning,
                word_gloss: pick_word_gloss(&word_fragments, &base_gloss, markers)
                    .map(|fragment| clip_gloss(&fragment, 24))
                    .unwrap_or_default(),
                base,
                base_gloss: clip_gloss(&base_gloss, 18),
            });
            if out.len() >= limit.clamp(1, 4) {
                break;
            }
        }
        Ok(out)
    }

    fn alias_hits(&self, key: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT lemma,kind FROM aliases WHERE form=?1 ORDER BY lemma,kind")?;
        let mut groups = BTreeMap::<String, Vec<String>>::new();
        for row in stmt.query_map([key], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (lemma, kind) = row?;
            if lemma != key {
                groups.entry(lemma).or_default().push(kind);
            }
        }
        let mut out = Vec::new();
        for (lemma, relations) in groups.into_iter().take(limit) {
            if let Some(mut hit) = self.lookup(&lemma)? {
                hit.match_type = "lemma".into();
                hit.relations = relations;
                out.push(hit);
            }
        }
        Ok(out)
    }

    pub fn prefix(&self, prefix: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let p = normalize(prefix);
        let limit = limit.min(MAX_LIMIT);
        let mut keys = self.prefix_entries(&p, limit)?;
        // 词形表里的屈折形式也算前缀命中：runn 要能补出 running。form 是 aliases
        // 的主键首列，本来就有索引，不需要新表。两侧各自有序，合并后再排一次。
        keys.extend(self.prefix_forms(&p, limit)?);
        keys.sort();
        keys.dedup();
        let mut hits = Vec::new();
        for key in keys.into_iter().take(limit) {
            match self.lookup(&key)? {
                Some(mut hit) => {
                    hit.match_type = "prefix".into();
                    hits.push(hit);
                }
                // 该词形自己没有词卡，就交回它的原形——与直接查 ran 的行为一致。
                None => hits.extend(self.alias_hits(&key, 1)?),
            }
        }
        Ok(hits)
    }

    fn prefix_entries(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let upper = format!("{prefix}\u{10ffff}");
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT key FROM entries WHERE key>=?1 AND key<?2 ORDER BY key LIMIT ?3",
        )?;
        let keys = stmt
            .query_map(params![prefix, upper, limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
    }

    fn prefix_forms(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let upper = format!("{prefix}\u{10ffff}");
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT form FROM aliases WHERE form>=?1 AND form<?2 ORDER BY form LIMIT ?3",
        )?;
        let forms = stmt
            .query_map(params![prefix, upper, limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(forms)
    }

    pub fn contains(&self, needle: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT key FROM entries WHERE instr(key,?1)>0 ORDER BY key LIMIT ?2",
        )?;
        let result = stmt
            .query_map(params![normalize(needle), limit.min(MAX_LIMIT)], |r| {
                r.get(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(result)
    }

    fn hits_for_keys(&self, keys: Vec<String>, kind: &str) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        for key in keys {
            if let Some(mut hit) = self.lookup(&key)? {
                hit.match_type = kind.into();
                hits.push(hit);
            }
        }
        Ok(hits)
    }

    /// 拼写建议。主排序是 Damerau 距离，另外两件事决定它好不好用：
    ///
    /// 1. 同距离先给常见词（`word_level` 带号小的先），带号相同才比长度差和字母序。
    ///    纯字母序会让 `teh` 先吐出 `eth/tea/tec/tee/tel/tem`，把 `the` 挤出十条之外。
    ///    没有词级表的旧词库按"无带号"处理，顺序与今天一致。
    /// 2. 词形表里的屈折形式也算候选：`runing` 离 `running` 一步、离原形 `run` 三步，
    ///    只扫 `entries.key` 就答不出"你想查的可能是 run"。这条通路把原形端上来，命中的
    ///    词形记进 `via`，所以 `distance` 说的是到 `via` 的距离，不是到 `word` 的距离。
    /// 3. 绕词形表来的原形排在同距离的直达词头之后。`worl` 离 `wore` 一步，于是 `wear`
    ///    会以"距离 1"混进来，压掉真正想查的 `world`——词形是二手证据，不该插直达命中队的队。
    /// 4. 上面三条扫完了还没排满，才轮到删字符索引补位（`--spell` 建的表），补的候选
    ///    一律排在已有候选之后。没有这张表时行为与今天逐条一致。
    pub fn fuzzy(&self, word: &str, limit: usize) -> Result<Vec<Suggestion>> {
        let w = normalize(word);
        let n = w.chars().count();
        let max = if n <= 4 { 1 } else { 2 };
        let (low, high) = (n.saturating_sub(max), n + max);
        let mut found: HashMap<String, (usize, Option<String>)> = HashMap::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT key FROM entries WHERE char_len BETWEEN ?1 AND ?2")?;
        for row in stmt.query_map(params![low, high], |r| r.get::<_, String>(0))? {
            let key = row?;
            if let Some(distance) = edit_distance(&w, &key, max) {
                found.insert(key, (distance, None));
            }
        }
        // 只收集，不在迭代词形的同时再准备别的语句。
        let mut reachable: HashMap<String, (usize, String)> = HashMap::new();
        {
            let mut forms = self.conn.prepare_cached(
                "SELECT form, lemma FROM aliases WHERE lemma<>form AND length(form) BETWEEN ?1 AND ?2",
            )?;
            for row in forms.query_map(params![low, high], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (form, lemma) = row?;
                let Some(distance) = edit_distance(&w, &form, max) else {
                    continue;
                };
                // 原形自己就是词头时上面已经按真实距离收过了，不必绕这条路。
                if found.contains_key(&lemma) {
                    continue;
                }
                match reachable.get(&lemma) {
                    Some((best, _)) if *best <= distance => {}
                    _ => {
                        reachable.insert(lemma, (distance, form));
                    }
                }
            }
        }
        let mut has_card = self
            .conn
            .prepare_cached("SELECT 1 FROM entries WHERE key=?1 LIMIT 1")?;
        for (lemma, (distance, form)) in reachable {
            if has_card
                .exists(params![lemma.as_str()])
                .context("查询词头是否存在")?
            {
                found.insert(lemma, (distance, Some(form)));
            }
        }

        let levels = self.levels(&found.keys().cloned().collect::<Vec<_>>())?;
        let mut scored: Vec<_> = found
            .into_iter()
            .map(|(word, (distance, via))| {
                let level = levels
                    .get(&word)
                    .and_then(|found| found.level)
                    .unwrap_or(u8::MAX);
                (
                    distance,
                    usize::from(via.is_some()),
                    level,
                    n.abs_diff(word.chars().count()),
                    word,
                    via,
                )
            })
            .collect();
        scored.sort();
        let cap = limit.min(MAX_LIMIT);
        let mut out: Vec<Suggestion> = scored
            .into_iter()
            .take(cap)
            .map(|(distance, _, _, _, word, via)| Suggestion {
                word,
                distance,
                via,
            })
            .collect();
        // 删字符索引是兜底，不是第二条打分通路：只有穷举扫描没排满才端上来，
        // 而且只接在已排序结果之后。这样它的存在按构造不可能把上面的候选挤掉。
        // 距离放宽一步（`max+1`，上限 3）才有补位价值——`max` 以内扫描本来就扫全了。
        if out.len() < cap && self.has_spell {
            let emitted = out
                .iter()
                .map(|hit| hit.word.clone())
                .collect::<HashSet<_>>();
            out.extend(
                self.spell_fill(&w, max.saturating_add(1).min(3), &emitted)?
                    .into_iter()
                    .take(cap - out.len()),
            );
        }
        Ok(out)
    }

    fn reverse(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // 释义按简体入库，繁体查询先折叠，否则一个字符都对上：词组表优先，命中就整段
        // 跳过，剩下的逐字折单字表。只折查询串，库里任何键都不动。表只有繁→简方向，
        // 简→繁一对多（干←幹／乾）不做。
        let query = crate::core::variant::fold_traditional(query);
        let query = query.as_str();
        let tokens = search_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let phrase = format!("\"{}\"", tokens.replace('"', "\"\""));
        let mut stmt = self.conn.prepare_cached("SELECT e.key,s.gloss FROM sense_search s JOIN entries e ON e.id=s.entry_id WHERE sense_search MATCH ?1 AND e.priority=(SELECT max(p.priority) FROM entries p WHERE p.key=e.key)")?;
        let mut groups = BTreeMap::<String, (usize, usize, Vec<String>)>::new();
        for row in stmt.query_map([phrase], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (key, gloss) = row?;
            let norm = normalize(&gloss);
            if !norm.contains(query) {
                continue;
            }
            let equal_fragment = norm
                .split(['，', ',', ';', '；', '。', '\n'])
                .any(|s| s.trim() == query);
            let rank = (usize::from(!equal_fragment), norm.chars().count());
            let group = groups.entry(key).or_insert((rank.0, rank.1, Vec::new()));
            if rank < (group.0, group.1) {
                group.0 = rank.0;
                group.1 = rank.1;
            }
            if !group.2.contains(&gloss) {
                group.2.push(gloss);
            }
        }
        let mut ranked: Vec<_> = groups.into_iter().collect();
        ranked.sort_by(|a, b| (a.1 .0, a.1 .1, &a.0).cmp(&(b.1 .0, b.1 .1, &b.0)));
        let mut hits = Vec::new();
        for (key, (_, _, senses)) in ranked.into_iter().take(limit) {
            if let Some(mut hit) = self.lookup(&key)? {
                hit.match_type = "reverse".into();
                hit.matched_senses = senses;
                hits.push(hit);
            }
        }
        Ok(hits)
    }

    /// 拼音检索按读音吻合度分四档，再在档内按词头长度、词头排序（单字在前、词组在后）。
    /// 词库没有词频字段，同音词只能按字形稳定排序：
    ///
    /// 1. 查询自带音节分隔且整串音节相等（`xi'an` → 西安，同音的“烯胺/锡安”也在这一档）；
    /// 2. 无声调连写整串相等（`xian` → 仙/先/西安……）；
    /// 3. 查询作为完整音节出现在词的读音里（`an` → 西安，`xian` → 先后）；
    /// 4. 只是连写前缀，音节尚未对上（`xian` → 想，因为 xiang 以 xian 开头）。
    ///
    /// 第 3 档必须排在第 4 档前：否则 `an` 的西安会被成百上千个 an…/xiang…
    /// 前缀词挤到结果末尾，分音节索引等于白建。前两档（同音字）仍排在边界命中
    /// 之前，所以默认 `--limit 8` 看的是最直接的读音，词组命中要更大的 limit。
    fn pinyin(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        ensure!(
            self.has_pinyin,
            "词库没有拼音索引；重建：zedict compile --pinyin --input <源.jsonl> --out <目录>"
        );
        let Some(key) = pinyin_keys(query).into_iter().next() else {
            return Ok(Vec::new());
        };
        let limit = limit.min(MAX_LIMIT);
        let upper = format!("{}\u{10ffff}", key.flat);
        let pattern = format!("% {} %", key.syllables);
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        // 按档位依次取，凑满 `limit` 就停：默认 limit 只碰前两个索引查询，
        // 整音节边界那次全表扫描只在用户把 limit 放大时才发生。
        if key.syllables.contains(' ') {
            let remaining = limit - keys.len();
            self.collect_pinyin(
                "SELECT e.key FROM pinyin p JOIN entries e ON e.id=p.entry_id
                 WHERE p.syllables=?1 GROUP BY e.key
                 ORDER BY length(e.key), e.key LIMIT ?2",
                params![key.syllables, remaining],
                &mut keys,
                &mut seen,
            )?;
        }
        let remaining = limit - keys.len();
        if remaining > 0 {
            self.collect_pinyin(
                "SELECT e.key FROM pinyin p JOIN entries e ON e.id=p.entry_id
                 WHERE p.flat=?1 GROUP BY e.key
                 ORDER BY length(e.key), e.key LIMIT ?2",
                params![key.flat, remaining],
                &mut keys,
                &mut seen,
            )?;
        }
        let remaining = limit - keys.len();
        if remaining > 0 {
            self.collect_pinyin(
                "SELECT e.key FROM pinyin p JOIN entries e ON e.id=p.entry_id
                 WHERE (' '||p.syllables||' ') LIKE ?1 GROUP BY e.key
                 ORDER BY length(e.key), e.key LIMIT ?2",
                params![pattern, remaining],
                &mut keys,
                &mut seen,
            )?;
        }
        let remaining = limit - keys.len();
        if remaining > 0 {
            self.collect_pinyin(
                "SELECT e.key FROM pinyin p JOIN entries e ON e.id=p.entry_id
                 WHERE p.flat>=?1 AND p.flat<?2 GROUP BY e.key
                 ORDER BY length(e.key), e.key LIMIT ?3",
                params![key.flat, upper, remaining],
                &mut keys,
                &mut seen,
            )?;
        }
        let mut hits = Vec::new();
        for key in keys {
            if let Some(mut hit) = self.lookup(&key)? {
                hit.match_type = "pinyin".into();
                hits.push(hit);
            }
        }
        Ok(hits)
    }

    fn collect_pinyin<P: rusqlite::Params>(
        &self,
        sql: &str,
        params: P,
        keys: &mut Vec<String>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
        for row in rows {
            let word = row?;
            if seen.insert(word.clone()) {
                keys.push(word);
            }
        }
        Ok(())
    }

    pub fn query(&self, request: &QueryRequest) -> Result<QueryResponse> {
        let key = normalize(&request.query);
        ensure!(
            !key.is_empty() && key.chars().count() <= MAX_QUERY_CHARS,
            "查询须为 1–256 个字符"
        );
        ensure!(!key.chars().any(char::is_control), "查询不能包含控制字符");
        ensure!(
            (1..=MAX_LIMIT).contains(&request.limit),
            "limit 须为 1–1000"
        );
        let exact = if request.mode == SearchMode::Auto {
            self.lookup(&key)?
        } else {
            None
        };
        let mode = if request.mode == SearchMode::Auto && has_han(&key) && exact.is_none() {
            SearchMode::Reverse
        } else {
            request.mode
        };
        let mut suggestions = Vec::new();
        let hits = match mode {
            SearchMode::Exact => self.lookup(&key)?.into_iter().collect(),
            SearchMode::Auto => {
                let mut hits: Vec<_> = exact.into_iter().collect();
                if has_han(&key) {
                    let remaining = request.limit.saturating_sub(hits.len());
                    if remaining > 0 {
                        for hit in self.reverse(&key, remaining)? {
                            if hits.iter().all(|seen| seen.key != hit.key) {
                                hits.push(hit);
                            }
                        }
                    }
                } else {
                    hits.extend(self.alias_hits(&key, request.limit.saturating_sub(hits.len()))?);
                    // 拼音回退只在没有英文词首候选时触发（带调号/数字调的查询
                    // 除外）：`an` 该先当英文前缀，`xiàn` 则明确是拼音。
                    if hits.is_empty() && self.has_pinyin && is_pinyin_like(&key) {
                        let marked = key.chars().any(|c| !c.is_ascii() || c.is_ascii_digit());
                        if marked || self.prefix_entries(&key, 1)?.is_empty() {
                            hits = self.pinyin(&key, request.limit)?;
                        }
                    }
                    if hits.is_empty() {
                        suggestions = self.fuzzy(&key, 5)?;
                    }
                }
                hits
            }
            SearchMode::Prefix => self.prefix(&key, request.limit)?,
            SearchMode::Contains => {
                self.hits_for_keys(self.contains(&key, request.limit)?, "contains")?
            }
            SearchMode::Fuzzy => {
                suggestions = self.fuzzy(&key, request.limit)?;
                Vec::new()
            }
            SearchMode::Reverse => self.reverse(&key, request.limit)?,
            SearchMode::Pinyin => self.pinyin(&key, request.limit)?,
        };
        let status = if !hits.is_empty() {
            "found"
        } else if mode == SearchMode::Fuzzy && !suggestions.is_empty() {
            "suggestions"
        } else {
            "not_found"
        };
        Ok(QueryResponse {
            schema_version: 1,
            query: request.query.clone(),
            normalized_query: key,
            status: status.into(),
            hits,
            suggestions,
        })
    }

    pub fn words(&self, offset: usize, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT key FROM entries ORDER BY key LIMIT ?1 OFFSET ?2")?;
        let rows = stmt
            .query_map(params![limit.min(MAX_LIMIT), i64::try_from(offset)?], |r| {
                r.get(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn random(&self, n: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT key FROM (SELECT DISTINCT key FROM entries) ORDER BY random() LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([n.min(MAX_LIMIT)], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// Banded optimal-string-alignment Damerau-Levenshtein, with adjacent transpositions.
pub fn edit_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<_> = a.chars().collect();
    let b: Vec<_> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let inf = max + 1;
    let mut prev2 = vec![inf; b.len() + 1];
    let mut prev: Vec<_> = (0..=b.len()).map(|v| v.min(inf)).collect();
    for i in 1..=a.len() {
        let mut curr = vec![inf; b.len() + 1];
        curr[0] = i.min(inf);
        for j in i.saturating_sub(max).max(1)..=b.len().min(i + max) {
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                curr[j] = curr[j].min(prev2[j - 2] + 1);
            }
        }
        prev2 = prev;
        prev = curr;
    }
    (prev[b.len()] <= max).then_some(prev[b.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transpositions_first_letter_and_normalization() {
        assert_eq!(edit_distance("world", "wrold", 2), Some(1));
        assert_eq!(edit_distance("world", "xorld", 2), Some(1));
        assert_eq!(edit_distance("abc", "xyz", 2), None);
        assert_eq!(normalize("  Cafe\u{301}  DON’T "), "café don't");
    }

    #[test]
    fn pinyin_folds_tones_digits_and_umlaut() {
        assert_eq!(
            pinyin_keys("xiàn"),
            vec![PinyinKey {
                flat: "xian".into(),
                syllables: "xian".into()
            }]
        );
        assert_eq!(
            pinyin_keys("jun1").first().map(|k| k.flat.as_str()),
            Some("jun")
        );
        assert_eq!(
            pinyin_keys("nǚ").first().map(|k| k.flat.as_str()),
            Some("nv")
        );
        assert_eq!(
            pinyin_keys("xi'an").first().map(|k| k.syllables.as_str()),
            Some("xi an")
        );
        // 多音字写成多个变体；标点和下划线只是音节边界。
        let readings = pinyin_keys("shé，zhé；zhē");
        assert_eq!(
            readings.iter().map(|k| k.flat.as_str()).collect::<Vec<_>>(),
            ["she", "zhe"]
        );
        assert_eq!(
            pinyin_keys("dǎ _ bái tiáo _")
                .first()
                .map(|k| k.flat.as_str()),
            Some("dabaitiao")
        );
    }

    #[test]
    fn affix_meanings_prefer_the_quoted_core() {
        assert_eq!(affix_meaning("pref. 表示“不”"), "不");
        assert_eq!(affix_meaning("pref. 表示“再；又”：return"), "再；又");
        assert_eq!(affix_meaning("comb. 共同"), "共同");
        assert_eq!(affix_meaning("表示“过度”之义"), "过度");
    }

    #[test]
    fn brief_gloss_cuts_at_the_first_sense_boundary() {
        assert_eq!(brief_gloss("不高兴的；生气的", 18), "不高兴的");
        assert_eq!(brief_gloss("（使）转动，旋转", 18), "转动");
        assert_eq!(brief_gloss("(使)变亮，（使）明亮", 18), "变亮");
        assert_eq!(
            brief_gloss("a long gloss, with a comma", 40),
            "a long gloss"
        );
        assert_eq!(
            brief_gloss("这是一个非常非常非常长的释义超过上限", 6),
            "这是一个非常…"
        );
    }

    #[test]
    fn gloss_fragments_drop_usage_marks_and_empty_words() {
        assert_eq!(
            gloss_fragments("（使）转动，旋转；的"),
            vec!["转动", "旋转"]
        );
        assert_eq!(gloss_fragments("的；了"), Vec::<String>::new());
        // “的”是虚字，不参与共享字统计。
        assert_eq!(shared_chars("高兴的", "不高兴的"), 2);
        assert_eq!(shared_chars("高兴的", "生气的"), 0);
    }
}
