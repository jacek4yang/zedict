//! Explicit, source-bound reviews. Guessed headwords are never admission evidence.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<()> {
    ensure!(
        !cancel.is_some_and(|c| c.load(Ordering::Relaxed)),
        "操作已取消；未替换正式词库"
    );
    Ok(())
}

use crate::core::evidence::{self, KeyEvidence, Tier};
use crate::core::model::{SourceRef, WordCard};
use crate::data::store::{normalize, DictStore, APPLICATION_ID, MAX_QUERY_CHARS, SCHEMA_VERSION};

// The existing extraction's final unresolved block is ~13 MiB. It must remain
// auditable without relaxing the separate 8 MiB limit on admitted word cards.
pub const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn read_record(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    let n = reader
        .take((MAX_RECORD_BYTES + 1) as u64)
        .read_until(b'\n', &mut buf)?;
    ensure!(n <= MAX_RECORD_BYTES, "原始记录超过 32 MiB");
    if n == 0 {
        return Ok(None);
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(Some(buf))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStatus {
    Verified,
    Unresolved,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AliasReview {
    pub form: String,
    pub kind: String,
    pub evidence: String,
}

/// One row of a generated inflection table (`tools/inflect_aliases.py`), which
/// relates a surface form to a lemma carried elsewhere in the library.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inflection {
    pub form: String,
    pub lemma: String,
    pub kind: String,
    pub evidence: String,
}

// The relations a word card may report as forms of its own lemma. Derivation is
// absent on purpose: "happiness" is not a form of "happy".
pub const INFLECTION_KINDS: [&str; 7] = [
    "past",
    "past_participle",
    "present_participle",
    "third_person",
    "plural",
    "comparative",
    "superlative",
];

fn validate_alias(form: &str, kind: &str, evidence: &str) -> Result<()> {
    ensure!(
        INFLECTION_KINDS.contains(&kind),
        "非法屈折关系 {}，派生关系不能作为原形",
        kind
    );
    ensure!(
        !form.is_empty() && form.chars().count() <= 256 && !evidence.trim().is_empty(),
        "词形必须包含有效词形和证据"
    );
    Ok(())
}

fn insert_alias(
    tx: &rusqlite::Transaction<'_>,
    form: &str,
    lemma: &str,
    kind: &str,
    evidence: &str,
) -> Result<usize> {
    validate_alias(form, kind, evidence)?;
    tx.execute(
        "INSERT OR IGNORE INTO aliases VALUES (?1,?2,?3,?4)",
        rusqlite::params![form, lemma, kind, evidence],
    )
    .map_err(Into::into)
}

/// 一个中文关键词挂满这么多例句就是噪声（"方法""问题"这类），不写进索引。
const NOISY_KEY_EXAMPLES: usize = 300;

/// 把这张卡的句对（跨卡去重）和它的关键词链写进两张例句侧表。
///
/// `sides` 必须与 `card.examples` 一一对应。两份结果是同一趟扫描、同一个过滤条件
/// 生成的，对不上就说明两个解析器分叉了——宁可构建失败，也不留下指向别句的索引。
fn index_examples(
    tx: &rusqlite::Transaction<'_>,
    headword: &str,
    card: &WordCard,
    sides: &[crate::data::compile::Sides],
    ids: &mut HashMap<(String, String), i64>,
) -> Result<()> {
    ensure!(
        sides.len() == card.examples.len(),
        "例句索引与词卡条数不一致：{}",
        card.headword
    );
    for (example, side) in card.examples.iter().zip(sides) {
        ensure!(
            side.en == example.en && side.zh == example.zh,
            "例句索引与词卡内容不一致：{}",
            card.headword
        );
        let next = ids.len() as i64 + 1;
        let id = match ids.entry((example.en.clone(), example.zh.clone())) {
            Entry::Occupied(existing) => *existing.get(),
            Entry::Vacant(vacant) => {
                tx.execute(
                    "INSERT INTO example VALUES (?1,?2,?3)",
                    rusqlite::params![next, vacant.key().0, vacant.key().1],
                )?;
                vacant.insert(next);
                next
            }
        };
        // side 0=英文关键词、1=中文关键词、2=词头自己。第三类让 `--full` 说得出这句
        // 挂在哪些卡下，前两类才是例句库能按中文或按短语反查的原因。
        for (word, kind) in side
            .en_keys
            .iter()
            .map(|word| (word, 0))
            .chain(side.zh_keys.iter().map(|word| (word, 1)))
        {
            let link = normalize(word);
            if link.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO example_link VALUES (?1,?2,?3)",
                rusqlite::params![id, link, kind],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO example_link VALUES (?1,?2,2)",
            rusqlite::params![id, headword],
        )?;
    }
    Ok(())
}

pub fn load_inflections(path: &Path) -> Result<Vec<Inflection>> {
    let file = File::open(path).context("读取屈折词形表")?;
    let mut rows = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        rows.push(
            serde_json::from_str(&line)
                .with_context(|| format!("屈折词形表第 {} 行无效", index + 1))?,
        );
    }
    Ok(rows)
}

/// 词级表里一份频率分带的自述：等级、原名、说明和源表条目数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelBand {
    pub level: u8,
    pub name: String,
    pub desc: String,
    pub color: String,
    pub items: usize,
}

/// 词级表里一份大纲词表的自述，如 `cet4` / 四级词汇。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelTag {
    pub tag: String,
    pub name: String,
    pub desc: String,
    pub color: String,
    pub items: usize,
}

/// 词级表第一行：等级和大纲词表的名字、说明，以及生成它的那份源文件。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelsMeta {
    pub kind: String,
    pub source: serde_json::Value,
    pub bands: Vec<LevelBand>,
    pub tags: Vec<LevelTag>,
}

/// One row of the generated word-level table (`tools/word_levels.py`). The level
/// is per *word form*, not per lemma, because that is how the source lists them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelRow {
    pub kind: String,
    pub word: String,
    /// `None` = the form is only in a syllabus list, which carries no frequency band.
    pub level: Option<u8>,
    pub tags: Vec<String>,
}

pub struct Levels {
    pub meta: LevelsMeta,
    pub rows: Vec<LevelRow>,
}

/// Read `tools/word_levels.py`'s output: one `meta` line, then one `word` line
/// per distinct form. Everything is admitted-or-rejected, never repaired: a file
/// that fails these checks is not the generator's output, and quietly dropping
/// rows from it would make the library's counts meaningless.
pub fn load_levels(path: &Path) -> Result<Levels> {
    let file = File::open(path).context("读取词级表")?;
    let mut lines = BufReader::new(file).lines();
    let meta: LevelsMeta = match lines.next() {
        Some(line) => serde_json::from_str(&line?)
            .context("词级表第一行不是 meta 记录；重新运行 tools/word_levels.py")?,
        None => bail!("词级表是空文件"),
    };
    ensure!(meta.kind == "meta", "词级表第一行的 kind 不是 meta");
    ensure!(
        !meta.bands.is_empty() && !meta.tags.is_empty(),
        "词级表的 bands/tags 是空的"
    );
    let mut tag_names = HashSet::new();
    let mut max_level = 0u8;
    for band in &meta.bands {
        ensure!(band.level > 0, "词级表里出现了 0 级；等级从 1 开始");
        max_level = max_level.max(band.level);
    }
    for tag in &meta.tags {
        let name = tag.tag.as_str();
        ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "大纲词表名字 {name} 含非法字符"
        );
        ensure!(
            tag_names.insert(name.to_string()),
            "大纲词表名字重复: {name}"
        );
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in lines.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let number = index + 2;
        let mut row: LevelRow =
            serde_json::from_str(&line).with_context(|| format!("词级表第 {number} 行无效"))?;
        ensure!(row.kind == "word", "词级表第 {number} 行的 kind 不是 word");
        if let Some(level) = row.level {
            ensure!(
                (1..=max_level).contains(&level),
                "词级表第 {number} 行等级 {level} 不在 1–{max_level}"
            );
        }
        for tag in &row.tags {
            ensure!(
                tag_names.contains(tag.as_str()),
                "词级表第 {number} 行引用了不存在的大纲词表 {tag}"
            );
        }
        // 存进去的键一定是 normalize 后的形状：库里其它表都是这么建的。
        let key = normalize(&row.word);
        ensure!(
            !key.is_empty() && key.chars().count() <= 256,
            "词级表第 {number} 行的词无效"
        );
        ensure!(
            seen.insert(key.clone()),
            "词级表第 {number} 行的词规范化后重复: {key}"
        );
        row.word = key;
        rows.push(row);
    }
    Ok(Levels { meta, rows })
}

/// 词级表里的 `tags` 数组按原顺序拼成一个字符串存进可选表。
fn level_tags(tags: &[String]) -> String {
    tags.join(",")
}

/// 构建时可以附加的资料表。缺省（`Extras::default()`）必须产出与不带这些标志
/// 时完全相同的词库，所以每一项都是"只补信息，不增删词条"。
#[derive(Default)]
pub struct Extras<'a> {
    /// `tools/inflect_aliases.py` 生成的屈折词形表：ran/running → run
    pub inflections: Option<&'a Path>,
    /// `词<TAB>中文`，给近义/反义/联想词补中文释义
    pub related: Option<&'a Path>,
    /// `tools/word_levels.py` 生成的词级表：每个词形的难度等级和大纲归属
    pub levels: Option<&'a Path>,
    /// 从本次输入的词卡例句里派生例句索引，供 `zedict examples` 检索。
    /// 布尔开关而不是可选文件：数据本来就在已经读的那一趟里，没有新来源可哈希。
    pub examples: bool,
    /// 派生 delete-1 邻域索引，给 `query --mode fuzzy` 兜底补位。同样是布尔开关：
    /// 词头本来就在库里，只是换个形状存。
    pub spell: bool,
    /// 从中文词卡的 `pinyin` 派生无声调前缀索引，供 `query --mode pinyin` 用。
    /// 同样是布尔开关：拼音本来就在词卡里，没有新来源可哈希。
    pub pinyin: bool,
    /// 词头证据阈值。`All` 是今天的行为：一个词头都不丢，也不建 `key_evidence` 表。
    /// `Some`/`Strong` 会在写进 `entries` **之前**丢掉自证不足的词头，因为那些词头
    /// 是错的而不只是"没把握"——用户看到的后果是查到错卡，不是查不到。
    /// 注意这不影响 `source_records`：原始记录在任何阈值下都全量归档。
    pub key_evidence: KeyEvidence,
    /// 参考词表（一行一个英文词的纯文本）。它只用来回答"查不到的词是不是常见
    /// 英文词"，所以它是本次构建的一个**输入文件**，走和 `--levels` 一样的哈希形状。
    pub vocab: Option<&'a Path>,
}

/// `Extras` 读一遍之后的结果：数据本体加上写进 `meta('datasets')` 的来源。
struct Aux {
    inflections: Vec<Inflection>,
    glosses: Option<crate::data::compile::RelationGlosses>,
    levels: Option<Levels>,
    vocab: Option<Vec<String>>,
    datasets: serde_json::Value,
}

impl Extras<'_> {
    fn load(&self) -> Result<Aux> {
        let mut datasets = serde_json::Map::new();
        let mut inflections = Vec::new();
        if let Some(path) = self.inflections {
            inflections = load_inflections(path)?;
            datasets.insert(
                "inflections".to_string(),
                dataset(path, inflections.len(), None)?,
            );
        }
        let mut glosses = None;
        if let Some(path) = self.related {
            let table = crate::data::compile::RelationGlosses::load(path)?;
            let keys = table.keys();
            datasets.insert(
                "related".to_string(),
                dataset(path, table.rows, Some(keys))?,
            );
            glosses = Some(table);
        }
        let mut levels = None;
        if let Some(path) = self.levels {
            let table = load_levels(path)?;
            // rows 已经是不同的词形数；等级/大纲的个数记在 meta('levels') 里。
            datasets.insert("levels".to_string(), dataset(path, table.rows.len(), None)?);
            levels = Some(table);
        }
        let mut vocab = None;
        if let Some(path) = self.vocab {
            let words = load_vocab(path)?;
            datasets.insert("vocab".to_string(), dataset(path, words.len(), None)?);
            vocab = Some(words);
        }
        Ok(Aux {
            inflections,
            glosses,
            levels,
            vocab,
            datasets: serde_json::Value::Object(datasets),
        })
    }
}

/// 参考词表：一行一个英文词的纯文本（`data/spell_words.txt`，Hunspell 派生）。
///
/// 这张表只回答"这是不是一个常见英文词"，所以读得很宽松：大小写、词形和重复行
/// 都不该让一次构建失败，归一化去重就够了。唯一会失败的是"一个词都没有"——那是
/// 给错了路径，而不是词表本来如此。
pub fn load_vocab(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).context("读取参考词表")?;
    let mut words = Vec::new();
    let mut seen = HashSet::new();
    for line in BufReader::new(file).lines() {
        let key = normalize(&line?);
        if key.is_empty() || key.chars().count() > MAX_QUERY_CHARS || !seen.insert(key.clone()) {
            continue;
        }
        words.push(key);
    }
    ensure!(
        !words.is_empty(),
        "参考词表 {} 里一个词都没有",
        path.display()
    );
    Ok(words)
}

/// 源 JSON 数组里的字符串项；非字符串项跳过。词头分级要在建卡之前从原始字段算，
/// 而 `serde_json::from_value` 会把整条 `value` 消费掉，所以这里先克隆出来。
fn string_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 一张可选表在这次构建里到底是什么：路径、内容哈希、行数（`keys` 只有释义表有）。
fn dataset(path: &Path, rows: usize, keys: Option<usize>) -> Result<serde_json::Value> {
    let mut value = serde_json::json!({
        "path": path.display().to_string(),
        "sha256": hash_file(path)?,
        "rows": rows,
    });
    if let Some(keys) = keys {
        value["keys"] = serde_json::json!(keys);
    }
    Ok(value)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub line: usize,
    pub record_sha256: String,
    pub status: ReviewStatus,
    #[serde(default)]
    pub headword: String,
    #[serde(default)]
    pub reviewer: String,
    #[serde(default)]
    pub evidence: String,
    // Explicit field selection is intentional: adjacent entries may be merged in the raw JSONL.
    #[serde(default)]
    pub senses: Vec<usize>,
    #[serde(default)]
    pub examples: Vec<usize>,
    #[serde(default)]
    pub keep_english: bool,
    #[serde(default)]
    pub keep_phonetics: bool,
    #[serde(default)]
    pub keep_forms: bool,
    #[serde(default)]
    pub keep_relations: bool,
    #[serde(default)]
    pub aliases: Vec<AliasReview>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub source_sha256: String,
    pub data_version: String,
    pub reviews: Vec<Review>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct BuildReport {
    pub source_sha256: String,
    pub data_version: String,
    pub raw_records: usize,
    pub verified_cards: usize,
    pub verified_headwords: usize,
    pub rejected_records: usize,
    pub unresolved_records: usize,
    pub aliases: usize,
    pub senses: usize,
    #[serde(default)]
    pub import_mode: String,
    #[serde(default)]
    pub indexed_cards: usize,
    #[serde(default)]
    pub indexed_headwords: usize,
    #[serde(default)]
    pub missing_headword_records: usize,
    #[serde(default)]
    pub invalid_headword_records: usize,
    #[serde(default)]
    pub archived_records: usize,
    #[serde(default)]
    pub inflections: usize,
    #[serde(default)]
    pub inflections_unmatched: usize,
    /// 补上中文释义的近义/反义/联想词条数（只在 `--related` 时非零）
    #[serde(default)]
    pub related_glosses: usize,
    /// 写进可选表 `word_level` 的词形数（只在 `--levels` 时非零）
    #[serde(default)]
    pub word_levels: usize,
    /// 去重后的句对数（只在 `--examples` 时非零）
    #[serde(default)]
    pub examples: usize,
    /// 去噪后写进 `example_link` 的链数
    #[serde(default)]
    pub example_links: usize,
    /// 因为挂太多例句而被剔除的中文关键词个数（静默清洗，必须可见）
    #[serde(default)]
    pub dropped_noisy_keys: usize,
    /// 写进 `spell_key` 的删字符邻域条数（只在 `--spell` 时非零）
    #[serde(default)]
    pub spell_rows: usize,
    /// 写进 `pinyin` 的带拼音词卡数（只在 `--pinyin` 时非零）
    #[serde(default)]
    pub pinyin_cards: usize,
    /// 写进 `pinyin` 的读音行数（多音字一条词卡会有多行）
    #[serde(default)]
    pub pinyin_rows: usize,
    /// 本次构建的词头证据阈值。旧产物里读出来是空串，等价于 `all`。
    #[serde(default)]
    pub key_evidence: String,
    /// 词头等于本条记录某条例句英文侧高亮关键词的记录数（最强的自证）。
    #[serde(default)]
    pub tier_example_key: usize,
    /// 词头只作为整词出现在本条记录自己的释义文本里的记录数。
    #[serde(default)]
    pub tier_in_gloss: usize,
    /// 两处都找不到的记录数。这些词头不是"没把握"，是从隔壁词条抄来的。
    #[serde(default)]
    pub tier_unevidenced: usize,
    /// 被阈值挡在 `entries` 之外的记录数（`source_records` 仍然全量归档）。
    #[serde(default)]
    pub dropped_unevidenced_cards: usize,
    /// 写进 `vocab` 表的参考词数（只在 `--vocab` 时非零）
    #[serde(default)]
    pub vocab_rows: usize,
}

pub fn audit(input: &Path, out: &Path) -> Result<serde_json::Value> {
    audit_cancellable(input, out, None)
}

pub fn audit_cancellable(
    input: &Path,
    out: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<serde_json::Value> {
    check_cancel(cancel)?;
    ensure!(
        !out.exists(),
        "审计输出目录已存在，请选择新目录: {}",
        out.display()
    );
    let digest = hash_file(input)?;
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    let mut queue = std::io::BufWriter::new(File::create(temp.path().join("queue.jsonl"))?);
    let mut reader = BufReader::new(File::open(input)?);
    let mut count = 0usize;
    let mut missing = 0usize;
    let mut replacement = 0usize;
    // 每个候选词头一份档位计数：同一个词头可能来自多条记录，有的自证、有的不自证，
    // 只看总数会把这两种情况混成一锅。
    let mut heads = HashMap::<String, BTreeMap<&'static str, usize>>::new();
    let mut tier_counts = BTreeMap::<&'static str, usize>::new();
    while let Some(raw) = read_record(&mut reader)? {
        check_cancel(cancel)?;
        count += 1;
        let v: serde_json::Value =
            serde_json::from_slice(&raw).with_context(|| format!("第 {count} 行 JSON 无效"))?;
        let headword = v["headword"].as_str().unwrap_or("");
        let tier = evidence::key_tier(
            headword,
            &string_array(&v["senses"]),
            &string_array(&v["examples"]),
        );
        if headword.is_empty() {
            missing += 1;
        } else {
            *heads
                .entry(headword.into())
                .or_default()
                .entry(tier.as_str())
                .or_default() += 1;
            *tier_counts.entry(tier.as_str()).or_default() += 1;
        }
        if std::str::from_utf8(&raw)?.contains('\u{fffd}') {
            replacement += 1;
        }
        // 没有词头的记录不参与档位统计：档位说的是"这个词头可不可信"，而这里根本没有
        // 词头可评。队列里给它 null，免得审查的人照着"优先核实身份"去核实一个不存在的东西。
        let note = if headword.is_empty() {
            "这条记录没有词头：正文里本来就没有词头，本地不可恢复；要补的是词头本身，不是核实身份"
        } else {
            match tier {
                Tier::ExampleKey => {
                    "词头与本条记录例句的英文高亮词一致，属自证；仍需核实字段边界及对应关系"
                }
                Tier::InGloss => {
                    "词头出现在本条记录自己的释义里，是自证但弱一档；仍需核实字段边界及对应关系"
                }
                _ => "词头在本条记录里找不到任何自证，它可能就是隔壁词条的词；优先核实身份",
            }
        };
        serde_json::to_writer(
            &mut queue,
            &serde_json::json!({
                "line": count, "record_sha256": hash_bytes(&raw), "source_offset": v["offset"],
                "candidate_headword": headword,
                "key_tier": (!headword.is_empty()).then(|| tier.as_str()),
                "status": "unresolved", "senses": v["senses"],
                "english": v["english"], "phon_uk": v["phon_uk"], "phon_us": v["phon_us"],
                "note": note
            }),
        )?;
        writeln!(queue)?;
    }
    queue.flush()?;
    drop(queue);
    let mut suspicious: Vec<_> = heads
        .iter()
        .filter(|(_, tiers)| tiers.values().sum::<usize>() > 20)
        .map(|(headword, tiers)| {
            serde_json::json!([headword, tiers.values().sum::<usize>(), tiers])
        })
        .collect();
    suspicious.sort_by(|a, b| {
        b[1].as_u64()
            .cmp(&a[1].as_u64())
            .then_with(|| a[0].as_str().cmp(&b[0].as_str()))
    });
    let only_unevidenced = heads
        .values()
        .filter(|tiers| tiers.len() == 1 && tiers.contains_key("unevidenced"))
        .count();
    let report = serde_json::json!({"source_sha256":digest,"raw_records":count,
        "candidate_headwords":heads.len(), "records_without_headword":missing,
        "replacement_character_records":replacement,"verified_records":0,
        "key_tier_counts":tier_counts, "keys_only_unevidenced":only_unevidenced,
        "high_cardinality_candidates":suspicious, "status":"requires_review"});
    std::fs::write(
        temp.path().join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    std::fs::write(
        temp.path().join("manifest.template.json"),
        serde_json::to_vec_pretty(&Manifest {
            schema_version: 1,
            source_sha256: digest.clone(),
            data_version: "local-review-1".into(),
            reviews: Vec::new(),
        })?,
    )?;
    ensure!(hash_file(input)? == digest, "审计期间源文件发生变化");
    check_cancel(cancel)?;
    std::fs::rename(temp.path(), out)?;
    Ok(report)
}

fn selected(v: &serde_json::Value, key: &str, indices: &[usize]) -> Result<serde_json::Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for &i in indices {
        ensure!(seen.insert(i), "重复的 {key} 索引 {i}");
        out.push(
            v[key]
                .get(i)
                .with_context(|| format!("{key}[{i}] 不存在"))?
                .clone(),
        );
    }
    Ok(serde_json::Value::Array(out))
}

fn reviewed_card(
    raw: &[u8],
    r: &Review,
    source_hash: &str,
    glosses: Option<&crate::data::compile::RelationGlosses>,
) -> Result<WordCard> {
    ensure!(
        !r.headword.trim().is_empty()
            && !r.reviewer.trim().is_empty()
            && !r.evidence.trim().is_empty(),
        "verified 必须填写词头、核验者和证据"
    );
    ensure!(normalize(&r.headword).chars().count() <= 256, "词头过长");
    ensure!(!r.headword.chars().any(char::is_control), "词头含控制字符");
    let mut v: serde_json::Value = serde_json::from_slice(raw)?;
    let offset = v["offset"].as_u64();
    v["senses"] = selected(&v, "senses", &r.senses)?;
    v["headword"] = r.headword.clone().into();
    // pos collected over a merged raw block is not independent evidence.
    v["pos"] = serde_json::json!([]);
    if !r.keep_english {
        v["english"] = serde_json::Value::Null;
    }
    if !r.keep_phonetics {
        v["phon_uk"] = serde_json::Value::Null;
        v["phon_us"] = serde_json::Value::Null;
    }
    if !r.keep_forms {
        v["forms"] = serde_json::Value::Null;
    }
    if !r.keep_relations {
        for field in ["synonyms", "antonyms", "related"] {
            v[field] = serde_json::json!([]);
        }
    }
    let raw = serde_json::from_value(v)?;
    let id = u32::try_from(r.line).context("记录数量超出支持范围")?;
    let mut card = crate::data::compile::convert(id, raw, glosses);
    let mut chosen = Vec::new();
    let mut seen = HashSet::new();
    for &index in &r.examples {
        ensure!(seen.insert(index), "重复的例句索引");
        chosen.push(
            card.examples
                .get(index)
                .context("拆分后的例句索引不存在")?
                .clone(),
        );
    }
    card.examples = chosen;
    for alias in &r.aliases {
        use crate::core::model::{FormKind, WordForm};
        let kind = match alias.kind.as_str() {
            "past" => FormKind::PastTense,
            "past_participle" => FormKind::PastParticiple,
            "present_participle" => FormKind::PresentParticiple,
            "third_person" => FormKind::ThirdPerson,
            "plural" => FormKind::Plural,
            "comparative" => FormKind::Comparative,
            "superlative" => FormKind::Superlative,
            _ => FormKind::Other,
        };
        if let Some(form) = card.forms.iter_mut().find(|f| f.kind == kind) {
            if !form.words.contains(&alias.form) {
                form.words.push(alias.form.clone());
            }
        } else {
            card.forms.push(WordForm {
                kind,
                words: vec![alias.form.clone()],
            });
        }
    }
    for sense in &card.senses {
        if let Some(pos) = sense.pos {
            if !card.pos.contains(&pos) {
                card.pos.push(pos);
            }
        }
    }
    ensure!(
        !card.senses.is_empty() || card.gloss_en.is_some(),
        "词卡没有经核验的释义"
    );
    card.source = Some(SourceRef {
        source_sha256: source_hash.into(),
        record_sha256: r.record_sha256.clone(),
        line: r.line,
        offset,
        reviewer: r.reviewer.clone(),
        evidence: r.evidence.clone(),
    });
    Ok(card)
}

pub fn build(input: &Path, manifest_path: &Path, target: &Path) -> Result<BuildReport> {
    build_cancellable(input, manifest_path, target, None)
}

pub fn build_cancellable(
    input: &Path,
    manifest_path: &Path,
    target: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<BuildReport> {
    build_with_extras(input, manifest_path, &Extras::default(), target, cancel)
}

/// Import structured records directly. Corrections are optional and take precedence
/// over original cards with the same headword; every raw record remains archived.
pub fn import_structured(
    input: &Path,
    corrections: Option<&Path>,
    target: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<BuildReport> {
    import_structured_with_extras(input, corrections, &Extras::default(), target, cancel)
}

/// As `import_structured`, with the optional data tables switched on.
pub fn import_structured_with_extras(
    input: &Path,
    corrections: Option<&Path>,
    extras: &Extras<'_>,
    target: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<BuildReport> {
    build_internal(input, corrections, target, cancel, true, extras)
}

/// As `build_cancellable`, with the optional data tables switched on.
pub fn build_with_extras(
    input: &Path,
    manifest_path: &Path,
    extras: &Extras<'_>,
    target: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<BuildReport> {
    build_internal(input, Some(manifest_path), target, cancel, false, extras)
}

fn build_internal(
    input: &Path,
    manifest_path: Option<&Path>,
    target: &Path,
    cancel: Option<&AtomicBool>,
    full: bool,
    extras: &Extras<'_>,
) -> Result<BuildReport> {
    check_cancel(cancel)?;
    ensure!(
        input != target && manifest_path != Some(target),
        "输出不能覆盖输入或核验清单"
    );
    let source_hash = hash_file(input)?;
    let manifest_bytes = if let Some(path) = manifest_path {
        std::fs::read(path).context("读取修正清单")?
    } else {
        serde_json::to_vec(&Manifest {
            schema_version: 1,
            source_sha256: source_hash.clone(),
            data_version: "structured-import-v1".into(),
            reviews: vec![],
        })?
    };
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    ensure!(manifest.schema_version == 1, "不支持的核验清单版本");
    ensure!(
        !manifest.data_version.trim().is_empty(),
        "data_version 不能为空"
    );
    ensure!(
        source_hash == manifest.source_sha256,
        "源文件哈希变化；必须重新核验，旧清单不可直接使用"
    );
    let mut reviews = BTreeMap::new();
    for r in &manifest.reviews {
        ensure!(
            r.line > 0 && reviews.insert(r.line, r).is_none(),
            "重复或无效的源记录行号"
        );
    }
    ensure!(
        full || reviews.values().any(|r| r.status == ReviewStatus::Verified),
        "没有 verified 记录；先核实词头与释义，不可直接编译猜测词库"
    );
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = tempfile::NamedTempFile::new_in(parent)?;
    let destination = if target.exists() {
        target.canonicalize()?
    } else {
        parent
            .canonicalize()?
            .join(target.file_name().context("输出须为文件路径")?)
    };
    ensure!(
        destination != input.canonicalize()?
            && manifest_path
                .map(|p| p.canonicalize())
                .transpose()?
                .as_ref()
                != Some(&destination),
        "输出不能覆盖源资料或核验清单"
    );
    // 可选表在打开临时库之前读完：读表失败时连事务都不必开始。
    let mut aux = extras.load()?;
    let mut conn = rusqlite::Connection::open(temp.path())?;
    conn.execute_batch(&format!("PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version={SCHEMA_VERSION};
        PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=ON;
        CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE entries(id INTEGER PRIMARY KEY, key TEXT NOT NULL, char_len INTEGER NOT NULL, card BLOB NOT NULL, priority INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE source_records(line INTEGER PRIMARY KEY, record_sha256 TEXT NOT NULL, raw BLOB NOT NULL);
        CREATE INDEX entry_key ON entries(key);
        CREATE INDEX entry_length ON entries(char_len,key);
        CREATE TABLE aliases(form TEXT NOT NULL, lemma TEXT NOT NULL, kind TEXT NOT NULL, evidence TEXT NOT NULL,
            PRIMARY KEY(form,lemma,kind));
        CREATE VIRTUAL TABLE sense_search USING fts5(entry_id UNINDEXED, gloss UNINDEXED, tokens, tokenize='unicode61');"))?;
    // 可选表只在传了 `--levels` 时建：不带该标志的构建必须和今天逐字节一致。
    // schema_version 仍是 2，读取端对这张表是 lazy 的——老词库照样能开。
    if aux.levels.is_some() {
        conn.execute_batch(
            "CREATE TABLE word_level(key TEXT PRIMARY KEY, level INTEGER, tags TEXT NOT NULL);",
        )?;
    }
    // 例句索引同样只在传了标志时建。两张表都是"侧表"：句子本体在词卡 BLOB 里，
    // 这里只存去重后的句对和它挂在哪些关键词下，读取端对它们都是 lazy 的。
    if extras.examples {
        conn.execute_batch(
            "CREATE TABLE example(id INTEGER PRIMARY KEY, en TEXT NOT NULL, zh TEXT NOT NULL);
             CREATE TABLE example_link(example_id INTEGER NOT NULL, key TEXT NOT NULL, side INTEGER NOT NULL,
               PRIMARY KEY(example_id,key,side)) WITHOUT ROWID;
             CREATE INDEX example_link_key ON example_link(key,side);",
        )?;
    }
    // 拼写候选的删字符索引，也只在传了标志时建。主键 (delkey,word) 本身就是 delkey
    // 的前缀索引，再单开一个 `ON delkey` 只是把同一份数据在库里存两遍。
    if extras.spell {
        conn.execute_batch(
            "CREATE TABLE spell_key(delkey TEXT NOT NULL, word TEXT NOT NULL,
               PRIMARY KEY(delkey,word)) WITHOUT ROWID",
        )?;
    }
    // 词头证据档位只在设了阈值时落盘：`all` 不过滤任何词头，也就不需要这张表。
    // 行号是主键而不是词头，因为 `entries.id` 本来就等于源行号——按键存会派生出
    // 一份和 `entries` 会漂移的第二真相。
    // 拼音索引同样是派生侧表：词卡里已有拼音，这里只存折叠后的读音键。
    // 连写键负责词首前缀，分音节键负责 `an` 命中 `西安` 这类音节边界。
    if extras.pinyin {
        conn.execute_batch(
            "CREATE TABLE pinyin(entry_id INTEGER NOT NULL, flat TEXT NOT NULL, syllables TEXT NOT NULL,
               PRIMARY KEY(entry_id,flat)) WITHOUT ROWID;
             CREATE INDEX pinyin_flat ON pinyin(flat,entry_id);
             CREATE INDEX pinyin_syllables ON pinyin(syllables,entry_id);",
        )?;
    }
    if extras.key_evidence.records() {
        conn.execute_batch(
            "CREATE TABLE key_evidence(line INTEGER PRIMARY KEY, tier INTEGER NOT NULL);",
        )?;
    }
    // 参考词表是本次构建的输入文件，不是派生侧表：它给"查不到的词"那句提示用。
    if aux.vocab.is_some() {
        conn.execute_batch("CREATE TABLE vocab(word TEXT PRIMARY KEY) WITHOUT ROWID;")?;
    }
    let tx = conn.transaction()?;
    let mut report = BuildReport {
        source_sha256: source_hash.clone(),
        data_version: manifest.data_version,
        import_mode: if full { "structured" } else { "selected" }.into(),
        key_evidence: extras.key_evidence.as_str().into(),
        ..Default::default()
    };
    let mut keys = HashSet::new();
    let mut corrected_keys = HashSet::new();
    let mut example_ids: HashMap<(String, String), i64> = HashMap::new();
    let mut reader = BufReader::new(File::open(input)?);
    while let Some(raw) = read_record(&mut reader)? {
        report.raw_records += 1;
        check_cancel(cancel)?;
        let value: serde_json::Value = serde_json::from_slice(&raw)
            .with_context(|| format!("第 {} 行 JSON 无效", report.raw_records))?;
        if full {
            // Keep even records without a headword, without inventing a searchable identity.
            let mut archive =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            archive.write_all(&raw)?;
            tx.execute(
                "INSERT INTO source_records VALUES (?1,?2,?3)",
                rusqlite::params![report.raw_records, hash_bytes(&raw), archive.finish()?],
            )?;
            report.archived_records += 1;
        }
        let review = reviews.remove(&report.raw_records);
        if review.is_none() && !full {
            report.unresolved_records += 1;
            continue;
        }
        // 词头分级和例句索引都从源 JSON 的原始数组算：`value` 在建卡时会被
        // `from_value` 消费掉，而分级要看的恰恰是"这条记录自己写了什么"，不是
        // 核验之后剩下的字段。只读 `senses`，不读 `english`——那个字段讲的是隔壁词条。
        let raw_senses = string_array(&value["senses"]);
        let raw_examples = string_array(&value["examples"]);
        let source_tier = evidence::key_tier(
            value["headword"].as_str().unwrap_or(""),
            &raw_senses,
            &raw_examples,
        );
        let card = if let Some(r) = review {
            ensure!(
                hash_bytes(&raw) == r.record_sha256,
                "第 {} 行哈希不符",
                r.line
            );
            match r.status {
                ReviewStatus::Unresolved => {
                    report.unresolved_records += 1;
                    continue;
                }
                ReviewStatus::Rejected => {
                    report.rejected_records += 1;
                    continue;
                }
                ReviewStatus::Verified => {}
            }
            let card = reviewed_card(&raw, r, &source_hash, aux.glosses.as_ref())
                .with_context(|| format!("修正记录第 {} 行", r.line))?;
            report.verified_cards += 1;
            corrected_keys.insert(normalize(&card.headword));
            card
        } else {
            let headword = value["headword"].as_str().unwrap_or("");
            if headword.trim().is_empty() {
                report.missing_headword_records += 1;
                continue;
            }
            if normalize(headword).chars().count() > 256 || headword.chars().any(char::is_control) {
                report.invalid_headword_records += 1;
                continue;
            }
            let offset = value["offset"].as_u64();
            let mut card = crate::data::compile::convert(
                u32::try_from(report.raw_records)?,
                serde_json::from_value(value)?,
                aux.glosses.as_ref(),
            );
            card.source = Some(SourceRef {
                source_sha256: source_hash.clone(),
                record_sha256: hash_bytes(&raw),
                line: report.raw_records,
                offset,
                reviewer: String::new(),
                evidence: "Imported from structured source fields".into(),
            });
            card
        };
        let key = normalize(&card.headword);
        // 人工核验过的记录由人负责，不由自动判据负责：任何阈值都收它。
        let tier = if review.is_some() {
            Tier::Reviewed
        } else {
            source_tier
        };
        match tier {
            Tier::ExampleKey => report.tier_example_key += 1,
            Tier::InGloss => report.tier_in_gloss += 1,
            Tier::Unevidenced => report.tier_unevidenced += 1,
            Tier::Reviewed => {}
        }
        if extras.key_evidence.records() {
            tx.execute(
                "INSERT INTO key_evidence VALUES (?1,?2)",
                rusqlite::params![report.raw_records, tier.as_u8() as i64],
            )?;
        }
        // 挡在 `keys.insert` 之前：一个不进库的词头不能留在屈折归并和删字符索引的
        // 候选集里，否则别名会指向一张根本不存在的词卡。
        if !extras.key_evidence.accepts(tier) {
            report.dropped_unevidenced_cards += 1;
            continue;
        }
        keys.insert(key.clone());
        report.related_glosses += card
            .synonyms
            .iter()
            .chain(&card.antonyms)
            .chain(&card.related)
            .filter(|relation| relation.gloss.is_some())
            .count();
        let packed = crate::data::compile::pack_compressed(&serde_json::to_vec(&card)?)?;
        tx.execute(
            "INSERT INTO entries VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                card.id,
                key,
                key.chars().count(),
                packed,
                i32::from(review.is_some())
            ],
        )?;
        for s in &card.senses {
            let tokens = crate::data::store::search_tokens(&s.gloss);
            tx.execute(
                "INSERT INTO sense_search(entry_id,gloss,tokens) VALUES (?1,?2,?3)",
                rusqlite::params![card.id, s.gloss, tokens],
            )?;
            report.senses += 1;
        }
        if extras.pinyin {
            if let Some(pinyin) = &card.pinyin {
                let readings = crate::data::store::pinyin_keys(pinyin);
                if !readings.is_empty() {
                    report.pinyin_cards += 1;
                }
                for reading in readings {
                    report.pinyin_rows += tx.execute(
                        "INSERT OR IGNORE INTO pinyin VALUES (?1,?2,?3)",
                        rusqlite::params![card.id, reading.flat, reading.syllables],
                    )?;
                }
            }
        }
        for a in review.into_iter().flat_map(|r| &r.aliases) {
            report.aliases +=
                insert_alias(&tx, &normalize(&a.form), &key, &a.kind, a.evidence.trim())?;
        }
        if extras.examples {
            // 修正卡只保留被点名的例句，索引要按同一批下标取，不能拿全量去建。
            let all = crate::data::compile::example_sides(&raw_examples);
            let sides: Vec<crate::data::compile::Sides> = match review {
                Some(r) => r
                    .examples
                    .iter()
                    .map(|&index| {
                        all.get(index)
                            .cloned()
                            .with_context(|| format!("第 {} 行例句索引越界", r.line))
                    })
                    .collect::<Result<Vec<_>>>()?,
                None => all,
            };
            index_examples(&tx, &key, &card, &sides, &mut example_ids)?;
        }
        report.indexed_cards += 1;
    }
    if !reviews.is_empty() {
        bail!("核验清单引用了不存在的源记录行号");
    }
    for row in &aux.inflections {
        check_cancel(cancel)?;
        let form = normalize(&row.form);
        let lemma = normalize(&row.lemma);
        validate_alias(&form, &row.kind, &row.evidence)?;
        // A generated table reaches past this library: many of its lemmas have
        // no card here at all, and a form pointing at one would be a dangling
        // index the store refuses to open with.
        if form == lemma || !keys.contains(&lemma) {
            report.inflections_unmatched += 1;
            continue;
        }
        report.inflections += insert_alias(&tx, &form, &lemma, &row.kind, &row.evidence)?;
    }
    if let Some(levels) = &aux.levels {
        // 词级表按词形建，不做原形归并：源表本来就把 yelling 和 yell 各自列了等级。
        for row in &levels.rows {
            check_cancel(cancel)?;
            tx.execute(
                "INSERT INTO word_level VALUES (?1,?2,?3)",
                rusqlite::params![row.word, row.level, level_tags(&row.tags)],
            )?;
            report.word_levels += 1;
        }
        // 等级和大纲表的名字、说明跟着词库走，`zedict hint` 才说得出第 8 级是什么意思。
        tx.execute(
            "INSERT INTO meta VALUES ('levels',?1)",
            [serde_json::to_string(&levels.meta)?],
        )?;
    }
    if extras.examples {
        // 剔除高频中文键是一次静默的数据清洗，所以键数、链数和最高频都要报出来。
        tx.execute(
            "CREATE TEMP TABLE noisy_key(key TEXT PRIMARY KEY, examples INTEGER NOT NULL)",
            [],
        )?;
        tx.execute(
            "INSERT INTO noisy_key SELECT key, count(*) FROM example_link
             WHERE side=1 GROUP BY key HAVING count(*) > ?1",
            [NOISY_KEY_EXAMPLES],
        )?;
        let dropped_links = tx.execute(
            "DELETE FROM example_link WHERE side=1 AND key IN (SELECT key FROM noisy_key)",
            [],
        )?;
        let (dropped_keys, dropped_max): (usize, usize) = tx.query_row(
            "SELECT count(*), ifnull(max(examples),0) FROM noisy_key",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        // 光报数还不够：被剔掉的键要能在库里查到名字，否则这一步就是无痕清洗。
        let noisy_keys = {
            let mut stmt =
                tx.prepare("SELECT key, examples FROM noisy_key ORDER BY examples DESC LIMIT 12")?;
            let rows = stmt.query_map([], |row| {
                Ok(serde_json::json!({
                    "key": row.get::<_, String>(0)?,
                    "examples": row.get::<_, usize>(1)?,
                }))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.execute("DROP TABLE noisy_key", [])?;
        let links: usize =
            tx.query_row("SELECT count(*) FROM example_link", [], |row| row.get(0))?;
        report.examples = example_ids.len();
        report.example_links = links;
        report.dropped_noisy_keys = dropped_keys;
        aux.datasets
            .as_object_mut()
            .context("可选资料记录不是对象")?
            .insert(
                "examples".to_string(),
                serde_json::json!({
                    "derived_from": "input",
                    "source_sha256": source_hash,
                    "rows": example_ids.len(),
                    "links": links,
                    "noisy_key_threshold": NOISY_KEY_EXAMPLES,
                    "dropped_noisy_keys": dropped_keys,
                    "dropped_noisy_links": dropped_links,
                    "dropped_max_examples": dropped_max,
                    "dropped_keys": noisy_keys,
                }),
            );
    }
    if extras.spell {
        // 删字符邻域：词头每个字符各删一次得到的所有串，外加"删零次"那一条自指行。
        // 查询侧照同一条规则生成，两边就在同一个 delkey 上会师——少了自指行，
        // "少打一个字母"的写法就没有任何键能和词头对上。
        let mut stmt = tx.prepare("INSERT OR IGNORE INTO spell_key VALUES (?1,?2)")?;
        let mut words = 0usize;
        for word in &keys {
            let chars: Vec<char> = word.chars().collect();
            // 单字词删完就是空串，而空串对任何查询都成立，等于把整张表倒出来。
            if chars.len() < 2 {
                continue;
            }
            stmt.execute(rusqlite::params![word, word])?;
            for skip in 0..chars.len() {
                let delkey: String = chars
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != skip)
                    .map(|(_, c)| *c)
                    .collect();
                stmt.execute(rusqlite::params![delkey, word])?;
            }
            words += 1;
        }
        drop(stmt);
        report.spell_rows = tx.query_row("SELECT count(*) FROM spell_key", [], |row| row.get(0))?;
        aux.datasets
            .as_object_mut()
            .context("可选资料记录不是对象")?
            .insert(
                "spell".to_string(),
                serde_json::json!({
                    "derived_from": "entries",
                    "source_sha256": source_hash,
                    "words": words,
                    "rows": report.spell_rows,
                }),
            );
    }
    if extras.pinyin {
        let (rows, cards): (usize, usize) = tx.query_row(
            "SELECT count(*), count(DISTINCT entry_id) FROM pinyin",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        aux.datasets
            .as_object_mut()
            .context("可选资料记录不是对象")?
            .insert(
                "pinyin".to_string(),
                serde_json::json!({
                    "derived_from": "entries",
                    "source_sha256": source_hash,
                    "cards": cards,
                    "rows": rows,
                }),
            );
    }
    if let Some(words) = &aux.vocab {
        // 参考词表不参与任何检索，只回答"这个查不到的词是不是常见英文词"。
        // 存归一化之后的词形：查询侧也归一化，两边才有同一个形状。
        let mut stmt = tx.prepare("INSERT INTO vocab VALUES (?1)")?;
        for word in words {
            check_cancel(cancel)?;
            stmt.execute([word])?;
        }
        drop(stmt);
        report.vocab_rows = words.len();
    }
    if extras.key_evidence.records() {
        // 阈值改了多少条卡，必须能从库里自己算出来：`key_evidence` 每一行都有档位，
        // 而 `entries` 少了哪几行，看这份记录就知道。
        let rows: usize =
            tx.query_row("SELECT count(*) FROM key_evidence", [], |row| row.get(0))?;
        aux.datasets
            .as_object_mut()
            .context("可选资料记录不是对象")?
            .insert(
                "key_evidence".to_string(),
                serde_json::json!({
                    "threshold": extras.key_evidence.as_str(),
                    "derived_from": "input",
                    "rows": rows,
                    "tier_example_key": report.tier_example_key,
                    "tier_in_gloss": report.tier_in_gloss,
                    "tier_unevidenced": report.tier_unevidenced,
                    "tier_reviewed": report.verified_cards,
                    "dropped_cards": report.dropped_unevidenced_cards,
                }),
            );
    }
    report.verified_headwords = corrected_keys.len();
    report.indexed_headwords = keys.len();
    tx.execute(
        "INSERT INTO meta VALUES ('report',?1)",
        [serde_json::to_string(&report)?],
    )?;
    tx.execute(
        "INSERT INTO meta VALUES ('manifest_sha256',?1)",
        [hash_bytes(&manifest_bytes)],
    )?;
    tx.execute(
        "INSERT INTO meta VALUES ('manifest',?1)",
        [String::from_utf8(manifest_bytes)?],
    )?;
    if aux.datasets.as_object().is_some_and(|d| !d.is_empty()) {
        tx.execute(
            "INSERT INTO meta VALUES ('datasets',?1)",
            [serde_json::to_string(&aux.datasets)?],
        )?;
    }
    tx.execute(
        "INSERT INTO sense_search(sense_search) VALUES ('optimize')",
        [],
    )?;
    tx.execute(
        "INSERT INTO sense_search(sense_search,rank) VALUES ('integrity-check',1)",
        [],
    )?;
    tx.commit()?;
    conn.close().map_err(|(_, e)| e)?;
    let check = DictStore::open(temp.path())?;
    check.check_integrity()?;
    drop(check);
    temp.as_file().sync_all()?;
    ensure!(
        hash_file(input)? == source_hash,
        "构建期间源文件发生变化；未替换词库"
    );
    check_cancel(cancel)?;
    temp.persist(target)
        .map_err(|e| e.error)
        .context("安装词库失败；旧词库已保留（Windows 上请先关闭使用词库的进程）")?;
    Ok(report)
}
