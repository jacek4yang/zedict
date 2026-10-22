#![allow(dead_code)]
use serde_json::json;
use std::path::{Path, PathBuf};
use zedict::data::ingest::{AliasReview, Manifest, Review, ReviewStatus};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub source: PathBuf,
    pub manifest: PathBuf,
    pub db: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        let manifest = dir.path().join("reviews.json");
        let db = dir.path().join("dict.sqlite");
        let config = dir.path().join("config.toml");
        let cache = dir.path().join("cache");
        let data = [
            ("world", "世界"),
            ("run", "跑；运行"),
            ("see", "看见"),
            ("saw", "锯子"),
            ("axe", "斧头"),
            ("axis", "轴"),
            ("apple", "苹果"),
            ("book", "书"),
            ("learn", "学习"),
            ("computer", "计算机"),
            ("split", "学，习"),
            ("café", "咖啡馆"),
            ("take into account", "考虑"),
            ("world", "领域"),
            ("robust", "强健的"),
            ("unverified", "不应出现"),
            ("rejected", "不应出现"),
        ];
        let mut lines = Vec::new();
        let mut reviews = Vec::new();
        for (i, (word, gloss)) in data.iter().enumerate() {
            // Deliberately wrong candidate and neighboring sense: tests must exercise the gate.
            let raw = json!({"offset":i*100,"headword":"wrong-guess","senses":["串入的错误义项",format!("<i>n.</i> {gloss}")],
                "phon_uk":null,"phon_us":null,"english":null,"examples":["Hello@你好","World@世界","Third@第三","Fourth@第四"]});
            let line = serde_json::to_string(&raw).unwrap();
            let aliases = match *word {
                "run" => vec![("running", "present_participle"), ("ran", "past")],
                "see" => vec![("saw", "past")],
                "axe" | "axis" => vec![("axes", "plural")],
                _ => Vec::new(),
            }
            .into_iter()
            .map(|(form, kind)| AliasReview {
                form: form.into(),
                kind: kind.into(),
                evidence: "synthetic test fixture".into(),
            })
            .collect();
            reviews.push(Review {
                line: i + 1,
                record_sha256: zedict::data::ingest::hash_bytes(line.as_bytes()),
                status: if *word == "unverified" {
                    ReviewStatus::Unresolved
                } else if *word == "rejected" {
                    ReviewStatus::Rejected
                } else {
                    ReviewStatus::Verified
                },
                headword: word.to_string(),
                reviewer: "test-fixture-not-production".into(),
                evidence: "synthetic fixture with explicitly assigned identity".into(),
                senses: vec![1],
                examples: vec![0, 1, 2, 3],
                keep_english: false,
                keep_phonetics: false,
                keep_forms: false,
                keep_relations: false,
                aliases,
            });
            lines.push(line);
        }
        std::fs::write(&source, lines.join("\n") + "\n").unwrap();
        let m = Manifest {
            schema_version: 1,
            source_sha256: zedict::data::ingest::hash_file(&source).unwrap(),
            data_version: "synthetic-tests".into(),
            reviews,
        };
        std::fs::write(&manifest, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        std::fs::write(&config, "db = 'dict.sqlite'\n").unwrap();
        zedict::data::ingest::build(&source, &manifest, &db).unwrap();
        Self {
            dir,
            source,
            manifest,
            db,
            config,
            cache,
        }
    }
    pub fn cli(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env_remove("ZEDICT_EDGE_TRANSLATE_URL")
            .env_remove("ZEDICT_EDGE_TTS_URL")
            .env_remove("ZEDICT_YOUDAO_URL")
            .env_remove("ZEDICT_STUDY_DB")
            .env("ZEDICT_CACHE_DIR", &self.cache);
        cmd.arg("--config")
            .arg(&self.config)
            .current_dir(std::env::temp_dir());
        cmd
    }
}

// ─────────────── 带词级表 + 词形表的词库夹具（hint / export / 拼写建议） ───────────────
//
// 词级表按源表的形状给：bands 是互不重叠的频率带，`level: null` 表示该词形只在
// 某份大纲词表里、源表没给带号。`running` 只进 aliases 不进词头，专门用来验
// "拼错的词形回到原形"这条路。

pub struct LevelFixture {
    pub dir: tempfile::TempDir,
    pub db: PathBuf,
    pub config: PathBuf,
    pub word_levels: usize,
}

impl LevelFixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        std::fs::write(
            &source,
            [
                json!({"headword":"world","pos":["n."],"senses":["<i>n.</i> 世界"]}),
                json!({"headword":"run","pos":["v.","n."],"senses":["<i>v.</i> 跑","<i>n.</i> 奔跑"]}),
                json!({"headword":"ephemeral","pos":["adj."],"senses":["<i>adj.</i> 短暂的"]}),
                json!({"headword":"zealous","pos":["adj."],"senses":["<i>adj.</i> 热心的"]}),
                json!({"headword":"axe","pos":["n."],"senses":["<i>n.</i> 斧头"]}),
                json!({"headword":"axis","pos":["n."],"senses":["<i>n.</i> 轴"]}),
                json!({"headword":"take into account","pos":["v."],"senses":["<i>v.</i> 考虑"]}),
            ]
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n")
                + "\n",
        )
        .unwrap();

        let levels = dir.path().join("levels.jsonl");
        let meta = json!({
            "kind": "meta",
            "source": {"file": "level.bin", "sha256": "0", "words": 4},
            "bands": [
                {"level":1,"name":"b1","desc":"COCA等级 1 - 2000","color":"F4B183","items":3},
                {"level":2,"name":"b2","desc":"COCA等级 2000 - 4000","color":"FFD966","items":1},
                {"level":9,"name":"b9","desc":"COCA等级 18000 - 20000","color":"C6E0B4","items":2}
            ],
            "tags": [
                {"tag":"junior","name":"初中","desc":"初中词汇","color":"000000","items":1},
                {"tag":"gre","name":"GRE","desc":"GRE词汇","color":"000000","items":2}
            ]
        });
        let rows = [
            json!({"kind":"word","word":"world","level":1,"tags":["junior"]}),
            json!({"kind":"word","word":"run","level":2,"tags":["junior"]}),
            json!({"kind":"word","word":"axe","level":9,"tags":["gre"]}),
            json!({"kind":"word","word":"axis","level":1,"tags":[]}),
            json!({"kind":"word","word":"ephemeral","level":9,"tags":["gre"]}),
            // 只有大纲归属、没有带号：源表里有一部分词就是这样标的
            json!({"kind":"word","word":"provisional","level":null,"tags":["gre"]}),
        ];
        std::fs::write(
            &levels,
            std::iter::once(meta)
                .chain(rows)
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();

        let inflections = dir.path().join("inflect.jsonl");
        std::fs::write(
            &inflections,
            [
                json!({"form":"running","lemma":"run","kind":"present_participle","evidence":"hunspell en_US.dic running st:run"}),
                json!({"form":"axes","lemma":"axe","kind":"plural","evidence":"hunspell en_US.dic axes st:axe"}),
            ]
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n")
                + "\n",
        )
        .unwrap();

        let db = dir.path().join("dict.sqlite");
        let report = zedict::data::ingest::import_structured_with_extras(
            &source,
            None,
            &zedict::data::ingest::Extras {
                inflections: Some(&inflections),
                levels: Some(&levels),
                ..Default::default()
            },
            &db,
            None,
        )
        .unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "db = 'dict.sqlite'\n").unwrap();
        Self {
            dir,
            db,
            config,
            word_levels: report.word_levels,
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn cli(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env("ZEDICT_CACHE_DIR", self.path("cache"))
            .current_dir(std::env::temp_dir());
        cmd.arg("--config").arg(&self.config);
        cmd
    }
}

// ─────────────────────── 带例句索引的词库夹具（examples / 例句反查） ───────────────────────
//
// 例句索引是从词卡例句派生的侧表，所以夹具必须同时钉住"句对数"和"关键词链数"。
// 两个数量级故意安排在阈值 300 的两侧：`噪声词` 挂 301 句要被剔除，`边界词` 正好
// 300 句要留下——阈值写反一位数，这两个断言就会有一个失败。

pub struct ExamplesFixture {
    pub dir: tempfile::TempDir,
    pub db: PathBuf,
    pub config: PathBuf,
    pub rows: usize,
    pub links: usize,
    pub dropped_keys: usize,
}

/// 关键词标记必须逐字用源数据的形状：英文半区、中文半区各包一个片段。
fn marked(en: &str, en_key: &str, zh: &str, zh_key: &str) -> String {
    let wrap = |key: &str| format!("<span class=\"key\">{key}</span>");
    format!(
        "{}@{}",
        en.replacen(en_key, &wrap(en_key), 1),
        zh.replacen(zh_key, &wrap(zh_key), 1)
    )
}

impl ExamplesFixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        let mut records = vec![
            // 中文关键词反查：病假条 → a certificate for sick leave
            json!({"headword":"sick","pos":["adj."],"senses":["<i>adj.</i> 生病的"],
                   "examples":[
                       marked("Please write me a certificate for sick leave","a certificate for sick leave",
                              "请给我开一张病假条","病假条"),
                       marked("He is on sick leave this week","sick leave",
                              "他这周在病假期间","病假"),
                       // 这句的关键词是 sick，但正文里还带着别张卡的词头 bare——
                       // `query bare --full` 的例句反查就靠这种句子。
                       marked("Even a bare wall looks clean when you are sick","sick",
                              "生病时空墙也干净","空墙")]}),
            // 非词头的英文短语键：库里没有 "in spite of" 这个词头，仍然要能按它查到
            json!({"headword":"spite","pos":["n."],"senses":["<i>n.</i> 恶意"],
                   "examples":[marked("In spite of the rain we went out","In spite of",
                                      "尽管下雨我们还是出门了","尽管")]}),
            // 完全没有关键词标记的例句：只有词头这一条链
            json!({"headword":"bare","pos":["adj."],"senses":["<i>adj.</i> 空的"],
                   "examples":["Nothing marked here at all@这里一个标记都没有"]}),
        ];
        // 阈值上下一各一批：301 句共用一个中文键（剔除），300 句共用另一个（保留）。
        for (headword, zh_key, count) in [("probe", "噪声词", 301usize), ("keep", "边界词", 300)]
        {
            let examples = (0..count)
                .map(|i| {
                    marked(
                        &format!("Sentence {headword} number {i} says nothing"),
                        &format!("{headword} number {i}"),
                        &format!("{zh_key}第{i}条说法"),
                        zh_key,
                    )
                })
                .collect::<Vec<_>>();
            records.push(json!({"headword":headword,"senses":[format!("<i>n.</i> {zh_key}")],"examples":examples}));
        }
        std::fs::write(
            &source,
            records
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let db = dir.path().join("dict.sqlite");
        let report = zedict::data::ingest::import_structured_with_extras(
            &source,
            None,
            &zedict::data::ingest::Extras {
                examples: true,
                ..Default::default()
            },
            &db,
            None,
        )
        .unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "db = 'dict.sqlite'\n").unwrap();
        Self {
            dir,
            db,
            config,
            rows: report.examples,
            links: report.example_links,
            dropped_keys: report.dropped_noisy_keys,
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn cli(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env("ZEDICT_CACHE_DIR", self.path("cache"))
            .current_dir(std::env::temp_dir());
        cmd.arg("--config").arg(&self.config);
        cmd
    }
}

// ───────────────────────── 可选资料：删字符拼写索引 ─────────────────────────
/// 同一批记录建两个库，一个带 `spell_key` 一个不带。补位候选按构造只会接在已有
/// 候选之后，所以"不带"的那个必须是"带"的那个的前缀——这个对照就是这个 fixture
/// 存在的全部理由，光有一个带表的库是证不出"没挤掉任何东西"的。
pub struct SpellFixture {
    pub dir: tempfile::TempDir,
    pub rows: usize,
    with_config: PathBuf,
    without_config: PathBuf,
}

impl SpellFixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        // 词头都是短词：删字符索引只在 4 字以内的查询上带得来新候选（长查询的
        // 穷举扫描本来就允许距离 2，而删一个字符的结构最多只差两步）。
        // `ctx` 差 `cat` 两步——两边都删掉一个 `t`/`a` 会落到同一个 `ct` 上，
        // 这条只有自指/邻域会师才走得到的通路，就是这张表存在的全部理由。
        let records = [
            json!({"headword":"ball","pos":["n."],"senses":["<i>n.</i> 球"]}),
            json!({"headword":"bad","pos":["adj."],"senses":["<i>adj.</i> 坏的"]}),
            json!({"headword":"cat","pos":["n."],"senses":["<i>n.</i> 猫"]}),
            json!({"headword":"certificate","pos":["n."],"senses":["<i>n.</i> 证书"]}),
            // 单字词删完就是空串。它必须整条不进表，否则空 delkey 会对任何查询成立。
            json!({"headword":"a","pos":["art."],"senses":["<i>art.</i> 一个"]}),
        ];
        std::fs::write(
            &source,
            records
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let build = |stem: &str, spell: bool| {
            let name = format!("{stem}.sqlite");
            let db = dir.path().join(&name);
            let report = zedict::data::ingest::import_structured_with_extras(
                &source,
                None,
                &zedict::data::ingest::Extras {
                    spell,
                    ..Default::default()
                },
                &db,
                None,
            )
            .unwrap();
            let config = dir.path().join(format!("{stem}.toml"));
            std::fs::write(&config, format!("db = '{name}'\n")).unwrap();
            (report, config)
        };
        let (report, with_config) = build("with", true);
        let (_, without_config) = build("without", false);
        Self {
            dir,
            rows: report.spell_rows,
            with_config,
            without_config,
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `with` = 带 `spell_key` 的库，`false` = 同一批记录建的不带对照库。
    pub fn cli(&self, with: bool) -> std::process::Command {
        self.spawn(if with {
            &self.with_config
        } else {
            &self.without_config
        })
    }

    fn spawn(&self, config: &std::path::Path) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env("ZEDICT_CACHE_DIR", self.path("cache"))
            .current_dir(std::env::temp_dir());
        cmd.arg("--config").arg(config);
        cmd
    }
}

// ───────────────────────── 词头证据阈值（--key-evidence / --vocab） ─────────────────────────
//
// 三条记录，一条一个档：`sick` 的词头就是自己例句里那个英文高亮词（最强的自证），
// `promotion` 的词头只出现在自己的释义里，`angle` 的词头在本条记录里哪儿都找不到
// ——它就是"错卡"的形状：释义 `气骨导间距` 属于隔壁词条。阈值开关唯一该变的区别是
// 第三类进不进 `entries`；原始记录归档、义项检索和档位普查在三种取值下必须一模一样。
pub struct EvidenceDb {
    pub dir: tempfile::TempDir,
    pub db: PathBuf,
    pub config: PathBuf,
    pub report: zedict::data::ingest::BuildReport,
    pub source: PathBuf,
    /// 参考词表文件总是存在——"表建没建"取决于这次构建有没有把它传进去，而不是
    /// 取决于磁盘上有没有这个文件。
    pub vocab: PathBuf,
}

/// 一行一个英文词的参考词表：`promotion` 在表里，但 `--key-evidence strong` 把它丢
/// 出了词库——这正是那句诚实提示唯一能成立的前提。
pub fn evidence_vocab(dir: &Path) -> PathBuf {
    let path = dir.join("vocab.txt");
    std::fs::write(
        &path,
        "Receive\nangle\nSICK\npromotion\npromotion\nunknownword\n",
    )
    .unwrap();
    path
}

pub fn build_evidence(
    stem: &str,
    parent: &Path,
    threshold: zedict::core::evidence::KeyEvidence,
    with_vocab: bool,
) -> EvidenceDb {
    let dir = tempfile::tempdir_in(parent).unwrap();
    let vocab = evidence_vocab(dir.path());
    let source = dir.path().join("source.jsonl");
    write_evidence_source(&source);
    let db = dir.path().join(format!("{stem}.sqlite"));
    let report = zedict::data::ingest::import_structured_with_extras(
        &source,
        None,
        &zedict::data::ingest::Extras {
            key_evidence: threshold,
            vocab: with_vocab.then_some(vocab.as_path()),
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    let config = dir.path().join(format!("{stem}.toml"));
    std::fs::write(&config, format!("db = '{stem}.sqlite'\n")).unwrap();
    EvidenceDb {
        dir,
        db,
        config,
        report,
        source,
        vocab,
    }
}

/// 只核验第 3 行（`angle`，自动判据下的 E 档）的那条记录，其余两行交给自动判据。
/// 用它才能把"人工确认"和"档位"这两件事分开证明：`strong` 挡住的记录，人签过就
/// 照样进库；而没签过的 D 档记录，在 `strong` 下仍然进不去。
pub fn build_evidence_verified(
    stem: &str,
    parent: &Path,
    threshold: zedict::core::evidence::KeyEvidence,
) -> EvidenceDb {
    let dir = tempfile::tempdir_in(parent).unwrap();
    let vocab = evidence_vocab(dir.path());
    let source = dir.path().join("source.jsonl");
    write_evidence_source(&source);
    let lines: Vec<String> = std::fs::read_to_string(&source)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let manifest_path = dir.path().join("reviews.json");
    let manifest = Manifest {
        schema_version: 1,
        source_sha256: zedict::data::ingest::hash_file(&source).unwrap(),
        data_version: "synthetic-tests".into(),
        reviews: vec![Review {
            line: 3,
            record_sha256: zedict::data::ingest::hash_bytes(lines[2].as_bytes()),
            status: ReviewStatus::Verified,
            headword: "angle".into(),
            reviewer: "test-fixture-not-production".into(),
            evidence: "synthetic fixture with explicitly assigned identity".into(),
            senses: vec![0],
            examples: vec![0],
            keep_english: false,
            keep_phonetics: false,
            keep_forms: false,
            keep_relations: false,
            aliases: vec![],
        }],
    };
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let db = dir.path().join(format!("{stem}.sqlite"));
    let report = zedict::data::ingest::import_structured_with_extras(
        &source,
        Some(&manifest_path),
        &zedict::data::ingest::Extras {
            key_evidence: threshold,
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    let config = dir.path().join(format!("{stem}.toml"));
    std::fs::write(&config, format!("db = '{stem}.sqlite'\n")).unwrap();
    EvidenceDb {
        dir,
        db,
        config,
        report,
        source,
        vocab,
    }
}

pub fn write_evidence_source(source: &Path) {
    std::fs::write(
        source,
        evidence_records()
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
}

pub fn evidence_records() -> [serde_json::Value; 3] {
    [
        // C 档：词头 == 例句英文侧的 <span class="key"> 高亮词。
        json!({"headword":"sick","pos":["adj."],"senses":["<i>adj.</i> 生病的"],
               "examples":["She was <span class=\"key\">sick</span> last week@她上周病了"]}),
        // D 档：词头作为整词出现在自己的释义里，但没有任何例句标记。
        json!({"headword":"promotion","pos":["n."],"senses":["<i>n.</i> promotion 促销活动"]}),
        // E 档：释义属于别的词条，例句里那句 "a right angle" 没有高亮标记，而
        // `english` 是英文释义池——它讲的是隔壁词条，所以两处都不能给它自证。
        // 这条记录就是"错卡"的真实形状：释义 `气骨导间距` 属于 air-bone gap。
        json!({"headword":"angle","pos":["n."],"senses":["<i>n.</i> 气骨导间距"],
               "english":"air-bone gap",
               "examples":["The plane turns through a right angle@这架飞机转过一个直角"]}),
    ]
}

impl EvidenceDb {
    pub fn cli(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env("ZEDICT_CACHE_DIR", self.dir.path().join("cache"))
            .current_dir(std::env::temp_dir());
        cmd.arg("--config").arg(&self.config);
        cmd
    }
}

// ───────────────────────── 离线假 HTTP 服务 (CI 无网络) ─────────────────────────
#[derive(Clone)]
pub struct FakeResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl FakeResponse {
    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into().into_bytes(),
        }
    }
    pub fn body(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
        }
    }
    pub fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: b"[]".to_vec(),
        }
    }
    /// OpenAI 兼容 SSE：每条事件一个 `data:` 块
    pub fn sse(deltas: &[&str]) -> Self {
        let mut body = String::new();
        for delta in deltas {
            let payload = json!({"choices":[{"delta":{"content":delta}}]}).to_string();
            body.push_str("data: ");
            body.push_str(&payload);
            body.push_str("\n\n");
        }
        body.push_str("data: [DONE]\n\n");
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: body.into_bytes(),
        }
    }
}

pub struct Seen {
    pub method: String,
    pub path: String,
    pub body: String,
}
struct FakeState {
    next: std::sync::Mutex<std::collections::VecDeque<FakeResponse>>,
    seen: std::sync::Mutex<Vec<Seen>>,
}

/// 一次连接处理一个请求并回 `Connection: close`，避免连接复用带来的不确定性
pub struct FakeHttp {
    pub base: String,
    state: std::sync::Arc<FakeState>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl FakeHttp {
    pub fn new(responses: Vec<FakeResponse>) -> Self {
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let state = std::sync::Arc::new(FakeState {
            next: std::sync::Mutex::new(responses.into()),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (worker_state, worker_stop) = (state.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => serve(socket, &worker_state, &worker_stop),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        // accept 在同一线程里轮询；端口在返回前已 bind，客户端连接不会失败
        Self {
            base: format!("http://{addr}"),
            state,
            stop,
            worker: Some(worker),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.state
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|s| Seen {
                method: s.method.clone(),
                path: s.path.clone(),
                body: s.body.clone(),
            })
            .collect()
    }

    pub fn count(&self) -> usize {
        self.state
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn bodies(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.body).collect()
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(mut socket: std::net::TcpStream, state: &FakeState, stop: &std::sync::atomic::AtomicBool) {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering;
    let _ = socket.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let _ = socket.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 4 * 1024 * 1024 || stop.load(Ordering::Relaxed) {
            return;
        }
        match socket.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("").to_string();
    let mut request = first.split_whitespace();
    let method = request.next().unwrap_or("GET").to_string();
    let path = request.next().unwrap_or("/").to_string();
    let length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body: Vec<u8> = buf[head_end..].to_vec();
    let mut stalls = 0;
    while body.len() < length && stalls < 30 {
        match socket.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => {
                stalls += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    let response = state
        .next
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pop_front()
        .unwrap_or_else(|| FakeResponse::status(500));
    state
        .seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Seen {
            method,
            path,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    let reason = if response.status == 200 { "OK" } else { "ERR" };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    let mut output = head.into_bytes();
    output.extend_from_slice(&response.body);
    let _ = socket.write_all(&output);
    let _ = socket.flush();
}

/// 缓存目录必须显式重定向：测试绝不能写进真实用户缓存。
/// 环境变量是进程级的，所以用全局锁把这些测试串行化。
static CACHE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub struct CacheDir {
    _dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl CacheDir {
    pub fn path(&self) -> &std::path::Path {
        self._dir.path()
    }
}

pub fn cache_dir() -> CacheDir {
    let guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("ZEDICT_CACHE_DIR", dir.path());
    CacheDir {
        _dir: dir,
        _guard: guard,
    }
}

// ─────────────────────── Edge 端点响应夹具 ───────────────────────
//
// 句长单位是 UTF-16 码元，且段落分隔符归在上一块里，所以数字一律现算，不抄进测试。
// 只有 srcSentLen 会被读；transSentLen 照真实响应一起给出，但夹具里它的值不参与配对。

/// 整块只返回一句的响应项（最省事的自洽夹具，译文必须只含一个句末标点）
pub fn edge_whole(source: &str, target: &str) -> serde_json::Value {
    json!({"translations": [{"text": target, "to": "zh-Hans", "sentLen": {
        "srcSentLen": [zedict::net::align::units(source)],
        "transSentLen": [zedict::net::align::units(target)],
    }}]})
}

pub fn edge_ok(items: Vec<serde_json::Value>) -> FakeResponse {
    FakeResponse::json(serde_json::to_string(&items).unwrap())
}

/// 与 CLI 默认 --batch-units 一致的分块，测试据此构造响应
pub fn edge_blocks(text: &str) -> Vec<String> {
    zedict::net::align::fit_blocks(text, 3600)
}
