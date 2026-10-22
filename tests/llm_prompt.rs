//! LLM 通路的提示词面：这些断言读的是**发出去的请求体**，不是内部函数返回值。
//!
//! 理由很实在：`--to`、`--style-file`、`--glossary-auto` 的失败方式都是"提示词里
//! 说的话和用户要的不一样"，只有在线路末端检查才算数。全部请求都打到
//! 127.0.0.1 的假服务，CI 不触网。

mod common;

use common::{FakeHttp, FakeResponse};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Output;
use zedict::core::evidence::KeyEvidence;

struct Env {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "brief = true\n").unwrap();
        std::fs::create_dir_all(dir.path().join("cache")).unwrap();
        Self { dir, config }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// `db` 用 `--db` 传，不写进配置：同一个 Env 既服务无词库的提示词测试，也服务
    /// 要读 `key_evidence` 的自动术语测试。
    fn command(&self, db: Option<&Path>) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env("ZEDICT_CACHE_DIR", self.path("cache"))
            .current_dir(std::env::temp_dir())
            .arg("--config")
            .arg(&self.config);
        if let Some(db) = db {
            cmd.arg("--db").arg(db);
        }
        cmd
    }

    /// `--base-url`/`--api-key` 挂在 `TranslateArgs` 上而不是全局参数，所以必须跟在子命令
    /// 后面。放在前面只会被 clap 拒绝，而 clap 的退出码也是 2，测试就分不清是谁报的错。
    fn run(&self, endpoint: &str, db: Option<&Path>, args: &[&str]) -> Output {
        self.command(db)
            .args(args)
            .arg("--base-url")
            .arg(endpoint)
            .arg("--api-key")
            .arg("test-only")
            .output()
            .unwrap()
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// 发给模型的两条消息。`messages[1]` 是用户消息，术语表/上下文/上一段译文都在里面。
fn messages(body: &str) -> (String, String) {
    let value: serde_json::Value = serde_json::from_str(body).expect("请求体必须是 JSON");
    let content = |index: usize| {
        value["messages"][index]["content"]
            .as_str()
            .unwrap_or_else(|| panic!("第 {index} 条消息不是字符串: {body}"))
            .to_string()
    };
    (content(0), content(1))
}

/// 用户消息里的术语行左边（术语本身）。用它断言"哪些词被钉死了"，比在整段文本里
/// 找子串可靠——原文本身也含有这些词。
fn glossary_terms(body: &str) -> Vec<String> {
    let (_, user) = messages(body);
    user.lines()
        .filter_map(|line| line.trim_start().split_once(" → "))
        .map(|(term, _)| term.trim().to_string())
        .collect()
}

/// 三个短语、三档证据：`a right angle` 自证于例句高亮词（C），`right triangle` 只在
/// 自己的释义里出现（D），`gas constant` 两边都没有（E）——它的释义其实就是隔壁词条的。
fn phrase_records() -> Vec<String> {
    [
        json!({"headword":"a right angle","pos":["n."],"senses":["<i>n.</i> 直角①"],
               "examples":["He turned through <span class=\"key\">a right angle</span>@他转了个直角"]}),
        json!({"headword":"right triangle","pos":["n."],
               "senses":["<i>n.</i> right triangle 直角三角形②"]}),
        json!({"headword":"gas constant","pos":["n."],"senses":["<i>n.</i> 阿伏伽德罗常量③"],
               "english":"Avogadro constant"}),
    ]
    .into_iter()
    .map(|record| record.to_string())
    .collect()
}

/// `threshold` 为 `All` 时库里**没有** `key_evidence` 表，正是自动术语必须报错的那种库。
fn phrase_store(stem: &str, threshold: KeyEvidence) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.jsonl");
    std::fs::write(&source, phrase_records().join("\n") + "\n").unwrap();
    let db = dir.path().join(format!("{stem}.sqlite"));
    zedict::data::ingest::import_structured_with_extras(
        &source,
        None,
        &zedict::data::ingest::Extras {
            key_evidence: threshold,
            ..Default::default()
        },
        &db,
        None,
    )
    .unwrap();
    (dir, db)
}

const SOURCE: &str =
    "He turned through a right angle and recorded the gas constant of a right triangle.";

#[test]
fn the_target_language_reaches_the_request() {
    let env = Env::new();
    let fake = FakeHttp::new(vec![FakeResponse::sse(&["Bonjour"])]);
    let output = env.run(
        &fake.url("/v1"),
        None,
        &["translate", "hello", "--engine", "llm", "--to", "fr"],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (system, user) = messages(&fake.bodies()[0]);
    assert!(system.contains("法语"), "{system}");
    assert!(system.contains("（fr）"), "{system}");
    assert!(!system.contains("简体中文"), "{system}");
    assert!(user.contains("法语"), "{user}");
    assert!(!user.contains("中文"), "{user}");
}

/// 逐句对照走的是另一套提示词（批量 JSON 数组），`--to` 也必须在那里成立。
#[test]
fn the_target_language_reaches_the_batch_prompt_too() {
    let env = Env::new();
    let fake = FakeHttp::new(vec![FakeResponse::sse(&[r#"["Bonjour.","Ça va."]"#])]);
    let output = env.run(
        &fake.url("/v1"),
        None,
        &[
            "translate",
            "Hello. How goes?",
            "--engine",
            "llm",
            "--bilingual",
            "--to",
            "de",
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (system, _) = messages(&fake.bodies()[0]);
    assert!(system.contains("德语"), "{system}");
    assert!(
        system.contains("逐词直译成德语") || system.contains("翻译成自然通顺的德语"),
        "{system}"
    );
    assert!(!system.contains("简体中文"), "{system}");
}

#[test]
fn a_malformed_language_code_is_a_parameter_error_and_sends_nothing() {
    let env = Env::new();
    let fake = FakeHttp::new(vec![]);
    for (flag, code) in [("--to", "en?to=zh"), ("--from", "../x"), ("--to", "")] {
        let output = env.run(
            &fake.url("/v1"),
            None,
            &["translate", "hello", "--engine", "llm", flag, code],
        );
        assert_eq!(output.status.code(), Some(2), "{flag} {code}");
        assert!(stderr_of(&output).contains("参数错误"), "{flag} {code}");
    }
    assert_eq!(fake.count(), 0, "语言代码非法时不该发出任何请求");
}

/// 表里没有但形状合法的代码：按原样写进提示词，不悄悄退回中文。
#[test]
fn an_unlisted_language_is_still_requested_by_its_code() {
    let env = Env::new();
    let fake = FakeHttp::new(vec![FakeResponse::sse(&["Tena"])]);
    let output = env.run(
        &fake.url("/v1"),
        None,
        &["translate", "hello", "--engine", "llm", "--to", "mi"],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (system, user) = messages(&fake.bodies()[0]);
    assert!(system.contains("目标语言：mi（mi）"), "{system}");
    assert!(user.contains("Translate the text to mi"), "{user}");
    assert!(!user.contains("中文"), "{user}");
}

#[test]
fn a_style_file_replaces_the_wording_but_not_the_language() {
    let env = Env::new();
    let template = env.write(
        "style.txt",
        "把下面这段翻成{{target_lang_cn}}（{{target_lang}}），共 {{count}} 项：\n{{text}}\n语境={{context}}|上一段={{prev_output}}|摘要={{summary}}|源={{src_lang_cn}}",
    );
    let fake = FakeHttp::new(vec![FakeResponse::sse(&["Bonjour"])]);
    let output = env.run(
        &fake.url("/v1"),
        None,
        &[
            "translate",
            "hello",
            "--engine",
            "llm",
            "--to",
            "fr",
            "--context",
            "菜单",
            "--style-file",
            template.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (_, user) = messages(&fake.bodies()[0]);
    assert!(
        user.starts_with("把下面这段翻成法语（fr），共 1 项：\nhello"),
        "{user}"
    );
    // 没给的变量渲染成空串，而不是留下花括号让模型自己去猜。
    assert!(user.contains("语境=菜单|上一段=|摘要=|源=未声明"), "{user}");
    assert!(!user.contains("{{"));
}

#[test]
fn a_template_that_cannot_work_is_a_parameter_error() {
    let env = Env::new();
    let fake = FakeHttp::new(vec![]);
    let cases = [
        ("未知变量", "{{text}} 用 {{style}} 写"),
        ("缺原文入口", "只要 {{target_lang}}"),
        ("花括号没配对", "{{text} 后面"),
        ("空模板", "   "),
    ];
    for (label, text) in cases {
        let template = env.write(&format!("{label}.txt"), text);
        let output = env.run(
            &fake.url("/v1"),
            None,
            &[
                "translate",
                "hello",
                "--engine",
                "llm",
                "--style-file",
                template.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(2), "{label}");
        assert!(stderr_of(&output).contains("参数错误"), "{label}");
    }
    assert_eq!(fake.count(), 0, "模板不合法时不该发出任何请求");
}

/// 分段翻译时，第二段必须看得见第一段的结果；`--no-prev-output` 关掉的就是这一行。
///
/// `--chunk` 的预算按 UTF-16 码元算，段落之间的空行自己也是一个单元，所以分段数
/// 由打包器决定而不是数段落个数——这里只要求"至少分出两段"，并检查其后每一段。
#[test]
fn the_previous_translation_is_carried_into_the_next_segment() {
    for (tail, expect) in [(Vec::new(), true), (vec!["--no-prev-output"], false)] {
        let env = Env::new();
        let fake = FakeHttp::new(
            ["甲。", "乙。", "丙。", "丁。", "戊。", "己。"]
                .iter()
                .map(|text| FakeResponse::sse(&[*text]))
                .collect(),
        );
        let mut args = vec![
            "translate",
            "Alpha.\n\nBeta.\n\nGamma.",
            "--engine",
            "llm",
            "--chunk",
            "7",
        ];
        args.extend(tail.iter().copied());
        let output = env.run(&fake.url("/v1"), None, &args);
        assert!(output.status.success(), "{}", stderr_of(&output));
        let bodies = fake.bodies();
        assert!(bodies.len() >= 2, "分段没生效: {bodies:?}");
        let first = messages(&bodies[0]).1;
        assert!(
            !first.contains("[上一段译文]"),
            "首段没有上一段，不该出现空占位: {first}"
        );
        assert_eq!(
            messages(&bodies[1]).1.contains("[上一段译文] 甲。"),
            expect,
            "{tail:?}"
        );
        for body in &bodies[2..] {
            assert_eq!(
                messages(body).1.contains("[上一段译文]"),
                expect,
                "{tail:?}"
            );
        }
    }
}

#[test]
fn automatic_glossary_only_pins_headwords_that_attest_to_themselves() {
    let (store, db) = phrase_store("some", KeyEvidence::Some);
    let env = Env::new();
    let fake = FakeHttp::new(vec![FakeResponse::sse(&["译文"])]);
    let output = env.run(
        &fake.url("/v1"),
        Some(&db),
        &["translate", SOURCE, "--engine", "llm", "--glossary-auto"],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(glossary_terms(&fake.bodies()[0]), ["a right angle"]);
    assert!(
        !fake.bodies()[0].contains("阿伏伽德罗常量"),
        "E 档词的释义绝不能被钉进提示词"
    );
    let fake = FakeHttp::new(vec![FakeResponse::sse(&["译文"])]);
    let output = env.run(
        &fake.url("/v1"),
        Some(&db),
        &[
            "translate",
            SOURCE,
            "--engine",
            "llm",
            "--glossary-auto",
            "some",
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    // 放宽到 `some` 才多得出弱证据那条；E 档在任何阈值下都不入围。
    assert_eq!(
        glossary_terms(&fake.bodies()[0]),
        ["a right angle", "right triangle"]
    );
    assert!(!fake.bodies()[0].contains("阿伏伽德罗常量"));
    drop(store);
}

/// 手工表优先，自动表只能补空位。
#[test]
fn a_hand_written_glossary_row_wins_over_the_automatic_one() {
    let (_store, db) = phrase_store("some2", KeyEvidence::Some);
    let env = Env::new();
    let manual = env.write("terms.tsv", "a right angle\t自己写的译法\n");
    let fake = FakeHttp::new(vec![FakeResponse::sse(&["译文"])]);
    let output = env.run(
        &fake.url("/v1"),
        Some(&db),
        &[
            "translate",
            SOURCE,
            "--engine",
            "llm",
            "--glossary-auto",
            "--glossary",
            manual.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    let (_, user) = messages(&fake.bodies()[0]);
    assert!(user.contains("a right angle → 自己写的译法"), "{user}");
    assert!(
        !user.contains("直角①"),
        "自动表不该覆盖用户写过的行:\n{user}"
    );
}

/// 没有 `key_evidence` 表的库给不出"哪些词是自证的"。这时只能报错，不能退化成
/// "任意键都算术语"——那等于把 39,557 条 E 档错词的译法全部钉死。
#[test]
fn a_library_without_tiers_refuses_to_guess_terms() {
    let (_store, db) = phrase_store("all", KeyEvidence::All);
    let env = Env::new();
    let fake = FakeHttp::new(vec![]);
    let output = env.run(
        &fake.url("/v1"),
        Some(&db),
        &["translate", SOURCE, "--engine", "llm", "--glossary-auto"],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("key_evidence"), "{stderr}");
    assert!(
        stderr.contains("--key-evidence"),
        "要给出重建命令: {stderr}"
    );
    assert_eq!(fake.count(), 0);
}
