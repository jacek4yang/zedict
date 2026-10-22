//! `zedict explain` 的离线测试：本地词库做 grounding，大模型只准照着说。
//!
//! 关键点不是模型答得好不好，而是**它拿到的是什么**：义项必须来自词库、词形要归并、
//! 未收录要明说，且同一请求第二次不再花钱。子进程一律指向 127.0.0.1 假服务。

mod common;

use common::{FakeHttp, FakeResponse, Fixture};
use serde_json::{json, Value};

fn sse(parts: &[&str]) -> FakeResponse {
    FakeResponse::sse(parts)
}

fn explain_cli(f: &Fixture, base: &str) -> std::process::Command {
    let mut cmd = f.cli();
    cmd.env("LLM_API_KEY", "sk-fake-key-for-local-tests")
        .env("LLM_BASE_URL", format!("{base}/v1"));
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

/// (system, user) 两条消息，顺便检查请求形态
fn messages(body: &str) -> (String, String) {
    let value: Value = serde_json::from_str(body).expect("请求体应当是 OpenAI 兼容 JSON");
    assert_eq!(value["stream"], json!(true), "讲解必须走流式");
    assert_eq!(value["temperature"], json!(0.3), "讲解用固定低温");
    let content = |i: usize| {
        value["messages"][i]["content"]
            .as_str()
            .unwrap_or_else(|| panic!("messages[{i}] 缺 content: {body}"))
            .to_string()
    };
    (content(0), content(1))
}

fn explain_dir(f: &Fixture) -> std::path::PathBuf {
    f.cache.join("explain")
}

fn cache_files(f: &Fixture) -> Vec<String> {
    std::fs::read_dir(explain_dir(f))
        .unwrap_or_else(|_| panic!("缓存目录不存在: {}", explain_dir(f).display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

fn entry_count(f: &Fixture) -> usize {
    std::fs::read_dir(explain_dir(f))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

#[test]
fn the_grounding_block_only_carries_verified_senses() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["robust 是", "“强健的”"])]);
    let (code, stdout, stderr) =
        run(explain_cli(&f, &fake.base).args(["explain", "robust", "--json"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(fake.count(), 1);
    assert!(fake.seen()[0].path.ends_with("/v1/chat/completions"));

    let (system, user) = messages(&fake.seen()[0].body);
    assert!(
        system.contains("不得引入 [本地词库] 之外的义项"),
        "实际: {system}"
    );
    assert!(user.contains("[text] robust"));
    assert!(user.contains("强健的"), "义项要真送进去: {user}");
    assert!(
        !user.contains("串入的错误义项"),
        "审核未通过的义项不得进提示词"
    );
    assert!(!user.contains("[context]"), "没给语境就不该有空标签");

    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["word"], "robust");
    assert_eq!(value["grounded"], json!(true));
    assert_eq!(value["headword"], "robust");
    assert_eq!(value["match_type"], "exact");
    assert_eq!(value["senses"], json!(1));
    assert_eq!(value["style"], "definition");
    assert_eq!(value["cefr"], Value::Null);
    assert_eq!(value["explanation"], "robust 是“强健的”");
    assert_eq!(value["engine"], "llm");
    assert_eq!(value["model"], "deepseek-chat");
    assert_eq!(value["cache"], json!(false));
    assert!(stderr.is_empty(), "命中词库不该有提示: {stderr}");
}

#[test]
fn an_inflected_query_is_explained_under_its_lemma() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["跑"])]);
    let (code, stdout, stderr) =
        run(explain_cli(&f, &fake.base).args(["explain", "running", "--json"]));
    assert_eq!(code, Some(0), "{stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["headword"], "run", "词形要归并到原形");
    assert_eq!(value["match_type"], "lemma");
    let (_system, user) = messages(&fake.seen()[0].body);
    assert!(user.contains("[本地词库] run（lemma）"), "实际: {user}");
    assert!(user.contains("[text] running"), "问的词还得是 running");
}

#[test]
fn a_missing_word_is_stated_not_invented() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["这个词不在本地词库里"])]);
    let (code, stdout, stderr) =
        run(explain_cli(&f, &fake.base).args(["explain", "sesquipedalian", "--json"]));
    assert_eq!(code, Some(0), "未收录不是错误");
    assert!(stderr.contains("本地词库未收录"), "要给出提示: {stderr}");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["grounded"], json!(false));
    assert_eq!(value["headword"], Value::Null);
    assert_eq!(value["senses"], Value::Null);
    let (_system, user) = messages(&fake.seen()[0].body);
    assert!(user.contains("[本地词库] （未收录该词）"), "实际: {user}");
}

#[test]
fn the_same_request_is_answered_from_the_cache() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["第一", "次"])]);
    let ask = || run(explain_cli(&f, &fake.base).args(["explain", "apple", "--json"]));
    let (code, first, err) = ask();
    assert_eq!(code, Some(0), "{err}");
    let (code, second, err) = ask();
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(fake.count(), 1, "同一个请求不该再花一次钱");

    let first: Value = serde_json::from_str(first.trim()).unwrap();
    let second: Value = serde_json::from_str(second.trim()).unwrap();
    assert_eq!(first["cache"], json!(false));
    assert_eq!(second["cache"], json!(true));
    assert_eq!(first["explanation"], second["explanation"]);
    assert_eq!(second["explanation"], "第一次");

    let files = cache_files(&f);
    assert_eq!(files.len(), 1, "一个请求一个文件，实际: {files:?}");
    let name = &files[0];
    assert!(
        name.starts_with("explain-") && name.ends_with(".json"),
        "实际: {name}"
    );
    assert_eq!(name.len(), "explain-".len() + 64 + ".json".len());
    let raw = std::fs::read_to_string(explain_dir(&f).join(name)).unwrap();
    assert!(!raw.contains("sk-fake-key"), "缓存文件不许带密钥");
    assert!(!raw.contains("127.0.0.1"), "缓存文件不许带端点");
}

#[test]
fn every_input_that_changes_the_answer_changes_the_key() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["x"]); 6]);
    for flags in [
        vec!["explain", "apple"],
        vec!["explain", "apple", "--style", "examples"],
        vec!["explain", "apple", "--cefr", "a2"],
        vec!["explain", "apple", "--in-context", "An apple a day."],
        vec!["explain", "apple", "--model", "other-model"],
    ] {
        let (code, stdout, stderr) = run(explain_cli(&f, &fake.base).args(&flags).arg("--json"));
        assert_eq!(code, Some(0), "{flags:?}: {stderr}");
        let value: Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(value["cache"], json!(false), "{flags:?} 是新请求");
    }
    assert_eq!(fake.count(), 5, "风格、等级、语境、模型都要重算");
    assert_eq!(cache_files(&f).len(), 5);

    // --no-cache 跳过读取但照样写回
    let (code, stdout, stderr) =
        run(explain_cli(&f, &fake.base).args(["explain", "apple", "--json", "--no-cache"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(fake.count(), 6, "--no-cache 必然发请求");
    assert_eq!(
        serde_json::from_str::<Value>(stdout.trim()).unwrap()["cache"],
        json!(false)
    );
    assert_eq!(entry_count(&f), 5, "同键覆盖，不新增文件");
}

#[test]
fn context_and_level_reach_the_prompt() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["语境义项"])]);
    let (code, _stdout, stderr) = run(explain_cli(&f, &fake.base).args([
        "explain",
        "apple",
        "--in-context",
        "An apple a day keeps the doctor away.",
        "--style",
        "context",
        "--cefr",
        "b1",
        "--json",
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    let (_system, user) = messages(&fake.seen()[0].body);
    assert!(user.contains("[context] An apple a day keeps the doctor away."));
    assert!(user.contains("CEFR B1"));
    assert!(user.contains("哪一条义项、为什么"), "语境风格要换任务描述");
}

#[test]
fn human_output_streams_without_json_wrapping() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["robust", "＝强健的"])]);
    let (code, stdout, stderr) = run(explain_cli(&f, &fake.base).args(["explain", "robust"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout, "robust＝强健的\n",
        "非 --json 就是流式原文加一个换行"
    );
}

#[test]
fn a_blank_or_oversized_context_is_a_parameter_error() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&["不该发生"])]);
    let long = format!("{}end", "a ".repeat(1001));
    for context in ["   ", long.trim_end()] {
        let (code, _stdout, stderr) = run(explain_cli(&f, &fake.base)
            .arg("explain")
            .arg("robust")
            .arg("--in-context")
            .arg(context)
            .arg("--json"));
        assert_eq!(code, Some(2), "语境长度要拦");
        assert!(stderr.contains("--in-context"), "得说是哪个参数: {stderr}");
    }
    assert_eq!(fake.count(), 0, "参数错误不该发请求");
}

#[test]
fn an_unknown_style_is_rejected_by_the_parser() {
    let f = Fixture::new();
    let (code, _stdout, stderr) = run(f
        .cli()
        .env("LLM_API_KEY", "sk-fake-key-for-local-tests")
        .args(["explain", "robust", "--style", "vibes", "--json"]));
    assert_eq!(code, Some(2));
    assert!(stderr.contains("possible values"), "实际: {stderr}");
}

#[test]
fn without_a_key_the_user_is_pointed_at_the_offline_path() {
    let f = Fixture::new();
    // Fixture::cli() 已经把 LLM_* 清空了：这里不配假服务，确认它不会真的联网
    let (code, _stdout, stderr) = run(f.cli().args(["explain", "robust", "--json"]));
    assert_eq!(code, Some(2));
    assert!(stderr.contains("LLM_API_KEY"), "实际: {stderr}");
    assert!(
        stderr.contains("zedict query"),
        "要给可执行的替代: {stderr}"
    );
}

#[test]
fn an_empty_stream_is_an_error_not_an_empty_lesson() {
    let f = Fixture::new();
    let fake = FakeHttp::new(vec![sse(&[])]);
    let (code, _stdout, stderr) =
        run(explain_cli(&f, &fake.base).args(["explain", "robust", "--json"]));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("讲解内容"), "实际: {stderr}");
    assert_eq!(entry_count(&f), 0, "失败结果不得写进缓存");
}
