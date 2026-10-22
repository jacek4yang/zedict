use serde_json::json;
use std::path::PathBuf;
use zedict::data::ingest;
use zedict::data::store::{DictStore, QueryRequest, SearchMode};

fn build(pinyin: bool) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jsonl");
    let lines = [
        json!({"headword":"先","pinyin":"xiān","senses":["<i>adv.</i> 先"]}),
        json!({"headword":"西安","pinyin":"xī ān","senses":["西安"]}),
        json!({"headword":"先后","pinyin":"xiān hòu","senses":["先后"]}),
        json!({"headword":"绿","pinyin":"lǜ","senses":["绿"]}),
        json!({"headword":"女","pinyin":"nǚ","senses":["女"]}),
        json!({"headword":"炅","pinyin":"jiǒng；guì","senses":["炅"]}),
        // 下面四条用来钉住排序：整读音、整音节边界都要排在部分前缀之前。
        json!({"headword":"安","pinyin":"ān","senses":["安"]}),
        json!({"headword":"公安","pinyin":"gōng ān","senses":["公安"]}),
        json!({"headword":"想","pinyin":"xiǎng","senses":["想"]}),
        json!({"headword":"hello","senses":["你好"]}),
    ]
    .iter()
    .map(|row| row.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    std::fs::write(&source, lines).unwrap();
    let db = dir.path().join("dict.sqlite");
    ingest::import_structured_with_extras(
        &source,
        None,
        &ingest::Extras {
            pinyin,
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    (dir, db)
}

fn keys(store: &DictStore, query: &str, mode: SearchMode) -> Vec<String> {
    store
        .query(&QueryRequest {
            query: query.into(),
            mode,
            limit: 10,
        })
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| hit.key)
        .collect()
}

#[test]
fn pinyin_mode_ignores_tones_umlaut_and_syllable_boundaries() {
    let (_dir, db) = build(true);
    let store = DictStore::open(&db).unwrap();
    assert!(store.has_pinyin());

    assert_eq!(
        keys(&store, "xian", SearchMode::Pinyin),
        ["先", "西安", "先后", "想"]
    );
    assert_eq!(
        keys(&store, "xiàn", SearchMode::Pinyin),
        ["先", "西安", "先后", "想"]
    );
    // 安 是整读音；公安/西安 只靠整音节边界；不能因为 an… 前缀词把它们挤掉。
    assert_eq!(
        keys(&store, "an", SearchMode::Pinyin),
        ["安", "公安", "西安"]
    );
    // 查询自带音节边界时，整串音节相等的词排最前：xi'an 先给西安，再给同音单字。
    assert_eq!(
        keys(&store, "xi'an", SearchMode::Pinyin),
        ["西安", "先", "想", "先后"]
    );
    assert_eq!(keys(&store, "lü", SearchMode::Pinyin), ["绿"]);
    assert_eq!(keys(&store, "nv", SearchMode::Pinyin), ["女"]);
    assert_eq!(keys(&store, "jiong", SearchMode::Pinyin), ["炅"]);
    assert_eq!(keys(&store, "gui", SearchMode::Pinyin), ["炅"]);
}

#[test]
fn auto_falls_back_to_pinyin_only_without_an_english_prefix() {
    let (_dir, db) = build(true);
    let store = DictStore::open(&db).unwrap();
    assert_eq!(
        keys(&store, "xiàn", SearchMode::Auto),
        ["先", "西安", "先后", "想"]
    );
    assert_eq!(keys(&store, "hello", SearchMode::Auto), ["hello"]);
    // `he` 是英文词首（hello），不该被拼音抢走；没有候选时只给拼写建议。
    let response = store
        .query(&QueryRequest {
            query: "he".into(),
            mode: SearchMode::Auto,
            limit: 10,
        })
        .unwrap();
    assert!(response.hits.is_empty());
}

#[test]
fn a_library_without_the_index_says_how_to_rebuild_it() {
    let (_dir, db) = build(false);
    let store = DictStore::open(&db).unwrap();
    assert!(!store.has_pinyin());
    let err = store
        .query(&QueryRequest {
            query: "xian".into(),
            mode: SearchMode::Pinyin,
            limit: 10,
        })
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("--pinyin") && err.contains("compile"), "{err}");
}
