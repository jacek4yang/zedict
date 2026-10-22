use std::io::Read;
use std::path::Path;
use zedict::{
    data::ingest,
    data::store::{DictStore, QueryRequest, SearchMode},
};

#[test]
fn full_import_archives_every_record_and_applies_corrections_without_a_gate() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jsonl");
    let db = dir.path().join("dict.sqlite");
    let rows = [
        serde_json::json!({"headword":"world","senses":["错误混入的释义"]}),
        serde_json::json!({"headword":"novel","senses":["小说"],"examples":["A novel@一本小说"]}),
        serde_json::json!({"headword":"worlds","senses":["世界"]}),
        serde_json::json!({"headword":null,"senses":["保留无词头记录"]}),
        serde_json::json!({"headword":"CAFÉ","senses":["咖啡馆"]}),
        serde_json::json!({"headword":"cafe\u{301}","senses":["咖啡店"]}),
    ];
    let lines: Vec<_> = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    std::fs::write(&source, lines.join("\n") + "\n").unwrap();
    let report = ingest::import_structured(&source, None, &db, None).unwrap();
    assert_eq!(
        (
            report.raw_records,
            report.archived_records,
            report.indexed_cards,
            report.indexed_headwords,
            report.missing_headword_records
        ),
        (6, 6, 5, 4, 1)
    );
    assert_eq!(report.verified_cards, 0);
    let manifest = dir.path().join("corrections.json");
    std::fs::write(&manifest, serde_json::to_vec(&serde_json::json!({
        "schema_version":1,"source_sha256":ingest::hash_file(&source).unwrap(),
        "data_version":"test", "reviews":[{
            "line":3,"record_sha256":ingest::hash_bytes(lines[2].as_bytes()),
            "status":"verified","headword":"world","reviewer":"test", "evidence":"fixture correction", "senses":[0]
        }]
    })).unwrap()).unwrap();
    let report = ingest::import_structured(&source, Some(&manifest), &db, None).unwrap();
    assert_eq!(report.indexed_headwords, 3);
    let store = DictStore::open(&db).unwrap();
    store.check_integrity().unwrap();
    let world = store.lookup("world").unwrap().unwrap();
    assert_eq!(world.cards.len(), 1);
    assert_eq!(world.cards[0].senses[0].gloss, "世界");
    assert_eq!(
        store.lookup("novel").unwrap().unwrap().cards[0]
            .examples
            .len(),
        1
    );
    assert_eq!(store.lookup("café").unwrap().unwrap().cards.len(), 2);
    let reverse = store
        .query(&QueryRequest {
            query: "错误混入".into(),
            mode: SearchMode::Reverse,
            limit: 8,
        })
        .unwrap();
    assert!(reverse.hits.is_empty());
    let rendered = zedict::render::card(&world.cards[0], false, 100);
    assert!(!rendered.contains("复核"));
    let conn = rusqlite::Connection::open(&db).unwrap();
    for (index, expected) in lines.iter().enumerate() {
        let bytes: Vec<u8> = conn
            .query_row(
                "SELECT raw FROM source_records WHERE line=?1",
                [index + 1],
                |r| r.get(0),
            )
            .unwrap();
        let mut raw = String::new();
        flate2::read::ZlibDecoder::new(bytes.as_slice())
            .read_to_string(&mut raw)
            .unwrap();
        assert_eq!(&raw, expected);
    }
    drop(conn);
    drop(store);
    let before = std::fs::read(&db).unwrap();
    std::fs::write(&source, b"{invalid json}\n").unwrap();
    assert!(ingest::import_structured(&source, None, &db, None).is_err());
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

fn inflection_fixture(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let source = dir.join("source.jsonl");
    std::fs::write(
        &source,
        [
            serde_json::json!({"headword":"run","senses":["跑"]}),
            serde_json::json!({"headword":"running","senses":["奔跑中"]}),
        ]
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
            + "\n",
    )
    .unwrap();
    (source, dir.join("dict.sqlite"))
}

fn table(dir: &Path, rows: &[serde_json::Value]) -> std::path::PathBuf {
    let path = dir.join("inflections.jsonl");
    std::fs::write(
        &path,
        rows.iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    path
}

#[test]
fn an_inflected_form_reaches_its_lemma_and_an_absent_lemma_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (source, db) = inflection_fixture(dir.path());
    let inflections = table(
        dir.path(),
        &[
            serde_json::json!({"form":"ran","lemma":"run","kind":"past","evidence":"hunspell en_US.dic ran st:run"}),
            serde_json::json!({"form":"running","lemma":"run","kind":"present_participle","evidence":"hunspell en_US.dic running st:run"}),
            serde_json::json!({"form":"aahed","lemma":"aah","kind":"past","evidence":"hunspell en_US.aff SFX D 0->ed"}),
        ],
    );
    let report = ingest::import_structured_with_extras(
        &source,
        None,
        &ingest::Extras {
            inflections: Some(&inflections),
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    assert_eq!((report.inflections, report.inflections_unmatched), (2, 1));
    let store = DictStore::open(&db).unwrap();
    store.check_integrity().unwrap();

    let ran = store
        .query(&QueryRequest {
            query: "ran".into(),
            mode: SearchMode::Auto,
            limit: 8,
        })
        .unwrap();
    assert_eq!(ran.hits.len(), 1);
    assert_eq!(ran.hits[0].key, "run");
    assert_eq!(ran.hits[0].match_type, "lemma");
    assert_eq!(ran.hits[0].relations, vec!["past".to_string()]);

    // A form the library carries its own card for keeps both readings.
    let running = store
        .query(&QueryRequest {
            query: "running".into(),
            mode: SearchMode::Auto,
            limit: 8,
        })
        .unwrap();
    assert_eq!(running.hits[0].match_type, "exact");
    assert_eq!(running.hits[1].key, "run");
    assert_eq!(running.hits[1].relations, vec!["present_participle"]);
}

#[test]
fn a_derivational_relation_refuses_the_build_and_leaves_the_library_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (source, db) = inflection_fixture(dir.path());
    ingest::import_structured(&source, None, &db, None).unwrap();
    let before = std::fs::read(&db).unwrap();
    let inflections = table(
        dir.path(),
        &[
            serde_json::json!({"form":"happiness","lemma":"happy","kind":"noun_form","evidence":"hunspell en_US.aff SFX Y 0->ness"}),
        ],
    );
    assert!(ingest::import_structured_with_extras(
        &source,
        None,
        &ingest::Extras {
            inflections: Some(&inflections),
            ..Default::default()
        },
        &db,
        None
    )
    .is_err());
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

/// 三项离线增益共用一个小夹具：联想词中文释义、词形前缀补全、繁体反查。
#[test]
fn optional_inputs_widen_the_word_card_without_touching_any_key() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jsonl");
    std::fs::write(
        &source,
        [
            serde_json::json!({"headword":"test","senses":["测试"],"synonyms":["probe","EXAM"]}),
            serde_json::json!({"headword":"run","senses":["跑"]}),
            serde_json::json!({"headword":"computer","senses":["计算机"]}),
            serde_json::json!({"headword":"plain","senses":["一目了然"]}),
        ]
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
            + "\n",
    )
    .unwrap();
    // 第三行有词有制表符但没有释义：跳过它，而不是让整个构建失败。
    let related = dir.path().join("related.tsv");
    std::fs::write(&related, "probe\t探针\nEXAM\t考试\ntiny\t\n").unwrap();
    let inflections = table(
        dir.path(),
        &[
            serde_json::json!({"form":"running","lemma":"run","kind":"present_participle","evidence":"hunspell en_US.dic running st:run"}),
        ],
    );
    let db = dir.path().join("dict.sqlite");
    let report = ingest::import_structured_with_extras(
        &source,
        None,
        &ingest::Extras {
            inflections: Some(&inflections),
            related: Some(&related),
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    assert_eq!(report.related_glosses, 2);

    let store = DictStore::open(&db).unwrap();
    store.check_integrity().unwrap();
    let datasets = store.datasets().unwrap();
    assert_eq!(datasets["related"]["rows"], 3);
    assert_eq!(datasets["related"]["keys"], 2);
    assert_eq!(datasets["inflections"]["rows"], 1);

    let hit = store.lookup("test").unwrap().unwrap();
    let card = &hit.cards[0];
    // 释义表里的大写键靠词库自己的 normalize 规则命中，不改任何键。
    assert_eq!(card.synonyms[0].word, "probe");
    assert_eq!(card.synonyms[0].gloss.as_deref(), Some("探针"));
    assert_eq!(card.synonyms[1].word, "EXAM");
    assert_eq!(card.synonyms[1].gloss.as_deref(), Some("考试"));
    assert!(zedict::render::card(card, false, 100).contains("probe（探针）"));
    // 音节切分早就实现了，只是构建时没有接上；单音节词仍然不给假切分。
    assert_eq!(card.phonetic.syllables, None);
    let computer_hit = store.lookup("computer").unwrap().unwrap();
    let computer = &computer_hit.cards[0];
    assert_eq!(
        computer.phonetic.syllables,
        Some(vec!["com".to_string(), "pu".to_string(), "ter".to_string()])
    );
    assert!(zedict::render::card(computer, false, 100).contains("音节: com·pu·ter"));

    // running 没有自己的词卡，只存在于词形表里，前缀补全仍然要能补出来。
    let mut completions: Vec<_> = store
        .prefix("runn", 8)
        .unwrap()
        .into_iter()
        .map(|h| h.key)
        .collect();
    completions.sort();
    assert!(
        completions.contains(&"run".to_string()),
        "prefix(runn) = {completions:?}"
    );

    // 繁体查询要先折成简体，才可能命中按单字建的反向索引。第三行走的是词组表：
    // 「瞭」不在单字表里，逐字折一个字符都动不了。
    for (traditional, simplified, key) in [
        ("測試", "测试", "test"),
        ("計算機", "计算机", "computer"),
        ("一目瞭然", "一目了然", "plain"),
    ] {
        let fold = |text: &str| {
            store
                .query(&QueryRequest {
                    query: text.into(),
                    mode: SearchMode::Auto,
                    limit: 8,
                })
                .unwrap()
                .hits
                .into_iter()
                .map(|h| h.key)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fold(traditional),
            fold(simplified),
            "{traditional} 应当与 {simplified} 反查到同一个词"
        );
        assert_eq!(fold(traditional), vec![key.to_string()]);
    }
}

/// 可选资料表只"补信息"：不给这些标志时，词库必须和现在逐字节一致。
#[test]
fn leaving_the_optional_tables_out_changes_not_a_single_byte() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jsonl");
    std::fs::write(
        &source,
        serde_json::to_string(&serde_json::json!({
            "headword":"test","senses":["测试"],"synonyms":["probe"]
        }))
        .unwrap()
            + "\n",
    )
    .unwrap();
    let plain = dir.path().join("plain.sqlite");
    let default_extras = dir.path().join("default.sqlite");
    ingest::import_structured(&source, None, &plain, None).unwrap();
    ingest::import_structured_with_extras(
        &source,
        None,
        &ingest::Extras::default(),
        &default_extras,
        None,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&plain).unwrap(),
        std::fs::read(&default_extras).unwrap()
    );

    let store = DictStore::open(&plain).unwrap();
    assert!(store.datasets().unwrap().as_object().unwrap().is_empty());
    let card = &store.lookup("test").unwrap().unwrap().cards[0];
    assert_eq!(card.synonyms[0].word, "probe");
    assert_eq!(card.synonyms[0].gloss, None);
}
