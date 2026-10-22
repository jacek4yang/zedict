//! `zedict compile --spell` 建的删字符索引只有一条规矩要钉死：它是**补位**，
//! 不是第二条打分通路。所以每个测试都同时在两个库上跑——同一批记录，一个带表、
//! 一个不带，"不带"的结果必须是"带"的结果的前缀。
//!
//! 词头全是短词。长查询本来就有距离 2 的穷举扫描兜着，而删一个字符最多只差两步，
//! 所以这张表能带来的新候选只出现在 4 字以内的查询上。把断言建在这个事实上，
//! 才不会测试一个理论上存在、实际永远走不到的分支。

mod common;
use common::SpellFixture;
use serde_json::Value;
use std::process::Command;
use zedict::data::store::DictStore;

fn run(f: &SpellFixture, with: bool, args: &[&str]) -> (Option<i32>, String, String) {
    let out = f.cli(with).args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("stdout 必须是 UTF-8"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `--json` 的候选列表，按输出顺序读回 (词, 距离)。零命中时退出码是 3，
/// 但那是"没查到"，不是"输出坏了"，两种码都要能读。
fn suggestions(f: &SpellFixture, with: bool, query: &str) -> Vec<(String, usize)> {
    let (code, stdout, stderr) = run(f, with, &["query", query, "--mode", "fuzzy", "--json"]);
    assert!(
        matches!(code, Some(0) | Some(3)),
        "退出码 {code:?}，stderr: {stderr}"
    );
    let out: Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("不是 JSON: {stdout}（{e}）"));
    out["suggestions"]
        .as_array()
        .unwrap_or_else(|| panic!("suggestions 必须是数组: {out}"))
        .iter()
        .map(|item| {
            (
                item["word"].as_str().unwrap().to_string(),
                item["distance"].as_u64().unwrap() as usize,
            )
        })
        .collect()
}

fn words(list: &[(String, usize)]) -> Vec<&str> {
    list.iter().map(|(word, _)| word.as_str()).collect()
}

fn spawn(config: &std::path::Path) -> Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
    cmd.env_remove("ZEDICT_DB")
        .env_remove("ZEDICT_CONFIG")
        .env("ZEDICT_CACHE_DIR", std::env::temp_dir())
        .current_dir(std::env::temp_dir())
        .arg("--config")
        .arg(config);
    cmd
}

#[test]
fn a_two_step_typo_on_a_short_word_reaches_the_headword_only_through_the_index() {
    let f = SpellFixture::new();
    // `ctx` 和 `cat` 各删一个字符都会落到 `ct` 上，所以这张表能把它们连起来；
    // 但它们的编辑距离是 2，而 4 字以下的查询只允许 1 步——不带的库必须给不出。
    assert!(
        !words(&suggestions(&f, false, "ctx")).contains(&"cat"),
        "不带表时主扫描不该给出 cat"
    );
    let with = suggestions(&f, true, "ctx");
    assert!(
        with.contains(&("cat".to_string(), 2)),
        "补位应给出 distance 2 的 cat: {with:?}"
    );
    // 补位候选是直达词头，不是绕词形表来的原形，所以 via 必须是空。
    let (code, stdout, _) = run(&f, true, &["query", "ctx", "--mode", "fuzzy", "--json"]);
    assert_eq!(code, Some(0), "{stdout}");
    let out: Value = serde_json::from_str(&stdout).unwrap();
    let cat = out["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["word"] == Value::String("cat".into()))
        .expect("cat 应在候选里");
    assert!(cat["via"].is_null(), "via: {}", cat["via"]);
    // 反过来，一步就能补回来的写法本来就在穷举扫描里：`bll` 补一个 a 就是 `ball`。
    // 这张表对它没有任何新增，两边的列表必须一模一样。
    assert_eq!(
        words(&suggestions(&f, false, "bll")),
        words(&suggestions(&f, true, "bll")),
        "主扫描已给出的候选不该被补位改动"
    );
}

#[test]
fn the_backfill_never_displaces_a_primary_candidate() {
    let f = SpellFixture::new();
    for query in ["bll", "all", "bal", "bl", "bad", "cat", "certificat", "xyz"] {
        let without = suggestions(&f, false, query);
        let with = suggestions(&f, true, query);
        assert!(
            with.len() >= without.len(),
            "{query}: 补位把候选变少了: {without:?} vs {with:?}"
        );
        assert_eq!(
            words(&without),
            words(&with[..without.len()]),
            "{query}: 已有候选的顺序被改动"
        );
        // 补进来的那几条距离必须真的比主通路的最后一条更远或相等——否则它们
        // 本来就该由穷举扫描给出，这张表的存在也就没有意义。
        if let Some(last) = without.last() {
            for extra in &with[without.len()..] {
                assert!(
                    extra.1 >= last.1,
                    "{query}: 补位候选 {extra:?} 比主通路最后一条 {last:?} 更近"
                );
            }
        }
    }
}

#[test]
fn the_index_holds_one_neighbourhood_per_headword_and_nothing_else() {
    let f = SpellFixture::new();
    // 每个词头一行自指 + 每个字符删一次（重复的删法算一条）：
    // ball 1+3、bad 1+3、cat 1+3、certificate 1+11（无相邻同字符，11 种删法各不同），
    // 单字的 a 整条不进表。合计 24。
    assert_eq!(f.rows, 24, "spell_key 行数");
    let store = DictStore::open(&f.path("with.sqlite")).unwrap();
    assert!(store.has_spell());
    let spell = store.datasets().unwrap()["spell"].clone();
    assert_eq!(spell["derived_from"], "entries");
    assert_eq!(spell["words"], 4, "单字词头不该计入");
    assert_eq!(spell["rows"], 24);
}

#[test]
fn a_library_without_the_table_is_todays_library() {
    let f = SpellFixture::new();
    let without = DictStore::open(&f.path("without.sqlite")).unwrap();
    assert!(!without.has_spell());
    // 精确查询完全不碰这张表，两个库的输出必须逐字节一致。
    for args in [
        vec!["query", "ball", "--json"],
        vec!["query", "certificate", "--json"],
    ] {
        let left = spawn(&f.path("with.toml")).args(&args).output().unwrap();
        let right = spawn(&f.path("without.toml")).args(&args).output().unwrap();
        assert_eq!(left.status.code(), right.status.code(), "{args:?}");
        assert_eq!(left.stdout, right.stdout, "{args:?}");
        assert!(!String::from_utf8_lossy(&left.stdout).contains('\u{1b}'));
    }
    // doctor 要如实说这张表在不在：datasets 里有记录不等于表建了。
    for (with, expected) in [(true, true), (false, false)] {
        let (code, stdout, stderr) = run(&f, with, &["doctor", "--json"]);
        assert_eq!(code, Some(0), "{stderr}");
        let out: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(out["tables"]["spell"], Value::Bool(expected), "{stdout}");
    }
}

#[test]
fn a_query_with_no_neighbour_never_dumps_the_table() {
    let f = SpellFixture::new();
    // 空 delkey 要是进了表，任何查询都能在上面会师，整张词头表就都端出来了。
    // 单字词头是唯一能造出空 delkey 的来源，所以这里直接把它卡住。
    assert!(suggestions(&f, true, "xyz").is_empty(), "xyz 不该有候选");
    assert!(suggestions(&f, true, "qqqq").is_empty(), "qqqq 不该有候选");
    // 单字词头本身照旧能精确查到——跳过它只是不进删字符表，不是从库里消失。
    let (code, _, _) = run(&f, true, &["query", "a", "--json"]);
    assert_eq!(code, Some(0), "单字词头应仍命中");
}
