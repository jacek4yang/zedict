//! Private source data is deliberately not required on CI.
use std::io::BufRead;
use std::path::Path;
use zedict::data::store::{DictStore, QueryRequest, SearchMode};

#[test]
#[ignore = "requires the existing local data/dict_full.jsonl; run explicitly after data reviews"]
fn source_review_cases_and_known_misbindings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = tempfile::tempdir().unwrap();
    let db = out.path().join("dict.sqlite");
    let report = zedict::data::ingest::build(
        &root.join("data/dict_full.jsonl"),
        &root.join("reviews/local.manifest.json"),
        &db,
    )
    .unwrap();
    assert_eq!(report.verified_headwords, 375);
    let store = DictStore::open(&db).unwrap();
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../reviews/cases.json")).unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let hit = store
            .lookup(query)
            .unwrap()
            .unwrap_or_else(|| panic!("Missing reviewed headword: {query}"));
        let source = hit.cards[0].source.as_ref().unwrap();
        assert_eq!(source.line as u64, case["source_line"].as_u64().unwrap());
        assert_eq!(
            source.record_sha256,
            case["record_sha256"].as_str().unwrap()
        );
        assert!(!hit.cards[0].senses.is_empty());
    }
    // The manifest hash, not copied gloss text, proves which source record each
    // reviewed card came from; this public test therefore carries no dictionary
    // content beyond the headwords themselves.
    for word in ["world", "run", "angle", "city", "apple"] {
        let hit = store.lookup(word).unwrap().unwrap();
        let source = hit.cards[0].source.as_ref().unwrap();
        assert_eq!(source.record_sha256.len(), 64);
        assert!(!hit.cards[0].senses.is_empty());
    }
    for (query, lemma) in [
        ("running", "run"),
        ("went", "go"),
        ("mice", "mouse"),
        ("better", "good"),
    ] {
        let result = store
            .query(&QueryRequest {
                query: query.into(),
                mode: SearchMode::Auto,
                limit: 8,
            })
            .unwrap();
        assert_eq!(result.hits[0].key, lemma);
    }
    let leaves = store
        .query(&QueryRequest {
            query: "leaves".into(),
            mode: SearchMode::Auto,
            limit: 8,
        })
        .unwrap();
    assert_eq!(
        leaves
            .hits
            .iter()
            .map(|h| h.key.as_str())
            .collect::<Vec<_>>(),
        ["leaves", "leaf", "leave"]
    );
    assert_eq!(store.fuzzy("wrold", 5).unwrap()[0].word, "world");
    assert!(store.lookup("robustly").unwrap().is_none()); // derivative was not silently made the primary entry
}

fn keys(store: &DictStore, query: &str) -> Vec<(String, String)> {
    store
        .query(&QueryRequest {
            query: query.into(),
            mode: SearchMode::Auto,
            limit: 8,
        })
        .unwrap()
        .hits
        .into_iter()
        .map(|h| (h.key, h.match_type))
        .collect()
}

#[test]
#[ignore = "requires data/, reviews/, data/inflect_aliases.jsonl, data/semantic_related.tsv and data/word_levels.jsonl"]
fn generated_inflections_reach_their_lemmas_and_derivations_never_do() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = tempfile::tempdir().unwrap();
    let db = out.path().join("dict.sqlite");
    let report = zedict::data::ingest::import_structured_with_extras(
        &root.join("data/dict_full.jsonl"),
        Some(&root.join("reviews/local.manifest.json")),
        &zedict::data::ingest::Extras {
            inflections: Some(&root.join("data/inflect_aliases.jsonl")),
            related: Some(&root.join("data/semantic_related.tsv")),
            levels: Some(&root.join("data/word_levels.jsonl")),
            examples: true,
            spell: true,
            pinyin: true,
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    assert!(
        report.inflections > 15_000,
        "inflections: {}",
        report.inflections
    );
    assert_eq!(report.inflections_unmatched, 0);
    assert!(
        report.related_glosses > 100,
        "related glosses: {}",
        report.related_glosses
    );
    let store = DictStore::open(&db).unwrap();
    store.check_integrity().unwrap();

    for (query, lemma, kind) in [
        ("ran", "run", "past"),
        ("running", "run", "present_participle"),
        ("children", "child", "plural"),
        ("boxes", "box", "plural"),
        ("studied", "study", "past"),
        ("teeth", "tooth", "plural"),
        ("bought", "buy", "past"),
    ] {
        let hit = store
            .query(&QueryRequest {
                query: query.into(),
                mode: SearchMode::Auto,
                limit: 8,
            })
            .unwrap()
            .hits
            .into_iter()
            .find(|h| h.key == lemma)
            .unwrap_or_else(|| panic!("{query} did not reach {lemma}: {:?}", keys(&store, query)));
        assert_eq!(hit.match_type, "lemma");
        assert!(
            hit.relations.iter().any(|r| r == kind),
            "{query} -> {lemma}: {:?}",
            hit.relations
        );
    }
    // However obvious the relationship looks to a reader, derivation is not a form.
    for (query, word) in [
        ("happiness", "happy"),
        ("worker", "work"),
        ("removal", "remove"),
    ] {
        assert!(
            keys(&store, query).into_iter().all(|(key, _)| key != word),
            "{query} was admitted as a form of {word}"
        );
    }
    // The affix rules also invent shapes, which corpus attestation has to remove.
    for query in ["stoped", "runing", "childs"] {
        assert!(
            keys(&store, query)
                .into_iter()
                .all(|(_, match_type)| match_type != "lemma"),
            "{query} resolved to a lemma"
        );
    }

    // 前缀补全要能穿过词形表：runn 也得列出 running。
    let runn = store
        .prefix("runn", 20)
        .unwrap()
        .into_iter()
        .map(|hit| hit.key)
        .collect::<Vec<_>>();
    assert!(runn.contains(&"running".to_string()), "{runn:?}");

    // 近义词的中文只在给了 --related 时出现；这几个词的 synonym 列表在源数据里就有条目。
    for word in ["best", "colour", "motley"] {
        let hit = store
            .lookup(word)
            .unwrap()
            .unwrap_or_else(|| panic!("{word} 没有词卡"));
        let glossed: usize = hit
            .cards
            .iter()
            .map(|card| {
                card.synonyms
                    .iter()
                    .filter(|relation| relation.gloss.is_some())
                    .count()
            })
            .sum();
        assert!(glossed > 5, "{word} 只配上 {} 条近义词中文", glossed);
    }

    // 释义是简体存的，繁体查询要能折回来。
    for (traditional, expected) in [("測試", "test"), ("計算機", "computer")] {
        let found = store
            .query(&QueryRequest {
                query: traditional.into(),
                mode: SearchMode::Auto,
                limit: 8,
            })
            .unwrap();
        assert!(
            found.found(),
            "{traditional} 没有命中任何释义（期望与 {expected} 同义）"
        );
    }

    // 词级表是可选表，带进来以后 `hint` 才有依据；个数以写入库里的词形为准。
    assert!(store.has_levels());
    assert_eq!(report.word_levels, 72_970);
    let levels = store.levels(&["the".into(), "an".into()]).unwrap();
    assert_eq!(levels["the"].level, Some(1));
    assert_eq!(levels["an"].level, None, "an 在源表里只挂大纲标签");
    assert!(levels["an"].tags.iter().any(|tag| tag == "cet4"));

    // 拼写建议：绕词形表回来的原形排在同距离的直达词头之后。
    let worl = store.fuzzy("worl", 8).unwrap();
    assert!(
        worl.iter().all(|s| s.via.is_none()),
        "wear（词形 wore）插进了直达词头前面: {worl:?}"
    );
    assert!(worl.iter().any(|s| s.word == "world"));
    let runing = store.fuzzy("runing", 8).unwrap();
    let first_via = runing
        .iter()
        .position(|s| s.via.is_some())
        .expect("runing 没有走词形通路");
    assert!(runing[..first_via].iter().all(|s| s.via.is_none()));
    assert_eq!(runing[first_via + 1].word, "run");
    assert_eq!(runing[first_via + 1].via.as_deref(), Some("running"));

    // 例句索引是从主源那一趟派生的，所以行数、链数和去噪结果必须一起钉死。
    // 这些数是实测：比"跳过没有词头的记录"之前的全量估算少 47 句（那 103 对挂在
    // 无词头记录上，本来就不入库）。
    assert!(store.has_examples());
    assert_eq!(report.examples, 64_253, "例句行数");
    assert_eq!(report.example_links, 511_574, "去噪后的关键词链数");
    assert_eq!(report.dropped_noisy_keys, 4, "被剔除的中文关键词数");

    // 中文关键词反查：病假条既只作过一个标记，正文里也只有那一句带它。
    let sick_note = store.examples_for(&["病假条".into()], 50).unwrap();
    assert_eq!(sick_note.len(), 1, "病假条: {sick_note:?}");
    assert!(!sick_note[0].en.is_empty());
    assert_eq!(store.example_scan_matches("病假条").unwrap(), 1);
    assert_eq!(store.example_key_matches("病假").unwrap(), 1);
    assert_eq!(store.example_scan_matches("病假").unwrap(), 2);

    // 非词头的英文短语键：库里确实有 "in spite of" 这个短语词头，所以它的链
    // 既有英文关键词侧也有词头侧；文本里含它的句子更多。
    assert_eq!(store.example_key_matches("in spite of").unwrap(), 10);
    assert_eq!(store.example_scan_matches("in spite of").unwrap(), 19);
    assert_eq!(
        store
            .examples_for(&["not-a-real-key-zzz".into()], 5)
            .unwrap()
            .len(),
        0
    );

    // 去噪是静默清洗，所以被剔掉的键必须留在库里能查名字。
    let datasets = store.datasets().unwrap();
    let dropped = datasets["examples"]["dropped_keys"]
        .as_array()
        .expect("datasets 里应有被剔除的中文键")
        .clone();
    assert_eq!(dropped.len(), 4, "{dropped:?}");
    assert_eq!(dropped[0]["examples"], 816, "{dropped:?}");
    assert_eq!(datasets["examples"]["noisy_key_threshold"], 300);

    // 删字符索引的规模是实测出来的：69,608 个词头里 25 个只有一个字母，删完就是
    // 空串（空串对任何查询都成立，等于把整张表倒出来），所以它们不进表。
    assert!(store.has_spell());
    assert_eq!(report.spell_rows, 861_626, "spell_key 行数");
    assert_eq!(datasets["spell"]["words"], 69_583, "进表的词头数");
    assert_eq!(datasets["spell"]["derived_from"], "entries");

    // 补位的可达范围只有这么宽：`thsi` 四个字母，主扫描 `max=1`，所以任何距离 2
    // 的候选都只可能来自这张表。而它绝不改动主扫描已经给出的东西——距离 ≤1 的
    // 那几条必须原样排在前面。
    let fill = store.fuzzy("thsi", 10).unwrap();
    assert!(
        fill.iter().any(|hit| hit.distance == 2),
        "应有 distance 2 的补位候选: {fill:?}"
    );
    let primary_end = fill.iter().position(|hit| hit.distance == 2).unwrap();
    assert!(
        fill[..primary_end]
            .iter()
            .all(|hit| hit.distance <= 1 && hit.via.is_none()),
        "补位之前的候选不该被动过: {fill:?}"
    );
    // 长查询就完全是另一回事了：`recieve` 七个字母允许两步，穷举扫描本来就扫全了，
    // 而它真正的问题是 `receive` 自己没有词卡（README 的头号缺陷那一节）。
    let recieve = store.fuzzy("recieve", 10).unwrap();
    assert!(
        !recieve.iter().any(|hit| hit.word == "receive"),
        "{recieve:?}"
    );
}

/// 例句索引和词卡是同一份源的两个视图。这里把"两半关键词拼起来 == 卡里的 keys"
/// 在全库上钉死：唯一的分歧可能来自关键词文本里含 `@`，出现了就必须改切分规则。
#[test]
#[ignore = "requires data/dict_full.jsonl"]
fn the_two_example_views_never_drift_apart() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let file = std::io::BufReader::new(
        std::fs::File::open(root.join("data/dict_full.jsonl")).expect("缺少 data/dict_full.jsonl"),
    );
    let mut total = 0usize;
    for (number, line) in file.lines().enumerate() {
        let value: serde_json::Value = serde_json::from_str(&line.unwrap())
            .unwrap_or_else(|e| panic!("第 {} 行不是 JSON: {e}", number + 1));
        let raw: Vec<String> = value["examples"]
            .as_array()
            .map(|array| {
                array
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let card = zedict::data::compile::parse_examples(&raw);
        let sides = zedict::data::compile::example_sides(&raw);
        assert_eq!(
            card.len(),
            sides.len(),
            "第 {} 行两视图条数不一致",
            number + 1
        );
        for (example, side) in card.iter().zip(&sides) {
            total += 1;
            assert_eq!(side.en, example.en);
            assert_eq!(side.zh, example.zh);
            let joined: Vec<String> = side
                .en_keys
                .iter()
                .chain(side.zh_keys.iter())
                .cloned()
                .collect();
            assert_eq!(
                joined,
                example.keys,
                "第 {} 行关键词分叉：{}",
                number + 1,
                example.en
            );
        }
    }
    assert_eq!(total, 232_657, "全库例句条数");
}

/// 词头证据普查的真实数字。构造样例只能证明规则，证明不了规则在 184,093 条记录上
/// 到底各落在多少——而"要挡掉的是哪些记录"正是这个功能对用户的承诺。数字变了必须
/// 是源数据变了或判据变了，两种都该有人看一眼。
#[test]
#[ignore = "requires data/dict_full.jsonl (three full builds)"]
fn the_headword_evidence_census_is_the_published_one() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = tempfile::tempdir().unwrap();
    let build = |threshold: zedict::core::evidence::KeyEvidence| {
        let db = out.path().join(format!("{}.sqlite", threshold.as_str()));
        (
            zedict::data::ingest::import_structured_with_extras(
                &root.join("data/dict_full.jsonl"),
                None,
                &zedict::data::ingest::Extras {
                    key_evidence: threshold,
                    ..Default::default()
                },
                &db,
                None,
            )
            .unwrap(),
            db,
        )
    };
    let (all, all_db) = build(zedict::core::evidence::KeyEvidence::All);
    let (some, some_db) = build(zedict::core::evidence::KeyEvidence::Some);
    let (strong, strong_db) = build(zedict::core::evidence::KeyEvidence::Strong);

    // 档位普查读的是源记录，和阈值无关。109,969 = 带词头的记录数，74,124 = 缺词头。
    for report in [&all, &some, &strong] {
        assert_eq!(report.raw_records, 184_093);
        assert_eq!(report.archived_records, 184_093, "净化不能少归档一条");
        assert_eq!(report.missing_headword_records, 74_124);
        assert_eq!(report.tier_example_key, 52_347);
        assert_eq!(report.tier_in_gloss, 18_065);
        assert_eq!(report.tier_unevidenced, 39_557);
        assert_eq!(
            report.tier_example_key + report.tier_in_gloss + report.tier_unevidenced,
            109_969
        );
    }
    assert_eq!(
        (all.indexed_cards, all.dropped_unevidenced_cards),
        (109_969, 0)
    );
    assert_eq!(all.indexed_headwords, 69_803);
    assert_eq!(
        (some.indexed_cards, some.dropped_unevidenced_cards),
        (70_412, 39_557)
    );
    assert_eq!(some.indexed_headwords, 62_499);
    assert_eq!(
        (strong.indexed_cards, strong.dropped_unevidenced_cards),
        (52_347, 57_622)
    );
    assert_eq!(strong.indexed_headwords, 46_551);

    // 抽查：挡住 E 档之后，这几张错卡的数量必须落到实测的数，而 `promotion` 的
    // 八条全是隔壁 `abbr.` 子表——它该整个消失。
    let rows = |db: &Path, key: &str| {
        zedict::data::store::DictStore::open(db)
            .unwrap()
            .lookup(key)
            .unwrap()
            .map(|hit| hit.cards.len())
            .unwrap_or(0)
    };
    for (db, angle, bomb, promotion) in [
        (&all_db, 28, 15, 8),
        (&some_db, 3, 2, 0),
        (&strong_db, 3, 2, 0),
    ] {
        assert_eq!(rows(db, "angle"), angle);
        assert_eq!(rows(db, "bomb"), bomb);
        assert_eq!(rows(db, "promotion"), promotion);
        DictStore::open(db).unwrap().check_integrity().unwrap();
    }
}
