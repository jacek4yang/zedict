//! `zedict export`：Anki 可直接导入的 CSV / TSV 文本表。
//!
//! 导出的每一行都必须是一张能翻面的卡：词级表缺失时少一个标签，而不是整批失败。
//! CSV 的义项换行留在引号字段里，所以断言按"记录子串"而不是物理行来比。

mod common;
use common::{Fixture, LevelFixture};
use serde_json::Value;

fn run(f: &LevelFixture, args: &[&str]) -> (Option<i32>, String, String) {
    let out = f.cli().args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("stdout 必须是 UTF-8"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const HEADER: &str = "#separator:comma\n#html:false\n#columns:Front,Back,Tags\n";
const KEYS: [&str; 7] = [
    "axe",
    "axis",
    "ephemeral",
    "run",
    "take into account",
    "world",
    "zealous",
];

/// 七张卡：axe, axis, ephemeral, run, take into account, world, zealous。
#[test]
fn csv_starts_with_anki_directives_and_tags_the_word_level() {
    let f = LevelFixture::new();
    let (code, out, err) = run(&f, &["export"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.starts_with(HEADER), "{out}");
    // 指令头 3 行 + 7 张卡；run 的第二条义项在引号字段里，多占一个物理行。
    assert_eq!(out.lines().count(), 3 + 7 + 1, "{out}");
    for record in [
        "axe,1. n. 斧头,zedict band-9 list-gre pos-n\n",
        "axis,1. n. 轴,zedict band-1 pos-n\n",
        "ephemeral,1. adj. 短暂的,zedict band-9 list-gre pos-adj\n",
        // 一条义项一行：CSV 能表示字段内换行，所以整个字段用引号包起来
        "run,\"1. v. 跑\n2. n. 奔跑\",zedict band-2 list-junior pos-v pos-n\n",
        // 词级表没有这个词头，只剩来源和词性；没有逗号的字段不加引号
        "take into account,1. v. 考虑,zedict pos-v\n",
        "world,1. n. 世界,zedict band-1 list-junior pos-n\n",
        "zealous,1. adj. 热心的,zedict pos-adj\n",
    ] {
        assert!(out.contains(record), "缺 {record:?}\n{out}");
    }
    // 只取两行时按词头顺序，从 --offset 起
    let (_, page, _) = run(&f, &["export", "--rows", "2", "--offset", "1"]);
    let fronts: Vec<&str> = page
        .lines()
        .skip(3)
        .filter(|l| KEYS.iter().any(|k| l.starts_with(&format!("{k},"))))
        .map(|l| l.split(',').next().unwrap())
        .collect();
    assert_eq!(fronts, ["axis", "ephemeral"]);
}

#[test]
fn tsv_flattens_every_field_to_one_line_per_card() {
    let f = LevelFixture::new();
    let (code, out, err) = run(&f, &["export", "--format", "tsv"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        out.starts_with("#separator:tab\n#html:false\n#columns:Front\tBack\tTags\n"),
        "{out}"
    );
    // TSV 没有引号机制，所以每条卡必须占满一行：行数正好等于卡数
    assert_eq!(out.lines().count(), 3 + 7, "{out}");
    assert!(out.contains("1. v. 跑 2. n. 奔跑"), "{out}");
    assert!(!out.contains('"'), "TSV 不该出现 CSV 的引号: {out}");
    assert_eq!(
        out.lines()
            .skip(3)
            .filter(|l| KEYS.iter().any(|k| l.starts_with(&format!("{k}\t"))))
            .count(),
        7
    );
}

#[test]
fn writing_a_file_reports_what_it_contains() {
    let f = LevelFixture::new();
    let path = f.path("deck.csv");
    let mut args = vec!["export", "--all", "--out", path.to_str().unwrap()];
    let (code, out, err) = run(&f, &args);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("已导出 7 行 csv"), "{out}");
    assert!(out.contains("标签含词级: 是"), "{out}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(HEADER));
    assert_eq!(text.lines().count(), 3 + 7 + 1);

    args.push("--json");
    let (code, stdout, err) = run(&f, &args);
    assert_eq!(code, Some(0), "{err}");
    let meta: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(meta["format"], "csv");
    assert_eq!(meta["keys"], 7);
    assert_eq!(meta["rows"], 7);
    assert_eq!(meta["levels"], true);
    assert_eq!(meta["out"], path.display().to_string());
    assert_eq!(meta["bytes"].as_u64().unwrap(), text.len() as u64);
}

/// 随机抽样走的是词库的 random()，同样只产出完整的卡。
#[test]
fn a_sample_is_bounded_and_still_valid_csv() {
    let f = LevelFixture::new();
    let path = f.path("sample.csv");
    let (code, out, err) = run(
        &f,
        &[
            "export",
            "--random",
            "--rows",
            "3",
            "--out",
            path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, Some(0), "{err}");
    let meta: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(meta["keys"], 3);
    assert_eq!(meta["rows"], 3);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(HEADER));
    // 每条记录都以一个已知词头开头：抽样不能混进半张卡
    let records = text
        .lines()
        .skip(3)
        .filter(|l| KEYS.iter().any(|k| l.starts_with(&format!("{k},"))))
        .count();
    assert_eq!(records, 3, "{text}");
}

#[test]
fn impossible_or_empty_requests_are_refused_early() {
    let f = LevelFixture::new();
    for args in [
        // --json 只报元数据，正文得落到文件里
        vec!["export", "--json"],
        vec!["export", "--all", "--random"],
        vec!["export", "--rows", "0"],
        vec!["export", "--rows", "100001"],
        vec!["export", "--format", "apkg"],
    ] {
        let (code, _, err) = run(&f, &args);
        assert_eq!(code, Some(2), "{args:?} → {code:?} {err}");
    }
    // 词头取完了就是没有：不是参数错，是"没找到"
    let (code, out, _) = run(&f, &["export", "--offset", "999"]);
    assert_eq!(code, Some(3));
    assert_eq!(out, HEADER);
}

/// 旧词库没有 word_level 表：导出照旧，只是少 band-/list- 标签。
#[test]
fn an_older_library_exports_without_level_tags() {
    let f = Fixture::new();
    let out = f.cli().args(["export", "--rows", "40"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with(HEADER));
    assert!(!text.contains("band-") && !text.contains("list-"), "{text}");
    assert!(text.contains("zedict"), "{text}");

    let path = f.dir.path().join("old.csv");
    let line = f
        .cli()
        .args([
            "export",
            "--json",
            "--out",
            path.to_str().unwrap(),
            "--format",
            "tsv",
        ])
        .output()
        .unwrap();
    assert_eq!(line.status.code(), Some(0));
    let meta: Value = serde_json::from_slice(&line.stdout).unwrap();
    assert_eq!(meta["levels"], false);
    assert_eq!(meta["format"], "tsv");
    assert!(meta["rows"].as_u64().unwrap() > 0);
    // 没有词级表时，行数就是卡数：TSV 的字段不会自带换行
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        3 + meta["rows"].as_u64().unwrap() as usize
    );
}
