//! 生词本与 Anki 导出的端到端测试：学习数据必须落在独立文件里，词典库一个字节都不动。

mod common;

use common::Fixture;
use serde_json::Value;
use std::path::Path;

fn study_cli(f: &Fixture, db: &Path) -> std::process::Command {
    let mut cmd = f.cli();
    cmd.env("ZEDICT_STUDY_DB", db);
    cmd
}

fn run(cmd: &mut std::process::Command) -> (Option<i32>, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn add_review_stats_and_remove_keep_the_dictionary_untouched() {
    let f = Fixture::new();
    let study_db = f.dir.path().join("study.sqlite");
    let before = zedict::data::ingest::hash_file(&f.db).unwrap();

    let (code, stdout, stderr) = run(study_cli(&f, &study_db)
        .args(["study", "add", "running"])
        .arg("--json"));
    assert_eq!(code, Some(0), "{stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["added"], true);
    assert_eq!(value["word"]["key"], "run", "running 应该归并到原形");
    assert_eq!(value["word"]["headword"], "run");
    assert_eq!(value["word"]["state"], "new");

    let (code, stdout, stderr) = run(study_cli(&f, &study_db)
        .args(["study", "add", "run"])
        .arg("--json"));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(stdout.trim()).unwrap()["added"],
        false
    );

    let (code, _stdout, stderr) =
        run(study_cli(&f, &study_db).args(["study", "add", "no-such-word"]));
    assert_eq!(code, Some(0), "{stderr}");

    let (code, stdout, stderr) = run(study_cli(&f, &study_db).args(["study", "list", "--json"]));
    assert_eq!(code, Some(0), "{stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["total"], 2);
    assert_eq!(value["words"].as_array().unwrap().len(), 2);

    let (code, stdout, stderr) = run(study_cli(&f, &study_db)
        .args(["study", "review", "run", "--grade", "good"])
        .arg("--json"));
    assert_eq!(code, Some(0), "{stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["word"]["reps"], 1);
    assert_eq!(value["word"]["state"], "review");
    assert!((value["word"]["interval_days"].as_f64().unwrap() - 1.0).abs() < 1e-9);

    let (code, stdout, stderr) = run(study_cli(&f, &study_db).args(["study", "stats", "--json"]));
    assert_eq!(code, Some(0), "{stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["total"], 2);
    assert_eq!(value["new"], 1);
    assert_eq!(value["review"], 1);
    assert_eq!(value["reviews"], 1);
    assert!(value["due"].as_i64().unwrap() >= 1);

    let (code, _stdout, stderr) =
        run(study_cli(&f, &study_db).args(["study", "review", "run", "--grade", "excellent"]));
    assert_eq!(code, Some(2), "非法评分是参数错误: {stderr}");

    let (code, _stdout, stderr) = run(study_cli(&f, &study_db).args(["study", "remove", "absent"]));
    assert_eq!(code, Some(3), "{stderr}");

    let (code, _stdout, stderr) = run(study_cli(&f, &study_db).args(["study", "remove", "run"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        zedict::data::ingest::hash_file(&f.db).unwrap(),
        before,
        "学习状态绝不能写词典库"
    );
    assert!(study_db.is_file(), "独立的学习库应当存在");
}

#[test]
fn apkg_export_contains_the_words_and_dictionary_backs() {
    let f = Fixture::new();
    let study_db = f.dir.path().join("study.sqlite");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("study.apkg");
    for word in ["apple", "book", "qwerty"] {
        let (code, _stdout, stderr) = run(study_cli(&f, &study_db).args(["study", "add", word]));
        assert_eq!(code, Some(0), "{stderr}");
    }

    let (code, stdout, stderr) = run(study_cli(&f, &study_db)
        .args(["study", "export", "--deck", "测试牌组"])
        .arg("--out")
        .arg(&out)
        .arg("--json"));
    assert_eq!(code, Some(0), "{stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["notes"], 3);
    assert_eq!(value["deck"], "测试牌组");

    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    let collection = extract(&bytes, "collection.anki2").expect("缺少 collection.anki2");
    assert_eq!(extract(&bytes, "media").as_deref(), Some(b"{}".as_slice()));

    let check = dir.path().join("check.sqlite");
    std::fs::write(&check, collection).unwrap();
    let conn = rusqlite::Connection::open(&check).unwrap();
    let notes: i64 = conn
        .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(notes, 3);
    let (flds, front): (String, String) = conn
        .query_row("SELECT flds, sfld FROM notes WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(front, "apple");
    assert!(flds.contains("苹果"), "{flds}");
    let back: String = conn
        .query_row("SELECT flds FROM notes WHERE id=3", [], |r| r.get(0))
        .unwrap();
    assert!(back.contains("词库未收录"), "{back}");
    let deck: String = conn
        .query_row("SELECT decks FROM col", [], |r| r.get(0))
        .unwrap();
    assert!(deck.contains("测试牌组"), "{deck}");
}

#[test]
fn exporting_an_empty_wordbook_is_an_actionable_error() {
    let f = Fixture::new();
    let study_db = f.dir.path().join("study.sqlite");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("empty.apkg");
    let (code, _stdout, stderr) = run(study_cli(&f, &study_db)
        .args(["study", "export"])
        .arg("--out")
        .arg(&out));
    assert_eq!(code, Some(1));
    assert!(stderr.contains("study add"), "{stderr}");
    assert!(!out.exists());
}

/// 只解 stored 条目，够验证本项目的写出器；不是通用 ZIP 解析器。
fn extract(bytes: &[u8], want: &str) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    while bytes.get(pos..pos + 4)? == 0x0403_4b50u32.to_le_bytes() {
        let name_len = u16::from_le_bytes(bytes.get(pos + 26..pos + 28)?.try_into().ok()?) as usize;
        let extra_len =
            u16::from_le_bytes(bytes.get(pos + 28..pos + 30)?.try_into().ok()?) as usize;
        let size = u32::from_le_bytes(bytes.get(pos + 18..pos + 22)?.try_into().ok()?) as usize;
        let name = std::str::from_utf8(bytes.get(pos + 30..pos + 30 + name_len)?).ok()?;
        let start = pos + 30 + name_len + extra_len;
        let data = bytes.get(start..start + size)?;
        if name == want {
            return Some(data.to_vec());
        }
        pos = start + size;
    }
    None
}
