//! `--key-evidence` 的唯一职责是：只改"哪条记录能进 `entries`"这一件事。
//! 归档、义项检索、档位普查在三种取值下必须一模一样——净化不能伪装成"数据没了"，
//! 也不能顺手把别的表改出行为差异。
//!
//! 另一头是 `--vocab`：那张表唯一允许影响的是**文本输出里多一行提示**。JSON 的键
//! 集合、退出码、`--json` 的字节，都不能因为它而变。

mod common;
use common::{build_evidence, build_evidence_verified, EvidenceDb};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use zedict::core::evidence::KeyEvidence;
use zedict::data::store::DictStore;

/// 三个阈值的库共用一个父目录，好在同一个 `DictStore` 形状上横向比对。
struct Tiers {
    all: EvidenceDb,
    some: EvidenceDb,
    strong: EvidenceDb,
    /// 唯一用途是让三个子 tempdir 的父目录活得比它们久——字段按声明顺序析构，
    /// 所以它必须写在最后。
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

impl Tiers {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let all = build_evidence("all", dir.path(), KeyEvidence::All, false);
        let some = build_evidence("some", dir.path(), KeyEvidence::Some, true);
        let strong = build_evidence("strong", dir.path(), KeyEvidence::Strong, true);
        Self {
            dir,
            all,
            some,
            strong,
        }
    }
    fn store(&self, which: &EvidenceDb) -> DictStore {
        DictStore::open(&which.db).unwrap()
    }
}

fn spawn(config: &Path) -> Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
    cmd.env_remove("ZEDICT_DB")
        .env_remove("ZEDICT_CONFIG")
        .env_remove("NO_COLOR")
        .env("ZEDICT_CACHE_DIR", std::env::temp_dir())
        .current_dir(std::env::temp_dir())
        .arg("--config")
        .arg(config);
    cmd
}

fn run(f: &EvidenceDb, args: &[&str]) -> (Option<i32>, String, String) {
    let out = f.cli().args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("stdout 必须是 UTF-8"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn hits(f: &EvidenceDb, query: &str) -> Vec<String> {
    let (code, stdout, stderr) = run(f, &["query", query, "--json"]);
    assert!(
        matches!(code, Some(0) | Some(3)),
        "{query}: 退出码 {code:?}，stderr: {stderr}"
    );
    let out: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{query}: {e}"));
    out["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["key"].as_str().unwrap().to_string())
        .collect()
}

fn rows(f: &EvidenceDb, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{} 表读不出来: {e}", table))
}

#[test]
fn a_threshold_changes_which_cards_are_indexed_and_nothing_else() {
    let t = Tiers::new();
    // 档位普查读的是源记录，跟阈值无关：三种取值必须报同一组数。
    for f in [&t.all, &t.some, &t.strong] {
        assert_eq!(f.report.tier_example_key, 1, "{:?}", f.report);
        assert_eq!(f.report.tier_in_gloss, 1);
        assert_eq!(f.report.tier_unevidenced, 1);
        assert_eq!(f.report.archived_records, 3, "净化不能少归档一条");
        assert_eq!(f.report.raw_records, 3);
    }
    assert_eq!(t.all.report.dropped_unevidenced_cards, 0);
    assert_eq!(t.some.report.dropped_unevidenced_cards, 1);
    assert_eq!(t.strong.report.dropped_unevidenced_cards, 2);

    // 阈值只允许决定 E 档（以及 strong 下的 D 档）进不进 `entries`。
    assert_eq!(rows(&t.all, "entries"), 3);
    assert_eq!(rows(&t.some, "entries"), 2);
    assert_eq!(rows(&t.strong, "entries"), 1);
    assert_eq!(
        rows(&t.all, "source_records"),
        3,
        "归档在三种取值下都要覆盖全部行"
    );
    assert_eq!(rows(&t.some, "source_records"), 3);
    assert_eq!(rows(&t.strong, "source_records"), 3);
    assert_eq!(rows(&t.some, "key_evidence"), 3, "档位表要覆盖每一行");
    assert_eq!(rows(&t.strong, "key_evidence"), 3);
    // `angle` 是 E 档：两个阈值下它都不该查得到，而 `sick` 在哪一档都在。
    assert_eq!(hits(&t.all, "angle"), ["angle"]);
    assert!(hits(&t.some, "angle").is_empty());
    assert!(hits(&t.strong, "angle").is_empty());
    for f in [&t.all, &t.some, &t.strong] {
        assert_eq!(hits(f, "sick"), ["sick"]);
    }
    // 只有 D 档会被 `strong` 单独挡掉——这是 `some` 与 `strong` 的唯一区别。
    assert_eq!(hits(&t.some, "promotion"), ["promotion"]);
    assert!(hits(&t.strong, "promotion").is_empty());

    for f in [&t.all, &t.some, &t.strong] {
        t.store(f).check_integrity().unwrap();
    }
}

#[test]
fn the_tier_table_exists_exactly_when_a_threshold_was_applied() {
    let t = Tiers::new();
    assert!(!t.store(&t.all).has_key_evidence());
    assert!(t.store(&t.some).has_key_evidence());
    assert!(t.store(&t.strong).has_key_evidence());
    // doctor 报的是"表在不在"，不是"数据集里有没有记录"。
    for (f, expected) in [(&t.all, false), (&t.some, true), (&t.strong, true)] {
        let (code, stdout, stderr) = run(f, &["doctor", "--json"]);
        assert_eq!(code, Some(0), "{stderr}");
        let out: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            out["tables"]["key_evidence"],
            Value::Bool(expected),
            "{stdout}"
        );
        assert_eq!(out["tables"]["vocab"], Value::Bool(expected), "{stdout}");
    }
}

/// 人工核验队列是这套判据唯一的对外出口：打开 `queue.jsonl` 的人必须当场看到这
/// 条记录的词头凭什么自证，而不是自己再去比一遍例句。
#[test]
fn the_audit_queue_says_what_makes_each_headword_self_evident() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jsonl");
    common::write_evidence_source(&source);
    let out = dir.path().join("review");
    let report = zedict::data::ingest::audit(&source, &out).unwrap();
    assert_eq!(report["key_tier_counts"]["example-key"], 1, "{report}");
    assert_eq!(report["key_tier_counts"]["in-gloss"], 1);
    assert_eq!(report["key_tier_counts"]["unevidenced"], 1);
    assert_eq!(report["candidate_headwords"], 3);
    // 只有 E 档的键，才是"净化后会彻底消失"的那一批。
    assert_eq!(report["keys_only_unevidenced"], 1);
    assert!(
        report["high_cardinality_candidates"]
            .as_array()
            .unwrap()
            .is_empty(),
        "三条记录各一个词头，没有谁超过 20 次: {report}"
    );

    let queue = std::fs::read_to_string(out.join("queue.jsonl")).unwrap();
    let rows: Vec<Value> = queue
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    let row = |headword: &str| {
        rows.iter()
            .find(|row| row["candidate_headword"] == headword)
            .unwrap_or_else(|| panic!("队列里没有 {headword}: {rows:?}"))
    };
    assert_eq!(row("sick")["key_tier"], "example-key");
    assert_eq!(row("promotion")["key_tier"], "in-gloss");
    assert_eq!(row("angle")["key_tier"], "unevidenced");
    // 每档一句不同的话，而只有 E 档那句会说"它可能就是隔壁词条的词"。
    let notes: Vec<&str> = rows
        .iter()
        .map(|row| row["note"].as_str().unwrap())
        .collect();
    assert_eq!(
        notes.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "{notes:?}"
    );
    assert!(row("angle")["note"].as_str().unwrap().contains("隔壁词条"));
    for headword in ["sick", "promotion"] {
        assert!(
            !row(headword)["note"].as_str().unwrap().contains("隔壁词条"),
            "{headword} 是自证的，不该被说成错卡: {:?}",
            row(headword)["note"]
        );
    }

    // 没有词头的记录没有档位可评：给它 `null` 和另一句话。
    let bare = dir.path().join("bare.jsonl");
    std::fs::write(
        &bare,
        format!(
            "{}\n{}\n",
            common::evidence_records()[0],
            r#"{"senses":["没有词头的一条"]}"#
        ),
    )
    .unwrap();
    let bare_out = dir.path().join("bare-review");
    let bare_report = zedict::data::ingest::audit(&bare, &bare_out).unwrap();
    assert_eq!(bare_report["records_without_headword"], 1, "{bare_report}");
    assert_eq!(
        bare_report["key_tier_counts"]["example-key"], 1,
        "缺词头不该被算成 unevidenced: {bare_report}"
    );
    assert!(
        bare_report["key_tier_counts"]["unevidenced"].is_null(),
        "{bare_report}"
    );
    let bare_rows: Vec<Value> = std::fs::read_to_string(bare_out.join("queue.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let no_head = bare_rows
        .iter()
        .find(|row| row["candidate_headword"] == "")
        .expect("队列里要有那条无词头记录");
    assert!(no_head["key_tier"].is_null(), "{no_head}");
    let note = no_head["note"].as_str().unwrap();
    assert!(note.contains("没有词头"), "{note}");
    assert!(!note.contains("隔壁词条"), "无词头不等于错词: {note}");
}

/// 老词库（阈值 `all`，没有这张表）必须和今天逐字节一致，且自动术语那条路在缺表
/// 时是"什么都不给"，不是"那就不看档位了"。
#[test]
fn a_library_without_the_table_is_todays_library() {
    let t = Tiers::new();
    let store = t.store(&t.all);
    assert!(!store.has_key_evidence());
    for evidence in [KeyEvidence::All, KeyEvidence::Some, KeyEvidence::Strong] {
        assert!(
            store
                .evidenced_keys(&["sick".into()], evidence)
                .unwrap()
                .is_empty(),
            "缺表时 {evidence:?} 不该给出任何术语"
        );
    }
    // 没被净化过的库里，`angle` 那条错卡照旧在——阈值 `all` 就是今天的行为。
    assert_eq!(hits(&t.all, "angle"), ["angle"]);
    for args in [
        vec!["query", "sick", "--json"],
        vec!["query", "angle", "--json"],
        vec!["query", "promotion", "--json"],
    ] {
        let left = spawn(&t.all.config).args(&args).output().unwrap();
        let right = spawn(&t.strong.config).args(&args).output().unwrap();
        // `sick` 三档都在库里，两个库的输出必须一个字节都不差。
        if args[1] == "sick" {
            assert_eq!(left.status.code(), right.status.code(), "{args:?}");
            assert_eq!(left.stdout, right.stdout, "{args:?}");
        }
        assert!(
            !String::from_utf8_lossy(&left.stdout).contains('\u{1b}'),
            "{args:?}: stdout 不许有 ANSI"
        );
    }
}

#[test]
fn evidence_gates_which_glosses_may_be_pinned_into_a_prompt() {
    let t = Tiers::new();
    let store = t.store(&t.some);
    // C 档在任何阈值下都算证据；D 档只算"some"的证据；两者都不许 E 档混进来
    //（`some` 库里已经没有 E 档行了，所以 `angle` 在这里查不到档位）。
    let sick = store
        .evidenced_keys(&["sick".into()], KeyEvidence::Strong)
        .unwrap();
    assert_eq!(sick.len(), 1);
    assert_eq!(sick[0].0, "sick");
    assert!(sick[0].1.contains("生病"), "释义 {:?}", sick[0].1);
    let promotion = store
        .evidenced_keys(&["promotion".into()], KeyEvidence::Some)
        .unwrap();
    assert_eq!(promotion.len(), 1, "D 档在 some 下可用: {promotion:?}");
    assert!(promotion[0].1.contains("促销"), "释义 {:?}", promotion[0].1);
    assert!(
        store
            .evidenced_keys(&["promotion".into()], KeyEvidence::Strong)
            .unwrap()
            .is_empty(),
        "D 档释义不许钉进 strong 术语表"
    );
    assert!(
        store
            .evidenced_keys(&["angle".into()], KeyEvidence::All)
            .unwrap()
            .is_empty(),
        "库里没有的行不该给出任何术语"
    );
}

/// 人工核验优先于任何自动判据：`strong` 挡住的 E 档记录，签过就照进。
#[test]
fn a_verified_record_beats_the_strictest_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let strong = build_evidence_verified("strong", dir.path(), KeyEvidence::Strong);
    assert_eq!(strong.report.tier_example_key, 1, "{:?}", strong.report);
    assert_eq!(strong.report.tier_in_gloss, 1);
    assert_eq!(strong.report.tier_unevidenced, 0, "第 3 行已被核验覆盖");
    assert_eq!(strong.report.dropped_unevidenced_cards, 1);
    assert_eq!(strong.report.indexed_cards, 2, "C 档 + 核验过的 E 档");
    assert_eq!(hits(&strong, "angle"), ["angle"]);
    assert!(
        hits(&strong, "promotion").is_empty(),
        "没签过的 D 档照样进不来"
    );
    let store = DictStore::open(&strong.db).unwrap();
    store.check_integrity().unwrap();
    // 核验过的记录在 `strong` 术语表里也是合法的——它是人确认过的译法。
    assert_eq!(
        store
            .evidenced_keys(&["angle".into()], KeyEvidence::Strong)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_vocab_hint_adds_a_line_and_changes_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let with = build_evidence("with", dir.path(), KeyEvidence::Strong, true);
    let without = build_evidence("without", dir.path(), KeyEvidence::Strong, false);
    assert!(DictStore::open(&with.db).unwrap().has_vocab());
    assert!(!DictStore::open(&without.db).unwrap().has_vocab());
    assert_eq!(with.report.vocab_rows, 5, "去重归一后五个词");
    assert_eq!(without.report.vocab_rows, 0);

    // 命中时一句都不多说。
    for f in [&with, &without] {
        let (code, stdout, _) = run(f, &["query", "sick"]);
        assert_eq!(code, Some(0));
        assert!(!stdout.contains("本地词库暂未收录"), "{stdout}");
    }
    // 未命中、但词表里有这个词：只多这一行文本。
    let (code, stdout, _) = run(&with, &["query", "promotion"]);
    assert_eq!(code, Some(3), "提示不改变退出码: {stdout}");
    assert!(
        stdout.contains("“promotion”是很常见的英文词，但本地词库暂未收录"),
        "{stdout}"
    );
    assert!(!stdout.contains('\u{1b}'));
    let (_, plain, _) = run(&without, &["query", "promotion"]);
    assert!(
        stdout.starts_with(&plain),
        "提示只能追加在原输出之后:\n--- 无表 ---\n{plain}--- 有表 ---\n{stdout}"
    );
    assert_eq!(
        stdout.lines().count(),
        plain.lines().count() + 1,
        "只该多出一行提示:\n{stdout}"
    );
    // 词表里没有的词，不能凭空断言"它是常见词"。
    let (_, stdout, _) = run(&with, &["query", "notawordinvocab"]);
    assert!(!stdout.contains("本地词库暂未收录"), "{stdout}");
    // 大写查询走的是同一套归一：词表存的是 `receive`。
    let (_, stdout, _) = run(&with, &["query", "RECEIVE"]);
    assert!(
        stdout.contains("“RECEIVE”是很常见的英文词，但本地词库暂未收录"),
        "{stdout}"
    );
    // --json 完全不知道有这行提示：键集合和字节都不能变。
    for query in ["promotion", "sick", "notawordinvocab"] {
        let left = spawn(&with.config)
            .args(["query", query, "--json"])
            .output()
            .unwrap();
        let right = spawn(&without.config)
            .args(["query", query, "--json"])
            .output()
            .unwrap();
        assert_eq!(left.status.code(), right.status.code(), "{query}");
        if query == "sick" {
            assert_eq!(left.stdout, right.stdout, "{query}: JSON 字节变了");
        }
        let shape = |bytes: &[u8]| {
            let mut keys: Vec<String> =
                serde_json::from_str::<Value>(&String::from_utf8_lossy(bytes))
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect();
            keys.sort();
            keys
        };
        assert_eq!(shape(&left.stdout), shape(&right.stdout), "{query}");
    }
}
