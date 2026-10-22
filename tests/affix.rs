//! 构词提示只用本地词库里已有的前缀词条和基础词，宁可少给也不硬拆。
//! 整词释义必须能和基础词对上才给提示：直接包含基础词义、带前缀语义标记，
//! 或和常见基础词共享实义字。曾在大词库上出现的假拆分都钉在这里。
use std::collections::HashMap;
use std::path::Path;
use zedict::{
    data::ingest::{self, Extras},
    data::store::{DictStore, QueryRequest, SearchMode},
    render::{self, Theme},
};

fn build(dir: &Path) -> DictStore {
    let source = dir.join("source.jsonl");
    let levels = dir.join("levels.jsonl");
    let db = dir.join("dict.sqlite");
    let rows = [
        serde_json::json!({"headword":"un-","senses":["pref. 表示“不”：unhappy 不高兴的"]}),
        serde_json::json!({"headword":"re-","senses":["pref. 表示“再；又”：return 返回"]}),
        serde_json::json!({"headword":"pre-","senses":["pref. 表示“前；预先”：preview 预览"]}),
        serde_json::json!({"headword":"tele-","senses":["pref. 表示“远；电”：telephone 电话"]}),
        serde_json::json!({"headword":"over-","senses":["pref. 表示“过度”：overcoat 大衣"]}),
        serde_json::json!({"headword":"out-","senses":["pref. 表示“出；外”：outside 外面的"]}),
        serde_json::json!({"headword":"super-","senses":["pref. 表示“超”：supermarket 超市"]}),
        serde_json::json!({"headword":"sub-","senses":["pref. 表示“下；次”：subway 地铁"]}),
        serde_json::json!({"headword":"pro-","senses":["pref. 表示“向前”：profile 轮廓"]}),
        serde_json::json!({"headword":"de-","senses":["pref. 表示“向下”：decline 下降"]}),
        serde_json::json!({"headword":"trans-","senses":["pref. 表示“横过”：translate 翻译"]}),
        serde_json::json!({"headword":"bi-","senses":["pref. 表示“二；双”"]}),
        serde_json::json!({"headword":"co-","senses":["pref. 表示“共同”"]}),
        serde_json::json!({"headword":"uni-","senses":["pref. 表示“单”"]}),
        serde_json::json!({"headword":"zed-","senses":["pref. 表示“测试”"]}),
        serde_json::json!({"headword":"happy","senses":["<i>adj.</i> 高兴的；快乐的"]}),
        serde_json::json!({"headword":"unhappy","senses":["<i>adj.</i> 不高兴的；伤心的"]}),
        serde_json::json!({"headword":"turn","senses":["<i>v.</i> （使）转动，旋转"]}),
        serde_json::json!({"headword":"return","senses":["<i>v.</i> 返回；归还"]}),
        serde_json::json!({"headword":"view","senses":["<i>n.</i> 看；视野；景色；看法"]}),
        serde_json::json!({"headword":"preview","senses":["<i>v.</i> 预览；事先查看"]}),
        serde_json::json!({"headword":"phone","senses":["<i>n.</i> 电话；耳机"]}),
        serde_json::json!({"headword":"telephone","senses":["<i>n.</i> 电话"]}),
        serde_json::json!({"headword":"coat","senses":["<i>n.</i> 外套；上衣"]}),
        serde_json::json!({"headword":"overcoat","senses":["<i>n.</i> 大衣"]}),
        serde_json::json!({"headword":"side","senses":["<i>n.</i> 边；侧面"]}),
        serde_json::json!({"headword":"outside","senses":["<i>adv.</i> 外面的；户外的"]}),
        serde_json::json!({"headword":"market","senses":["<i>n.</i> 市场；集市"]}),
        serde_json::json!({"headword":"supermarket","senses":["<i>n.</i> 超市"]}),
        serde_json::json!({"headword":"able","senses":["<i>adj.</i> 能够…的；有才干的"]}),
        serde_json::json!({"headword":"unable","senses":["<i>adj.</i> 不能的；不会的"]}),
        serde_json::json!({"headword":"way","senses":["<i>n.</i> 道路；方法"]}),
        serde_json::json!({"headword":"subway","senses":["<i>n.</i> 地铁"]}),
        serde_json::json!({"headword":"file","senses":["<i>n.</i> 文件夹；档案"]}),
        serde_json::json!({"headword":"profile","senses":["<i>n.</i> 轮廓；剖面"]}),
        serde_json::json!({"headword":"test","senses":["<i>n.</i> 检验；试验"]}),
        serde_json::json!({"headword":"protest","senses":["<i>v.</i> 抗议"]}),
        serde_json::json!({"headword":"main","senses":["<i>adj.</i> 主要的"]}),
        serde_json::json!({"headword":"remain","senses":["<i>v.</i> 仍然是"]}),
        serde_json::json!({"headword":"cent","senses":["<i>n.</i> 分；一分的硬币"]}),
        serde_json::json!({"headword":"recent","senses":["<i>adj.</i> 不久前的"]}),
        serde_json::json!({"headword":"veal","senses":["<i>n.</i> 小牛肉"]}),
        serde_json::json!({"headword":"reveal","senses":["<i>v.</i> 揭示；揭露"]}),
        serde_json::json!({"headword":"liver","senses":["<i>n.</i> 肝"]}),
        serde_json::json!({"headword":"deliver","senses":["<i>v.</i> 递送；发表"]}),
        serde_json::json!({"headword":"cline","senses":["<i>n.</i> 渐变群；生态群"]}),
        serde_json::json!({"headword":"decline","senses":["<i>v.</i> 下降；拒绝"]}),
        serde_json::json!({"headword":"late","senses":["<i>adj.</i> 迟的；晚的"]}),
        serde_json::json!({"headword":"translate","senses":["<i>v.</i> 翻译；转化"]}),
        serde_json::json!({"headword":"form","senses":["<i>n.</i> 形式；形状","<i>v.</i> 形成"]}),
        serde_json::json!({"headword":"transform","senses":["<i>v.</i> 改变；转化"]}),
        serde_json::json!({"headword":"vent","senses":["<i>n.</i> 出口；通风孔","<i>vt.</i> 发泄"]}),
        serde_json::json!({"headword":"prevent","senses":["<i>vt.</i> 预防；防止"]}),
        serde_json::json!({"headword":"parent","senses":["<i>n.</i> 父母亲；根源"]}),
        serde_json::json!({"headword":"transparent","senses":["<i>adj.</i> 透明的"]}),
        serde_json::json!({"headword":"vise","senses":["<i>n.</i> 虎钳；签证"]}),
        serde_json::json!({"headword":"supervise","senses":["<i>v.</i> 监督；管理"]}),
        serde_json::json!({"headword":"corn","senses":["<i>n.</i> 玉米"]}),
        serde_json::json!({"headword":"unicorn","senses":["<i>n.</i> 独角兽"]}),
        serde_json::json!({"headword":"ver","senses":["<i>n.</i> 春天"]}),
        serde_json::json!({"headword":"cover","senses":["<i>v.</i> 覆盖；包括"]}),
        serde_json::json!({"headword":"it","senses":["<i>pron.</i> 它"]}),
        serde_json::json!({"headword":"unit","senses":["<i>n.</i> 单位；单元"]}),
        serde_json::json!({"headword":"come","senses":["<i>v.</i> 来"]}),
        serde_json::json!({"headword":"bird","senses":["<i>n.</i> 鸟"]}),
        serde_json::json!({"headword":"est","senses":["<i>abbr.</i> 东部标准时间"]}),
        serde_json::json!({"headword":"thing","senses":["<i>n.</i> 东西"]}),
        serde_json::json!({"headword":"unest","senses":["不应拆成 un- + est"]}),
        serde_json::json!({"headword":"zedthing","senses":["不应拆成 zed- + thing"]}),
    ];
    let lines: Vec<_> = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect();
    std::fs::write(&source, lines.join("\n") + "\n").unwrap();

    let level_rows = [
        serde_json::json!({
            "kind": "meta",
            "source": {"file": "level.bin", "sha256": "0", "words": 30},
            "bands": [
                {"level":1,"name":"b1","desc":"常用","color":"000000","items":30},
                {"level":2,"name":"b2","desc":"次常用","color":"000000","items":2},
                {"level":4,"name":"b4","desc":"进阶","color":"000000","items":1}
            ],
            "tags": [{"tag":"test","name":"测试","desc":"测试","color":"000000","items":0}]
        }),
        serde_json::json!({"kind":"word","word":"happy","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"turn","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"view","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"phone","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"coat","level":2,"tags":[]}),
        serde_json::json!({"kind":"word","word":"side","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"market","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"able","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"way","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"file","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"test","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"main","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"cent","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"late","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"parent","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"corn","level":2,"tags":[]}),
        serde_json::json!({"kind":"word","word":"form","level":1,"tags":[]}),
        serde_json::json!({"kind":"word","word":"vent","level":4,"tags":[]}),
        serde_json::json!({"kind":"word","word":"thing","level":1,"tags":[]}),
        // 以下基础词故意不给等级：只靠语义对上才能拆，不能拿“像常用词”兜底。
        // veal / cline / vise / ver / it 都不该成为拆分的支点。
    ];
    let level_lines: Vec<_> = level_rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect();
    std::fs::write(&levels, level_lines.join("\n") + "\n").unwrap();

    ingest::import_structured_with_extras(
        &source,
        None,
        &Extras {
            levels: Some(&levels),
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    DictStore::open(&db).unwrap()
}

#[test]
fn splits_a_word_into_a_known_prefix_and_base() {
    let dir = tempfile::tempdir().unwrap();
    let store = build(dir.path());
    assert!(store.lookup("un-").unwrap().is_some(), "前缀词条要入库");
    assert!(store.lookup("happy").unwrap().is_some(), "基础词要入库");
    let hints = store.affix_hints("unhappy", 1).unwrap();
    assert_eq!(hints.len(), 1, "{hints:?}");
    let hint = &hints[0];
    assert_eq!(hint.prefix, "un-");
    assert_eq!(hint.meaning, "不");
    assert_eq!(hint.base, "happy");
    assert_eq!(hint.base_gloss, "高兴的");
    assert_eq!(hint.word_gloss, "不高兴的");

    let ret = store.affix_hints("return", 1).unwrap();
    assert_eq!(ret[0].prefix, "re-");
    assert_eq!(ret[0].meaning, "再；又");
    assert_eq!(ret[0].base, "turn");
    // “（使）转动”里的用法标记要去掉，基础词义只留“转动”。
    assert_eq!(ret[0].base_gloss, "转动");
    assert_eq!(ret[0].word_gloss, "返回");
    // 大写查询走同一套归一。
    assert_eq!(store.affix_hints("UNHAPPY", 1).unwrap(), hints);
}

#[test]
fn keeps_high_confidence_splits() {
    let dir = tempfile::tempdir().unwrap();
    let store = build(dir.path());
    let hint = |word: &str| {
        let list = store.affix_hints(word, 1).unwrap();
        assert_eq!(list.len(), 1, "{word} 应该有提示: {list:?}");
        list.into_iter().next().unwrap()
    };

    let preview = hint("preview");
    assert_eq!(
        (preview.prefix.as_str(), preview.base.as_str()),
        ("pre-", "view")
    );
    assert_eq!(preview.base_gloss, "看");
    assert_eq!(preview.word_gloss, "事先查看");

    let telephone = hint("telephone");
    assert_eq!(
        (telephone.prefix.as_str(), telephone.base.as_str()),
        ("tele-", "phone")
    );
    assert_eq!(telephone.base_gloss, "电话");
    assert_eq!(telephone.word_gloss, "电话");

    // over- 的意思在“大衣”里看不出来，靠“coat（外套）”共享的“衣”放行。
    let overcoat = hint("overcoat");
    assert_eq!(
        (overcoat.prefix.as_str(), overcoat.base.as_str()),
        ("over-", "coat")
    );

    for (word, prefix, base) in [
        ("outside", "out-", "side"),
        ("supermarket", "super-", "market"),
        ("unable", "un-", "able"),
        ("subway", "sub-", "way"),
        // 最弱一档：前缀语义标记 + 常见基础词 + 词性重合。
        ("transform", "trans-", "form"),
        ("prevent", "pre-", "vent"),
    ] {
        let found = hint(word);
        assert_eq!(
            (found.prefix.as_str(), found.base.as_str()),
            (prefix, base),
            "{word}"
        );
    }
}

#[test]
fn rejects_lookalike_splits_without_semantic_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = build(dir.path());
    for word in [
        "profile",
        "protest",
        "remain",
        "recent",
        "reveal",
        "deliver",
        "decline",
        "translate",
        "transparent",
        "supervise",
        "unicorn",
        "bird",
        "come",
        "cover",
        "unit",
    ] {
        assert!(
            store.affix_hints(word, 1).unwrap().is_empty(),
            "{word} 不应该硬拆"
        );
    }
    // est 只有缩写词性，不能当作词根。
    assert!(store.affix_hints("unest", 1).unwrap().is_empty());
    // zed- 不在常见前缀表里。
    assert!(store.affix_hints("zedthing", 1).unwrap().is_empty());
    assert!(store.affix_hints("un", 1).unwrap().is_empty());
    assert!(store.affix_hints("不高兴", 1).unwrap().is_empty());
    assert!(store.affix_hints("unzox", 1).unwrap().is_empty());
}

#[test]
fn hints_appear_only_in_full_human_output() {
    let dir = tempfile::tempdir().unwrap();
    let store = build(dir.path());
    let response = store
        .query(&QueryRequest {
            query: "unhappy".into(),
            mode: SearchMode::Exact,
            limit: 8,
        })
        .unwrap();
    let mut hints = HashMap::new();
    hints.insert(
        "unhappy".to_string(),
        store.affix_hints("unhappy", 1).unwrap(),
    );

    let full = render::response_themed_with_hints(&response, &hints, false, 100, Theme::Ascii);
    assert!(full.contains("[记忆提示]"), "{full}");
    assert!(
        full.contains("un-（不）+ happy（高兴的） → 不高兴的"),
        "{full}"
    );
    assert!(!full.contains('\u{1b}'), "ascii 主题不带 ANSI");

    let brief = render::response_themed_with_hints(&response, &hints, true, 100, Theme::Ascii);
    assert!(!brief.contains("记忆提示"), "{brief}");
    let plain = render::response(&response, false, 100);
    assert!(!plain.contains("记忆提示"), "{plain}");
}

#[test]
fn the_cli_wires_hints_into_full_text_only() {
    let dir = tempfile::tempdir().unwrap();
    drop(build(dir.path()));
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "db = 'dict.sqlite'\n").unwrap();
    let run = |extra: &[&str]| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_THEME")
            .env_remove("ZEDICT_BACKGROUND")
            .env_remove("NO_COLOR")
            .arg("--config")
            .arg(&config)
            .arg("--theme")
            .arg("ascii");
        cmd.args(extra).output().unwrap()
    };

    let out = run(&["query", "unhappy"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("[记忆提示]"), "{stdout}");
    assert!(stdout.contains("un-（不）+ happy（高兴的）"), "{stdout}");

    let brief = run(&["query", "unhappy", "--brief"]);
    assert!(!String::from_utf8_lossy(&brief.stdout).contains("记忆提示"));

    let json = run(&["query", "unhappy", "--json"]);
    let stdout = String::from_utf8_lossy(&json.stdout);
    assert!(!stdout.contains("记忆提示"), "{stdout}");
    let _: serde_json::Value = serde_json::from_str(&stdout).unwrap();
}
