mod common;
use common::Fixture;
use zedict::data::store::{DictStore, QueryRequest, SearchMode};

#[test]
fn verified_fields_only_and_ambiguous_forms() {
    let f = Fixture::new();
    let store = DictStore::open(&f.db).unwrap();
    store.check_integrity().unwrap();
    assert!(store.lookup("wrong-guess").unwrap().is_none());
    assert!(store.lookup("unverified").unwrap().is_none());
    assert!(store.lookup("rejected").unwrap().is_none());
    let world = store.lookup("world").unwrap().unwrap();
    assert_eq!(world.cards.len(), 2);
    assert!(world
        .cards
        .iter()
        .all(|c| c.senses.iter().all(|s| !s.gloss.contains("错误"))));
    let request = |query: &str| QueryRequest {
        query: query.into(),
        mode: SearchMode::Auto,
        limit: 8,
    };
    let saw = store.query(&request("saw")).unwrap();
    assert_eq!(
        saw.hits.iter().map(|h| h.key.as_str()).collect::<Vec<_>>(),
        ["saw", "see"]
    );
    let axes = store.query(&request("axes")).unwrap();
    assert_eq!(axes.hits.len(), 2);
    assert_eq!(store.query(&request("running")).unwrap().hits[0].key, "run");
    assert!(store
        .fuzzy("wrold", 5)
        .unwrap()
        .iter()
        .any(|s| s.word == "world"));
    assert!(store
        .fuzzy("xorld", 5)
        .unwrap()
        .iter()
        .any(|s| s.word == "world"));
    assert_eq!(
        store.lookup("  CAFE\u{301}  ").unwrap().unwrap().key,
        "café"
    );
}

#[test]
fn reverse_one_two_many_characters_and_punctuation() {
    let f = Fixture::new();
    let store = DictStore::open(&f.db).unwrap();
    for (q, expected) in [
        ("书", "book"),
        ("学习", "learn"),
        ("计算机", "computer"),
        ("苹果", "apple"),
    ] {
        let result = store
            .query(&QueryRequest {
                query: q.into(),
                mode: SearchMode::Reverse,
                limit: 8,
            })
            .unwrap();
        assert_eq!(result.hits.len(), 1, "{q}");
        assert_eq!(result.hits[0].key, expected);
        assert!(!result.hits[0].matched_senses.is_empty());
    }
}

#[test]
fn auto_chinese_checks_exact_headword_before_reverse() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("cn.jsonl");
    std::fs::write(
        &source,
        concat!(
            "{\"headword\":\"呕\",\"pinyin\":\"ǒu\",\"senses\":[\"vomit\"]}\n",
            "{\"headword\":\"呕吐\",\"senses\":[\"vomit\"]}\n",
            "{\"headword\":\"vomit\",\"senses\":[\"<i>v.</i> 呕吐\"]}\n",
        ),
    )
    .unwrap();
    let db = dir.path().join("cn.sqlite");
    zedict::data::ingest::import_structured(&source, None, &db, None).unwrap();
    let store = DictStore::open(&db).unwrap();
    let request = |query: &str| QueryRequest {
        query: query.into(),
        mode: SearchMode::Auto,
        limit: 8,
    };
    for (query, exact) in [("呕", "呕"), ("呕吐", "呕吐")] {
        let result = store.query(&request(query)).unwrap();
        assert_eq!(result.hits[0].key, exact);
        assert!(
            result.hits.iter().any(|hit| hit.key == "vomit"),
            "{query}: 反查结果丢失"
        );
    }
}

#[test]
fn changed_source_or_bad_reviews_never_replace_working_db() {
    let f = Fixture::new();
    let old = std::fs::read(&f.db).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&f.manifest).unwrap()).unwrap();
    manifest["reviews"][0]["record_sha256"] = "wrong".into();
    std::fs::write(&f.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(zedict::data::ingest::build(&f.source, &f.manifest, &f.db).is_err());
    assert_eq!(old, std::fs::read(&f.db).unwrap());
    std::fs::write(&f.source, "{}\n").unwrap();
    assert!(zedict::data::ingest::build(&f.source, &f.manifest, &f.db).is_err());
    assert_eq!(old, std::fs::read(&f.db).unwrap());
}

#[test]
fn corrupt_old_and_wrong_version_databases_fail_cleanly() {
    let f = Fixture::new();
    let old = f.dir.path().join("old.db");
    std::fs::write(&old, b"ZEDICT01xxxxxxxxxxxx").unwrap();
    assert!(DictStore::open(&old)
        .err()
        .unwrap()
        .to_string()
        .contains("ZEDICT01"));
    std::fs::write(&old, b"SQLite format 3\0broken").unwrap();
    assert!(DictStore::open(&old).is_err());
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.execute_batch("PRAGMA user_version=999").unwrap();
    drop(conn);
    assert!(DictStore::open(&f.db).is_err());
}

#[test]
fn html_entities_inline_and_block_boundaries() {
    assert_eq!(
        zedict::core::model::strip_html("<p>A &amp; B</p><p>&#20013;&#x6587;</p>"),
        "A & B 中文"
    );
    assert_eq!(
        zedict::core::model::strip_html("<ol><li>greeting</li><li>every morning</li></ol>"),
        "greeting every morning"
    );
    assert_eq!(
        zedict::core::model::PartOfSpeech::parse("vt."),
        Some(zedict::core::model::PartOfSpeech::TransitiveVerb)
    );
    assert_eq!(
        zedict::core::model::FormKind::parse("时态"),
        zedict::core::model::FormKind::Other
    );
}

#[test]
fn inline_highlight_does_not_introduce_spaces_and_cancellation_preserves_db() {
    assert_eq!(
        zedict::core::model::strip_html("The <span class=\"key\">world</span>."),
        "The world."
    );
    assert_eq!(
        zedict::core::model::strip_html("我们的<span class=\"key\">世界</span>。"),
        "我们的世界。"
    );
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.db).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    assert!(zedict::data::ingest::build_cancellable(
        &fixture.source,
        &fixture.manifest,
        &fixture.db,
        Some(&cancel)
    )
    .is_err());
    assert_eq!(std::fs::read(&fixture.db).unwrap(), before);
}
