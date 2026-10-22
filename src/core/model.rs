//! 自有词典数据模型 —— 与欧陆原始格式完全解耦。
//!
//! 所有字段均为清洗后的结构化数据；原始 HTML 仅在 `raw_html` 可选保留。

use serde::{Deserialize, Serialize};

/// 词性（自有序列化，小写英文码）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartOfSpeech {
    Noun,
    Verb,
    TransitiveVerb,
    IntransitiveVerb,
    AmbitransitiveVerb,
    Adjective,
    Adverb,
    Preposition,
    Pronoun,
    Conjunction,
    Interjection,
    Article,
    Numeral,
    Auxiliary,
    Abbreviation,
}

impl PartOfSpeech {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim_matches('.').to_ascii_lowercase().as_str() {
            "n" => Self::Noun,
            "v" => Self::Verb,
            "vt" => Self::TransitiveVerb,
            "vi" => Self::IntransitiveVerb,
            "vt. & vi" => Self::AmbitransitiveVerb,
            "adj" => Self::Adjective,
            "adv" => Self::Adverb,
            "prep" => Self::Preposition,
            "pron" => Self::Pronoun,
            "conj" => Self::Conjunction,
            "interj" | "int" => Self::Interjection,
            "art" => Self::Article,
            "num" => Self::Numeral,
            "aux" => Self::Auxiliary,
            "abbr" => Self::Abbreviation,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Noun => "n.",
            Self::Verb => "v.",
            Self::TransitiveVerb => "vt.",
            Self::IntransitiveVerb => "vi.",
            Self::AmbitransitiveVerb => "vt. & vi.",
            Self::Adjective => "adj.",
            Self::Adverb => "adv.",
            Self::Preposition => "prep.",
            Self::Pronoun => "pron.",
            Self::Conjunction => "conj.",
            Self::Interjection => "interj.",
            Self::Article => "art.",
            Self::Numeral => "num.",
            Self::Auxiliary => "aux.",
            Self::Abbreviation => "abbr.",
        }
    }

    #[allow(dead_code)]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Noun => "n",
            Self::Verb => "v",
            Self::TransitiveVerb => "vt",
            Self::IntransitiveVerb => "vi",
            Self::AmbitransitiveVerb => "vt_vi",
            Self::Adjective => "adj",
            Self::Adverb => "adv",
            Self::Preposition => "prep",
            Self::Pronoun => "pron",
            Self::Conjunction => "conj",
            Self::Interjection => "interj",
            Self::Article => "art",
            Self::Numeral => "num",
            Self::Auxiliary => "aux",
            Self::Abbreviation => "abbr",
        }
    }
}

/// 用法 / 领域标签
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub kind: LabelKind,
    pub code: String, // 如 "informal", "chemistry"
    pub zh: String,   // 如 "口", "化"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelKind {
    Usage,  // 语域: informal/slang/archaic...
    Domain, // 领域: chemistry/medicine...
}

/// 词形变化
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordForm {
    pub kind: FormKind,
    pub words: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormKind {
    PastTense, // 时态
    PastParticiple,
    PresentParticiple,
    ThirdPerson,
    Plural,         // 复数
    Comparative,    // 比较级
    Superlative,    // 最高级
    NounForm,       // 名词形式
    AdjectiveForm,  // 形容词形式
    AdverbForm,     // 副词形式
    VerbForm,       // 动词形式
    AbbreviationOf, // 缩写
    Other,
}

impl FormKind {
    /// 从欧陆复述标签 ("时态"/"名词"/"比较级"...) 解析
    pub fn parse(label: &str) -> Self {
        match label.replace(' ', "").as_str() {
            "过去式" => Self::PastTense,
            "过去分词" => Self::PastParticiple,
            "现在分词" => Self::PresentParticiple,
            "第三人称单数" => Self::ThirdPerson,
            "复数" => Self::Plural,
            "比较级" => Self::Comparative,
            "最高级" => Self::Superlative,
            "名词" => Self::NounForm,
            "形容词" => Self::AdjectiveForm,
            "副词" => Self::AdverbForm,
            "动词" => Self::VerbForm,
            "缩写" => Self::AbbreviationOf,
            _ => Self::Other,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PastTense => "过去式",
            Self::PastParticiple => "过去分词",
            Self::PresentParticiple => "现在分词",
            Self::ThirdPerson => "第三人称单数",
            Self::Plural => "复数",
            Self::Comparative => "比较级",
            Self::Superlative => "最高级",
            Self::NounForm => "名词形式",
            Self::AdjectiveForm => "形容词形式",
            Self::AdverbForm => "副词形式",
            Self::VerbForm => "动词形式",
            Self::AbbreviationOf => "缩写",
            Self::Other => "其他",
        }
    }
}

/// 单个义项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sense {
    pub pos: Option<PartOfSpeech>,
    /// 中文释义 (纯文本)
    pub gloss: String,
    /// 用法/领域标签
    pub labels: Vec<Label>,
    /// 保留的富文本 (可选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
}

/// 关系词 (近义 / 反义 / 联想)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RelationWire")]
pub struct Relation {
    pub word: String,
    /// 构建时从 `--related` 释义表补上的中文; 表里没有就是 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gloss: Option<String>,
}

/// 一条构词提示：常见前缀 + 基础词。释义、基础词词义都取自本地词库，
/// 只用于联想记忆，不冒充严格词源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffixHint {
    /// 前缀本身，含结尾连字符，如 `un-`
    pub prefix: String,
    /// 前缀的简短含义，如 `不`
    pub meaning: String,
    /// 去掉前缀后剩下的基础词，如 `happy`
    pub base: String,
    /// 基础词的简短词义，如 `高兴的`
    pub base_gloss: String,
    /// 整词的简短词义，如 `不高兴的`
    pub word_gloss: String,
}

/// 老词卡里的近义词只是一个裸字符串, 仍然能读成同一个形状。
#[derive(Deserialize)]
#[serde(untagged)]
enum RelationWire {
    Bare(String),
    Full {
        word: String,
        #[serde(default)]
        gloss: Option<String>,
    },
}

impl From<RelationWire> for Relation {
    fn from(wire: RelationWire) -> Self {
        match wire {
            RelationWire::Bare(word) => Relation { word, gloss: None },
            RelationWire::Full { word, gloss } => Relation { word, gloss },
        }
    }
}

/// 双语对齐例句
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Example {
    pub en: String,
    pub zh: String,
    /// 高亮关键词 (与 headword 对齐)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub keys: Vec<String>,
}

/// 音标 + 音节
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Phonetic {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us: Option<String>,
    /// 不分英美时欧陆只给一条通用音标 (`<en>`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    /// 音节切分 ex·ten·si·ble (规则推导)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub syllables: Option<Vec<String>>,
}

/// 重音位置 (从 IPA 提取, 预留)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stress {
    Primary,
    Secondary,
}

/// ═══════════ 核心模型: 词卡 ═══════════
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
    /// 自有递增 ID
    pub id: u32,
    pub headword: String,
    /// 原形 (running → run; 头词为屈折形式时填写)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lemma: Option<String>,
    /// 中文词条的拼音 (欧陆 `<div class="PY">`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinyin: Option<String>,
    #[serde(skip_serializing_if = "Phonetic::is_empty", default)]
    pub phonetic: Phonetic,
    /// 词性去重列表 (address → [n, v])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pos: Vec<PartOfSpeech>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub senses: Vec<Sense>,
    /// 词条级英英释义
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gloss_en: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forms: Vec<WordForm>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub antonyms: Vec<Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Relation>,
    /// 词条内短语 (`<SPEC>`)，如 "in the long run（长远；终究）"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Example>,
    /// 词条级标签 (聚合自 senses)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub source_sha256: String,
    pub record_sha256: String,
    pub line: usize,
    pub offset: Option<u64>,
    pub reviewer: String,
    pub evidence: String,
}

impl Phonetic {
    fn is_empty(&self) -> bool {
        self.uk.is_none() && self.us.is_none() && self.en.is_none() && self.syllables.is_none()
    }
}

// ─────────── 标签解析表 ───────────
const USAGE_MAP: &[(&str, &str)] = &[
    ("口", "informal"),
    ("俚", "slang"),
    ("谚", "proverb"),
    ("古", "archaic"),
    ("诗", "poetic"),
    ("方", "dialect"),
    ("谑", "humorous"),
    ("贬", "derogatory"),
    ("褒", "commendatory"),
    ("婉", "euphemism"),
    ("书", "literary"),
    ("罕", "rare"),
    ("美", "us"),
    ("英", "uk"),
];
const DOMAIN_MAP: &[(&str, &str)] = &[
    ("化", "chemistry"),
    ("医", "medicine"),
    ("生物", "biology"),
    ("计算", "computing"),
    ("物理", "physics"),
    ("法律", "law"),
    ("经济", "economics"),
    ("军事", "military"),
    ("音乐", "music"),
    ("体育", "sports"),
    ("天文", "astronomy"),
    ("地理", "geography"),
    ("宗教", "religion"),
    ("商业", "business"),
    ("电", "electricity"),
    ("机械", "machinery"),
    ("数", "mathematics"),
    ("海", "marine"),
    ("航空", "aviation"),
    ("动物", "zoology"),
    ("植物", "botany"),
    ("建筑", "architecture"),
    ("纺织", "textile"),
    ("农业", "agriculture"),
    ("心理", "psychology"),
    ("语言", "linguistics"),
    ("印刷", "printing"),
    ("冶金", "metallurgy"),
];

/// 从释义 HTML 提取 〈用法〉 与 [领域] 标签
pub fn extract_labels(html: &str) -> Vec<Label> {
    let mut out = Vec::new();
    for (zh, code) in USAGE_MAP {
        if html.contains(&format!("〈{zh}〉")) {
            out.push(Label {
                kind: LabelKind::Usage,
                code: code.to_string(),
                zh: zh.to_string(),
            });
        }
    }
    for (zh, code) in DOMAIN_MAP {
        if html.contains(&format!("[{zh}]")) {
            out.push(Label {
                kind: LabelKind::Domain,
                code: code.to_string(),
                zh: zh.to_string(),
            });
        }
    }
    out
}

/// 剥离 HTML 标签为纯文本
pub fn strip_html(s: &str) -> String {
    static BLOCKS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)</?(?:p|div|li|ul|ol|br|h[1-6])\b[^>]*>").expect("constant regex")
    });
    let separated = BLOCKS.replace_all(s, " ");
    let mut out = String::new();
    let mut in_tag = false;
    for c in separated.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_escape::decode_html_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 简易英文音节切分 (maximal-onset 近似, best-effort)
pub fn syllabify(word: &str) -> Option<Vec<String>> {
    let w = word.trim();
    if w.is_empty()
        || !w
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '\'')
    {
        return None;
    }
    let lower = w.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    if chars.len() < 3 {
        return Some(vec![w.to_string()]);
    }
    let is_vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
    let mut cuts: Vec<usize> = Vec::new();
    let n = chars.len();
    let mut i = 1;
    while i < n - 1 {
        if is_vowel(chars[i]) && !is_vowel(chars[i - 1]) {
            // 元音后找辅音簇的切点
            let mut j = i + 1;
            while j < n && !is_vowel(chars[j]) {
                j += 1;
            }
            if j < n && j - i > 1 {
                // 辅音簇: 单辅音归下一音节, 双辅音中间切 (le/x 不切)
                let cut = if j - i == 2 && !matches!(chars[i + 1], 'l' | 'x') {
                    i + 1
                } else if j - i > 2 {
                    j - 1
                } else {
                    j
                };
                if cut > 0 && cut < n && !cuts.contains(&cut) {
                    cuts.push(cut);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    if cuts.is_empty() {
        return None;
    }
    cuts.sort_unstable();
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0usize;
    for c in cuts {
        if c > start {
            parts.push(&w[start..c]);
            start = c;
        }
    }
    if start < n {
        parts.push(&w[start..]);
    }
    if parts.len() >= 2 && parts.iter().all(|p| !p.is_empty()) {
        Some(parts.into_iter().map(|s| s.to_string()).collect())
    } else {
        None
    }
}
