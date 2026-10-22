//! 查词时的英英翻译：交互终端默认自动，管道靠配置或显式开关。
//!
//! 自动翻译失败必须静默退回英文；JSON 契约不因翻译而改变。所有请求都打到
//! 127.0.0.1 假服务，CI 不触网。

mod common;

use common::{FakeHttp, FakeResponse};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Env {
    fn new(config_text: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.jsonl");
        let record = json!({
            "headword": "love",
            "pos": ["n."],
            "senses": ["<i>n.</i> 爱"],
            "english": "<div class=\"exp\"><span class=\"cara\">n.</span><ol><li>a strong feeling</li><div class=\"eg\"><ul><li>his love for work</li></ul></div></ol></div>",
        });
        std::fs::write(&source, record.to_string() + "\n").unwrap();
        let db = dir.path().join("dict.sqlite");
        zedict::data::ingest::import_structured(&source, None, &db, None).unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, config_text).unwrap();
        Self { dir, config }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env_remove("ZEDICT_EDGE_TRANSLATE_URL")
            .env("ZEDICT_CACHE_DIR", self.path("cache"))
            .current_dir(std::env::temp_dir())
            .arg("--config")
            .arg(&self.config);
        cmd
    }

    fn run(&self, edge: &str, args: &[&str]) -> Output {
        self.command()
            .env("ZEDICT_EDGE_TRANSLATE_URL", edge)
            .args(args)
            .output()
            .unwrap()
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn with_db(db: &Path) -> String {
    format!("db = '{}'\n", db.file_name().unwrap().to_string_lossy())
}

fn edge_translations() -> FakeHttp {
    FakeHttp::new(vec![FakeResponse::json(
        json!([
            {"translations": [{"text": "一种强烈的感情"}]},
            {"translations": [{"text": "他对工作的热爱"}]}
        ])
        .to_string(),
    )])
}

#[test]
fn config_enables_automatic_translation_for_piped_queries() {
    let env = Env::new(&format!(
        "{}en_zh = true\n",
        with_db(Path::new("dict.sqlite"))
    ));
    let fake = edge_translations();
    let output = env.run(&fake.url("/translatetext"), &["query", "love"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("译：一种强烈的感情"), "{stdout}");
    assert!(stdout.contains("译：他对工作的热爱"), "{stdout}");
    assert!(output.stderr.is_empty(), "{}", stderr_of(&output));
    // 词典碎片在送翻前补句点，避免 Edge 截断；译文仍按原句归档。
    let body: Vec<String> = serde_json::from_str(&fake.bodies()[0]).unwrap();
    assert_eq!(body, ["a strong feeling.", "his love for work."]);
}

#[test]
fn piped_queries_stay_offline_without_opt_in() {
    let env = Env::new(&with_db(Path::new("dict.sqlite")));
    let fake = edge_translations();
    let output = env.run(&fake.url("/translatetext"), &["query", "love"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(!stdout_of(&output).contains("译："));
    assert_eq!(fake.count(), 0, "管道输出默认不该发出翻译请求");
}

#[test]
fn explicit_flag_still_forces_translation_without_config() {
    let env = Env::new(&with_db(Path::new("dict.sqlite")));
    let fake = edge_translations();
    let output = env.run(&fake.url("/translatetext"), &["query", "love", "--en-zh"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(stdout_of(&output).contains("译：一种强烈的感情"));
    assert_eq!(fake.count(), 1);
}

#[test]
fn no_flag_overrides_config_and_sends_nothing() {
    let env = Env::new(&format!(
        "{}en_zh = true\n",
        with_db(Path::new("dict.sqlite"))
    ));
    let fake = edge_translations();
    let output = env.run(
        &fake.url("/translatetext"),
        &["query", "love", "--no-en-zh"],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        !stdout_of(&output).contains("译："),
        "{}",
        stdout_of(&output)
    );
    assert_eq!(fake.count(), 0, "关闭后不该发出任何翻译请求");
}

#[test]
fn json_never_carries_translations_even_when_forced() {
    let env = Env::new(&format!(
        "{}en_zh = true\n",
        with_db(Path::new("dict.sqlite"))
    ));
    let fake = edge_translations();
    let output = env.run(
        &fake.url("/translatetext"),
        &["query", "love", "--json", "--en-zh"],
    );
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(fake.count(), 0, "JSON 输出不该触发翻译请求");
    let value: serde_json::Value = serde_json::from_str(stdout_of(&output).trim()).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert!(!stdout_of(&output).contains("译："));
    assert!(stderr_of(&output).contains("--en-zh"));
}

#[test]
fn automatic_failure_is_silent_and_keeps_the_english_card() {
    let env = Env::new(&format!(
        "{}en_zh = true\n",
        with_db(Path::new("dict.sqlite"))
    ));
    // 403 是终止错误，Edge 不会退避重试，测试不会为失败等待两秒。
    let fake = FakeHttp::new(vec![FakeResponse::status(403)]);
    let output = env.run(&fake.url("/translatetext"), &["query", "love"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("a strong feeling"), "{stdout}");
    assert!(!stdout.contains("译："), "{stdout}");
    assert!(
        output.stderr.is_empty(),
        "自动翻译失败不该刷错误: {}",
        stderr_of(&output)
    );
}

#[test]
fn explicit_failure_is_reported() {
    let env = Env::new(&with_db(Path::new("dict.sqlite")));
    let fake = FakeHttp::new(vec![FakeResponse::status(403)]);
    let output = env.run(&fake.url("/translatetext"), &["query", "love", "--en-zh"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(stdout_of(&output).contains("a strong feeling"));
    assert!(
        stderr_of(&output).contains("英英翻译失败"),
        "{}",
        stderr_of(&output)
    );
}

/// 配了 LLM 但端点坏掉时，不能把整张卡的译文一起丢掉——Edge 还通就继续用 Edge。
#[test]
fn llm_failure_falls_back_to_edge() {
    let env = Env::new(&format!(
        "{}en_zh = true\n",
        with_db(Path::new("dict.sqlite"))
    ));
    let llm = FakeHttp::new(vec![FakeResponse::status(500)]);
    let edge = edge_translations();
    let output = env
        .command()
        .env("LLM_API_KEY", "sk-test")
        .env("LLM_BASE_URL", llm.url("/v1"))
        .env("ZEDICT_EDGE_TRANSLATE_URL", edge.url("/translatetext"))
        .args(["query", "love"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(stdout_of(&output).contains("译：一种强烈的感情"));
    assert_eq!(llm.count(), 1, "应先尝试配置的大模型");
    assert_eq!(edge.count(), 1, "大模型失败后应改走 Edge");
    assert!(output.stderr.is_empty(), "{}", stderr_of(&output));
}
