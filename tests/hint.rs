//! `zedict hint`：本地词级比对、原文标注、参数校验，以及词级加权后的拼写建议。
//!
//! 夹具里的 `running`/`axes` 只有词形关系、没有自己的词头，专门用来验两条回退通路：
//! 档位从原形借（`running`→`run`），以及拼错的词形回到原形卡。

mod common;
use common::{Fixture, LevelFixture};
use serde_json::Value;
use zedict::data::store::DictStore;

fn run(f: &LevelFixture, args: &[&str]) -> (Option<i32>, String, String) {
    let out = f.cli().args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("stdout 必须是 UTF-8"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(f: &LevelFixture, args: &[&str]) -> Value {
    let mut full = args.to_vec();
    full.push("--json");
    let (code, stdout, stderr) = run(f, &full);
    assert_eq!(code, Some(0), "{stderr}");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("输出不是 JSON: {stdout}（{e}）"))
}

const TEXT: &str = "world ephemeral provisional zealous qwertyuiop running";

fn item_words(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.contains('['))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect()
}

/// 去掉 `〖…〗` 整段记号，还原本来的文字。
fn strip_markers(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '〖' => depth += 1,
            '〗' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 目标档 2：world(1) 和 running(经 run=2) 在纲，其余四种"生词"各有各的原因。
#[test]
fn each_out_of_range_word_reports_why_it_is_one() {
    let f = LevelFixture::new();
    assert_eq!(f.word_levels, 6);
    let (code, out, err) = run(&f, &["hint", TEXT, "--level", "2"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        out.contains("在纲 2；生词 4：超档 1、仅大纲 1、词库有无档 1、两处都无 1"),
        "{out}"
    );
    assert_eq!(
        item_words(&out),
        ["ephemeral", "provisional", "zealous", "qwertyuiop"]
    );
    let lines: Vec<&str> = out.lines().filter(|l| l.contains('[')).collect();
    // 分组顺序＝该不该先看：超档 → 仅大纲 → 词库有无档 → 两处都无
    assert!(
        lines[0].contains("档 9") && lines[0].contains("gre") && lines[0].contains("adj. 短暂的")
    );
    assert!(lines[1].contains("无档") && lines[1].contains("仅大纲"));
    assert!(lines[2].contains("词库有无档") && lines[2].contains("adj. 热心的"));
    assert!(lines[3].contains("两处都无"));
    assert!(!out.contains('\u{1b}'));

    // 词级来自原形时要说清楚是从哪个原形借的
    let (_, via, _) = run(&f, &["hint", "running", "--level", "1"]);
    assert!(via.contains("档 2") && via.contains("←run"), "{via}");
    assert!(via.contains("超档"), "{via}");
}

#[test]
fn json_carries_the_band_self_description_and_every_count() {
    let f = LevelFixture::new();
    let v = json(&f, &["hint", TEXT, "--level", "2"]);
    assert_eq!(v["threshold"], 2);
    assert_eq!(v["band"]["desc"], "COCA等级 2000 - 4000");
    assert_eq!(v["band"]["level"], 2);
    assert_eq!(v["tokens"], 6);
    assert_eq!(v["units"], 6);
    assert_eq!(v["distinct"], 6);
    assert_eq!(v["phrase_units"], 0);
    assert_eq!(v["counts"]["below"], 2);
    assert_eq!(v["counts"]["above"], 1);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0]["word"], "ephemeral");
    assert_eq!(items[0]["status"], "above");
    assert_eq!(items[0]["level"], 9);
    assert_eq!(items[0]["tags"][0], "gre");
    assert_eq!(items[1]["status"], "tagged");
    assert_eq!(items[2]["status"], "unranked");
    assert_eq!(items[2]["gloss"], "adj. 热心的");
    assert_eq!(items[3]["status"], "unknown");
    // level / via_lemma / tags / gloss 都是"有就说，没有就不写"，不写 null 占位
    let last = items[3].as_object().unwrap();
    for key in ["level", "gloss", "tags", "via_lemma"] {
        assert!(!last.contains_key(key), "{last:?}");
    }
    // 档位越界时 band 查不到自述，字段直接不出现
    assert!(!json(&f, &["hint", TEXT, "--level", "10"])
        .as_object()
        .unwrap()
        .contains_key("band"));
}

/// 标注必须逐字保留原文，只在每个统计单位后面加记号。
#[test]
fn annotate_marks_every_unit_without_rewriting_the_text() {
    let f = LevelFixture::new();
    let (code, out, err) = run(&f, &["hint", TEXT, "--level", "2", "--annotate"]);
    assert_eq!(code, Some(0), "{err}");
    let marked = out
        .split("标注:\n")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .unwrap_or_else(|| panic!("没有标注行: {out}"));
    assert_eq!(
        marked,
        "world〖1〗 ephemeral〖9〗 provisional〖gre〗 zealous〖?〗 qwertyuiop〖?〗 running〖2〗"
    );
    assert_eq!(strip_markers(marked), TEXT);
    assert!(!out.contains('\u{1b}'), "输出里不许有 ANSI 转义");
    // 不要求标注时不产生标注段
    assert!(!json(&f, &["hint", TEXT])
        .as_object()
        .unwrap()
        .contains_key("annotated"));
}

#[test]
fn only_unknown_drops_the_words_the_level_table_can_place() {
    let f = LevelFixture::new();
    let (code, out, err) = run(&f, &["hint", TEXT, "--level", "2", "--only-unknown"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(item_words(&out), ["zealous", "qwertyuiop"]);
    // 首行统计不动：过滤的是清单，不是账
    assert!(out.contains("生词 4"), "{out}");
    let v = json(&f, &["hint", TEXT, "--level", "2", "--only-unknown"]);
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    assert_eq!(v["counts"]["above"], 1);
}

/// 词库收录的多词条目按一个词计，否则 "take into account" 会被数成 4 个生词。
#[test]
fn include_phrase_collapses_library_phrases_only() {
    let f = LevelFixture::new();
    let phrase = "take into account the world";
    let plain = json(&f, &["hint", phrase, "--level", "2"]);
    let grouped = json(&f, &["hint", phrase, "--level", "2", "--include-phrase"]);
    assert_eq!(plain["tokens"], 5);
    assert_eq!(plain["units"], 5);
    assert_eq!(plain["phrase_units"], 0);
    assert_eq!(grouped["units"], 3);
    assert_eq!(grouped["phrase_units"], 1);
    assert_eq!(grouped["distinct"], 3);
    // 短语整体一个单位：词级表没有它，但词头有，所以算"词库有无档"，不拆成四个词
    let words: Vec<&str> = grouped["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["word"].as_str().unwrap())
        .collect();
    assert_eq!(words, ["take into account", "the"]);
    assert_eq!(grouped["items"][0]["status"], "unranked");
}

#[test]
fn arguments_and_files_are_checked_before_any_analysis() {
    let f = LevelFixture::new();
    for args in [
        vec!["hint", TEXT, "--level", "0"],
        vec!["hint", TEXT, "--level", "11"],
        vec!["hint", TEXT, "--level", "abc"],
        vec!["hint", "   "],
        vec!["hint", TEXT, "--file", "also-given.txt"],
    ] {
        let (code, _, err) = run(&f, &args);
        assert_eq!(code, Some(2), "{args:?} → {code:?} {err}");
    }
    let (code, _, err) = run(
        &f,
        &["hint", "--file", f.path("absent.txt").to_str().unwrap()],
    );
    assert_eq!(code, Some(1), "{err}");

    let file = f.path("doc.txt");
    std::fs::write(&file, "\u{feff}world ephemeral\r\n\r\nzealous\r\n").unwrap();
    let args = ["hint", "--file", file.to_str().unwrap(), "--level", "2"];
    let (code, out, err) = run(&f, &args);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("超档 1"), "{out}");
    // BOM 和 \r 都不能变成词的一部分
    assert_eq!(json(&f, &args)["tokens"], 3);
}

/// 词级表是可选表：旧词库必须给出能照着做的重建命令，而不是静默把一切当生词。
#[test]
fn a_library_without_word_levels_says_how_to_rebuild_it() {
    let f = Fixture::new();
    let out = f.cli().args(["hint", "world"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        err.contains("tools/word_levels.py") && err.contains("--levels"),
        "{err}"
    );
    // 走到这条分支本身就说明 hint 是子命令，没被当成隐式 query
    assert!(!err.contains("未收录"));
}

// ───────────────────────── 拼写建议：词级加权 + 词形回退 ─────────────────────────

fn suggestions(store: &DictStore, word: &str) -> Vec<(String, usize, Option<String>)> {
    store
        .fuzzy(word, 6)
        .unwrap()
        .into_iter()
        .map(|s| (s.word, s.distance, s.via))
        .collect()
}

fn words(store: &DictStore, word: &str) -> Vec<String> {
    suggestions(store, word)
        .into_iter()
        .map(|(word, _, _)| word)
        .collect()
}

/// 同距离先给常见词。不带词级表的旧词库退回长度差＋字母序，顺序与今天一致。
#[test]
fn suggestions_prefer_the_commoner_of_two_equal_distance_words() {
    let f = LevelFixture::new();
    let store = DictStore::open(&f.db).unwrap();
    // axe 是 9 档、axis 是 1 档，两者都距 axs 一步；带号排在长度差之前
    assert_eq!(words(&store, "axs"), ["axis", "axe"]);
    assert_eq!(words(&store, "wrold")[0], "world");

    let plain = Fixture::new();
    let old = DictStore::open(&plain.db).unwrap();
    assert!(!old.has_levels());
    assert_eq!(words(&old, "axs"), ["axe", "axis"]);
}

/// 词形是二手证据：绕词形表回来的原形不插到同距离的直达词头前面。
#[test]
fn a_lemma_reached_through_a_form_waits_behind_direct_matches() {
    let f = Fixture::new();
    let store = DictStore::open(&f.db).unwrap();
    assert!(!store.has_levels());
    // san 离词头 saw 一步，也离词形 ran／saw 一步；run 和 see 只能经词形端上来
    assert_eq!(
        suggestions(&store, "san"),
        vec![
            ("saw".to_string(), 1, None),
            ("run".to_string(), 1, Some("ran".to_string())),
            ("see".to_string(), 1, Some("saw".to_string())),
        ]
    );
}

/// 拼错的屈折形式回到原形卡，并把命中的那个词形一起说清楚。
#[test]
fn a_misspelled_inflection_falls_back_to_its_lemma_card() {
    let f = LevelFixture::new();
    let store = DictStore::open(&f.db).unwrap();
    assert!(store.lookup("running").unwrap().is_none());
    assert_eq!(
        suggestions(&store, "runinng"),
        vec![("run".to_string(), 1, Some("running".to_string()))]
    );
    // 词头自己就是候选时不绕这条路，距离是到自己的
    assert_eq!(
        suggestions(&store, "wrold")
            .into_iter()
            .find(|(word, _, _)| word == "world")
            .unwrap(),
        ("world".to_string(), 1, None)
    );
    // 原形已经在候选里了，不会因为词形表再补一份带 via 的重复项
    let axe = suggestions(&store, "axxes")
        .into_iter()
        .find(|(word, _, _)| word == "axe")
        .unwrap();
    assert_eq!(axe.1, 2usize);
    assert_eq!(axe.2, None);
}
