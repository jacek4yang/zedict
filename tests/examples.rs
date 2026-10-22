//! `zedict examples`：例句索引是词卡例句的侧表，所以这里两头都要钉住——
//! 关键词反查（含中文侧）与全文扫描各走各的路径，`matched_by` 必须如实报告。
//!
//! 夹具把噪声阈值卡在两侧：`噪声词` 挂 301 句要被剔除，`边界词` 正好 300 句要留下。
//! 阈值写成 `>=` 还是 `>`，这两个断言里就会有一个失败。

mod common;
use common::{ExamplesFixture, LevelFixture};
use serde_json::Value;
use zedict::data::store::DictStore;

fn run(f: &ExamplesFixture, args: &[&str]) -> (Option<i32>, String, String) {
    let out = f.cli().args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("stdout 必须是 UTF-8"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(f: &ExamplesFixture, args: &[&str]) -> Value {
    let mut full = args.to_vec();
    full.push("--json");
    let (code, stdout, stderr) = run(f, &full);
    assert_eq!(code, Some(0), "{stderr}");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("输出不是 JSON: {stdout}（{e}）"))
}

fn sentences(out: &Value) -> Vec<String> {
    out["examples"]
        .as_array()
        .expect("examples 必须是数组")
        .iter()
        .map(|hit| hit["en"].as_str().unwrap().to_string())
        .collect()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("这里应该是字符串数组: {value}"))
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

fn datasets(store: &DictStore) -> Value {
    store.datasets().unwrap()
}

#[test]
fn the_build_counts_every_pair_and_reports_what_it_dropped() {
    let f = ExamplesFixture::new();
    // 3 + 1 + 1 + 301 + 300 句；每句 1 条英文键 + 1 条中文键 + 1 条词头链，
    // 只有 bare 那句没标记，只剩词头一条。
    assert_eq!(f.rows, 606);
    assert_eq!(f.links, 1_515);
    assert_eq!(f.dropped_keys, 1);
    // 剔掉的键要在库里点名，否则这一步就是无痕清洗。
    let store = DictStore::open(&f.db).unwrap();
    let examples = datasets(&store)["examples"].clone();
    let dropped = examples["dropped_keys"]
        .as_array()
        .expect("datasets 里应有被剔除的中文键");
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0]["key"], "噪声词");
    assert_eq!(dropped[0]["examples"], 301);
    assert_eq!(examples["noisy_key_threshold"], 300);
    assert_eq!(examples["dropped_noisy_links"], 301);
    assert_eq!(examples["rows"], 606);
    assert_eq!(examples["links"], 1_515);
    // 悬空与无链都在建库时拦下来，这里只是把闸门跑一遍。
    store.check_integrity().unwrap();
}

#[test]
fn a_chinese_keyword_finds_back_the_english_it_was_marked_in() {
    let f = ExamplesFixture::new();
    let out = json(&f, &["examples", "病假条"]);
    assert_eq!(out["matched_by"], "key");
    assert_eq!(out["matches"], 1);
    assert_eq!(
        sentences(&out),
        ["Please write me a certificate for sick leave".to_string()]
    );
    assert_eq!(out["examples"][0]["zh"], "请给我开一张病假条");
    assert_eq!(strings(&out["examples"][0]["zh_keys"]), ["病假条"]);
    assert_eq!(strings(&out["examples"][0]["headwords"]), ["sick"]);

    // 关键词是精确的，子串不是：`病假` 只挂在第二句上，但两句正文里都有它。
    let store = DictStore::open(&f.db).unwrap();
    assert_eq!(store.example_key_matches("病假").unwrap(), 1);
    assert_eq!(store.example_scan_matches("病假").unwrap(), 2);
    let out = json(&f, &["examples", "病假"]);
    assert_eq!(out["matched_by"], "key");
    assert_eq!(
        sentences(&out),
        ["He is on sick leave this week".to_string()]
    );
}

#[test]
fn a_phrase_that_is_not_a_headword_is_still_a_key() {
    let f = ExamplesFixture::new();
    let store = DictStore::open(&f.db).unwrap();
    assert!(
        store.lookup("in spite of").unwrap().is_none(),
        "夹具里 in spite of 不该是词头"
    );
    let out = json(&f, &["examples", "in spite of"]);
    assert_eq!(out["matched_by"], "key");
    assert_eq!(out["matches"], 1);
    // 关键词入库前统一 normalize 过，所以大小写不同的查询也能命中。
    assert_eq!(strings(&out["examples"][0]["keys"]), ["in spite of"]);
    let out = json(&f, &["examples", "In Spite Of"]);
    assert_eq!(out["matched_by"], "key");
    assert_eq!(
        sentences(&out),
        ["In spite of the rain we went out".to_string()]
    );
}

#[test]
fn a_substring_with_no_key_at_all_falls_back_to_scanning() {
    let f = ExamplesFixture::new();
    let out = json(&f, &["examples", "went out"]);
    assert_eq!(out["matched_by"], "scan");
    assert_eq!(out["matches"], 1);
    assert_eq!(
        sentences(&out),
        ["In spite of the rain we went out".to_string()]
    );
    // 扫描是全文的，所以中文半区里的片段也算命中——即使它比关键词还长。
    let out = json(&f, &["examples", "噪声词第7条说法"]);
    assert_eq!(out["matched_by"], "scan");
    assert_eq!(out["matches"], 1);
    assert_eq!(out["rows"], 1);

    // 关键词和正文两边都没有，才算未命中。
    let (code, stdout, _) = run(&f, &["examples", "这句话夹具里没有 zzz"]);
    assert_eq!(code, Some(3), "{stdout}");
    assert!(stdout.contains("例句库里没有"), "{stdout}");
}

#[test]
fn a_key_hanging_off_too_many_examples_is_dropped_but_its_sentences_stay() {
    let f = ExamplesFixture::new();
    let store = DictStore::open(&f.db).unwrap();
    assert_eq!(store.example_key_matches("噪声词").unwrap(), 0);
    // 剔的只是关键词，句子还在库里：查询自动退到全文扫描，不能报"没找到"。
    let out = json(&f, &["examples", "噪声词"]);
    assert_eq!(out["matched_by"], "scan");
    assert_eq!(out["matches"], 301);
    assert_eq!(out["rows"], 8);
    // 正好压在阈值上的键不受影响。
    let out = json(&f, &["examples", "边界词"]);
    assert_eq!(out["matched_by"], "key");
    assert_eq!(out["matches"], 300);
    assert_eq!(store.example_key_matches("边界词").unwrap(), 300);
}

#[test]
fn a_missing_index_is_a_build_problem_not_a_miss() {
    let f = LevelFixture::new();
    let out = f.cli().args(["examples", "world"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("没有例句索引"), "{stderr}");
    assert!(stderr.contains("zedict compile --examples"), "{stderr}");
}

#[test]
fn a_blank_query_is_rejected_before_the_dictionary_is_opened() {
    let f = ExamplesFixture::new();
    let (code, _, stderr) = run(&f, &["examples", "   "]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("参数错误"), "{stderr}");
}

#[test]
fn human_output_pairs_the_two_halves_and_offers_a_copyable_next_step() {
    let f = ExamplesFixture::new();
    let (code, stdout, stderr) = run(&f, &["examples", "病假条", "--full"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("1 句命中，显示 1 句（关键词）"), "{stdout}");
    assert!(stdout.contains("a certificate for sick leave"), "{stdout}");
    assert!(stdout.contains("请给我开一张病假条"), "{stdout}");
    assert!(stdout.contains("相关词条: sick"), "{stdout}");
    assert!(
        stdout.contains(
            "zedict explain sick --in-context \"Please write me a certificate for sick leave\""
        ),
        "{stdout}"
    );
    assert!(!stdout.contains('\u{1b}'), "stdout 不得带 ANSI");
    // --limit 只裁显示，候选总数仍然要说实话。
    let out = json(&f, &["examples", "边界词", "--limit", "2"]);
    assert_eq!(out["rows"], 2);
    assert_eq!(out["matches"], 300);
}

#[test]
fn a_full_query_shows_the_word_in_other_cards_examples_only() {
    let f = ExamplesFixture::new();
    // bare 出现在 sick 那张卡的例句里（关键词却是 sick），所以查 bare 要能看见。
    let (code, stdout, _) = run(&f, &["examples", "bare"]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(
        !stdout.contains("Even a bare wall"),
        "bare 作为关键词只有它自己那句: {stdout}"
    );
    let (code, stdout, stderr) = run(&f, &["query", "bare"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("其他词条中的例句（含 bare）"), "{stdout}");
    assert!(
        stdout.contains("Even a bare wall looks clean when you are sick"),
        "{stdout}"
    );

    // 自己卡里的例句不能重复端上来。
    let (_, stdout, _) = run(&f, &["query", "sick"]);
    assert!(!stdout.contains("其他词条中的例句"), "{stdout}");

    // JSON 是对外契约，反查只加在人类可读输出上。
    let (code, stdout, _) = run(&f, &["query", "bare", "--json"]);
    assert_eq!(code, Some(0));
    assert!(!stdout.contains("其他词条中的例句"), "{stdout}");
    let _: Value = serde_json::from_str(&stdout).unwrap();

    // 没有索引的旧词库，查询输出一个字都不能变。
    let plain = LevelFixture::new();
    let out = plain
        .cli()
        .args(["query", "world", "--full"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("其他词条中的例句"));
}
