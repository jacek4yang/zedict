mod common;
use common::Fixture;
use std::{io::Write, process::Stdio};

#[test]
fn modes_always_emit_json_and_validate_arguments() {
    let f = Fixture::new();
    for (q, mode) in [
        ("world", "auto"),
        ("world", "exact"),
        ("wor", "prefix"),
        ("orl", "contains"),
        ("wrold", "fuzzy"),
        ("世界", "reverse"),
    ] {
        let out = f
            .cli()
            .args(["query", q, "--mode", mode, "--json"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert!(out.stderr.is_empty());
    }
    for args in [
        vec!["query", "world", "--mode", "typo"],
        vec!["query", "world", "--limit", "0"],
        vec!["query", "   "],
    ] {
        assert_eq!(f.cli().args(args).output().unwrap().status.code(), Some(2));
    }
    let miss = f.cli().args(["unknownzzz", "--json"]).output().unwrap();
    assert_eq!(miss.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&miss.stdout).unwrap()["status"],
        "not_found"
    );
}

#[test]
fn arbitrary_cwd_complete_cards_config_and_batch() {
    let f = Fixture::new();
    let full = f.cli().arg("apple").output().unwrap();
    assert!(full.status.success());
    let text = String::from_utf8(full.stdout).unwrap();
    assert!(text.contains("Fourth"));
    assert!(!text.contains('\u{1b}'));
    let brief = f.cli().args(["apple", "--brief"]).output().unwrap();
    assert!(!String::from_utf8(brief.stdout).unwrap().contains("Fourth"));
    let mut child = f
        .cli()
        .args(["query", "--stdin", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("world\n\n苹果\nunknownzzz\n".as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let values: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(values.len(), 3);
    assert_eq!(values[1]["query"], "苹果");
    assert!(f.cli().arg("doctor").output().unwrap().status.success());
    assert!(f.cli().arg("--version").output().unwrap().status.success());
}

#[test]
fn themes_are_explicit_and_json_stays_plain() {
    let f = Fixture::new();
    let ascii = f
        .cli()
        .args(["apple", "--theme", "ascii"])
        .output()
        .unwrap();
    let ascii = String::from_utf8(ascii.stdout).unwrap();
    assert!(ascii.contains("[释义]"), "{ascii}");
    assert!(!ascii.contains('\u{1b}'), "{ascii}");

    let rich = f.cli().args(["apple", "--theme", "rich"]).output().unwrap();
    let rich = String::from_utf8(rich.stdout).unwrap();
    assert!(rich.contains("[释义]"), "{rich}");
    assert!(rich.contains('\u{1b}'), "rich 主题应带 ANSI 色彩: {rich}");

    let json = f
        .cli()
        .args(["apple", "--theme", "rich", "--json"])
        .output()
        .unwrap();
    assert!(!String::from_utf8(json.stdout).unwrap().contains('\u{1b}'));
}

#[test]
fn aliases_and_help_do_not_need_dictionary() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_dict-english"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"))
        .args(["completions", "powershell"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!out.stdout.is_empty());
}
