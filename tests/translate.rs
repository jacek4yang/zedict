//! `zedict translate` 的命令行面：引擎选择、--file、退出码、缓存与 --out。
//!
//! 子进程一律指向 127.0.0.1 假服务；密钥只在这些测试里出现，绝不外发。

mod common;

use common::{edge_blocks, edge_ok, edge_whole, FakeHttp, FakeResponse};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use zedict::net::align;

const TWO_BLOCKS: &str = "I have a pen.\n\nYes.\n";
const BILINGUAL: &str = "I have a pen.\n我有笔。\n\nYes.\n是的。\n";
const PLAIN: &str = "我有笔。\n\n是的。\n";
const SENTENCES: &str = "I have a pen. I also have an apple.";

/// translate 不碰词库，所以这里只准备配置与缓存目录，不必构建 sqlite
struct Env {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "db = 'unused.sqlite'\n").unwrap();
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

    fn cli(&self, endpoint: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_zedict"));
        cmd.env_remove("ZEDICT_DB")
            .env_remove("ZEDICT_CONFIG")
            .env_remove("NO_COLOR")
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_BASE_URL")
            .env_remove("LLM_MODEL")
            .env("ZEDICT_CACHE_DIR", self.path("cache"))
            .env("ZEDICT_EDGE_TRANSLATE_URL", endpoint)
            .current_dir(std::env::temp_dir());
        cmd.arg("--config").arg(&self.config);
        cmd
    }
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap_or_else(|err| panic!("stdout 非 UTF-8: {err}"))
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// 一次批量请求覆盖两个块：每块整块返回一句
fn edge_two_blocks() -> FakeResponse {
    let blocks = edge_blocks(TWO_BLOCKS);
    assert_eq!(blocks.len(), 2);
    edge_ok(vec![
        edge_whole(&blocks[0], "我有笔。"),
        edge_whole(&blocks[1], "是的。"),
    ])
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn with_path(prefix: &[&str], path: &Path) -> Vec<String> {
    let mut out = args(prefix);
    out.push(path.to_string_lossy().into_owned());
    out
}

#[test]
fn auto_engine_uses_the_key_free_edge_path() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let file = env.write("doc.txt", TWO_BLOCKS.as_bytes());
    let fake = FakeHttp::new(vec![edge_two_blocks()]);
    let output = env
        .cli(&fake.url("/translatetext"))
        .args(["translate", "--file"])
        .arg(&file)
        .arg("--bilingual")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), BILINGUAL);
    assert_eq!(stderr_of(&output), "", "stdout 被重定向时不该打印进度");
    assert_eq!(fake.count(), 1);
    let seen = &fake.seen()[0];
    assert_eq!(seen.method, "POST");
    assert!(seen.path.contains("includeSentenceLength=true"));
    assert!(seen.path.contains("to=zh-Hans"));
    assert!(!seen.path.contains("&from="), "默认交给服务端识别");
    let sent: Vec<String> = serde_json::from_str(&seen.body).expect("请求体是裸 JSON 字符串数组");
    assert_eq!(sent, edge_blocks(TWO_BLOCKS));
}

#[test]
fn json_keeps_text_and_adds_alignment_metadata() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let fake = FakeHttp::new(vec![edge_two_blocks()]);
    let output = env
        .cli(&fake.url("/translatetext"))
        .args(["translate", TWO_BLOCKS, "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["engine"], "edge");
    assert_eq!(value["text"], "我有笔。\n\n是的。", "旧的 text 键必须保留");
    assert_eq!(value["target_lang"], "zh-Hans");
    assert_eq!(value["alignment"], "verified");
    assert_eq!(value["detected"], true);
    assert_eq!(value["requests"], 1);
    assert_eq!(value["cache_hits"], 0);
    assert_eq!(
        value["blocks"][0]["sentences"][0]["source"],
        "I have a pen."
    );
}

#[test]
fn cached_reruns_issue_no_requests() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let file = env.write("doc.txt", TWO_BLOCKS.as_bytes());
    let fake = FakeHttp::new(vec![edge_two_blocks(), edge_two_blocks()]);
    let endpoint = fake.url("/translatetext");
    let run = |extra: &[&str]| -> serde_json::Value {
        let output = env
            .cli(&endpoint)
            .args(["translate", "--file"])
            .arg(&file)
            .arg("--json")
            .args(extra)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr_of(&output));
        serde_json::from_slice(&output.stdout).unwrap()
    };

    let first = run(&[]);
    assert_eq!(first["requests"], 1);
    assert_eq!(first["cache_hits"], 0);
    assert_eq!(fake.count(), 1);

    let second = run(&[]);
    assert_eq!(second["requests"], 0, "重跑不该再请求");
    assert_eq!(second["cache_hits"], 2);
    assert_eq!(second["alignment"], "verified", "缓存也要能重新逐句对齐");
    assert_eq!(second["text"], first["text"]);
    assert_eq!(fake.count(), 1, "命中缓存后一个请求都不发");

    let forced = run(&["--no-cache"]);
    assert_eq!(forced["requests"], 1);
    assert_eq!(forced["cache_hits"], 0);
    assert_eq!(fake.count(), 2, "--no-cache 强制重新请求");
    let files = std::fs::read_dir(env.path("cache").join("edge-translate"))
        .unwrap()
        .count();
    assert_eq!(files, 2, "每块一个缓存文件");
}

#[test]
fn out_file_receives_exactly_what_stdout_shows() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let fake = FakeHttp::new(vec![edge_two_blocks()]);
    let out = env.path("result.txt");
    let output = env
        .cli(&fake.url("/translatetext"))
        .args(["translate", TWO_BLOCKS, "--bilingual", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), BILINGUAL);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), BILINGUAL);
}

#[test]
fn file_input_strips_bom_and_normalizes_crlf() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice("I have a pen.\r\n\r\nYes.\r\n".as_bytes());
    let file = env.write("crlf.txt", &bytes);
    let fake = FakeHttp::new(vec![edge_two_blocks()]);
    let output = env
        .cli(&fake.url("/translatetext"))
        .args(["translate", "--file"])
        .arg(&file)
        .args(["--bilingual", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["blocks"][0]["sentences"][0]["source"],
        "I have a pen."
    );
    let sent: Vec<String> = serde_json::from_str(&fake.bodies()[0]).unwrap();
    assert_eq!(sent, edge_blocks(TWO_BLOCKS), "BOM 与 CR 不该进入请求体");
    assert!(!stdout_of(&output).contains('\r'), "逐句对照不该带出 \\r");
}

#[test]
fn a_configured_key_makes_auto_choose_the_llm() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let llm = FakeHttp::new(vec![FakeResponse::sse(&[
        r#"["我有笔。","我还有一个苹果。"]"#,
    ])]);
    let edge = FakeHttp::new(vec![]);
    let output = env
        .cli(&edge.url("/translatetext"))
        .env("LLM_API_KEY", "test-only")
        .env("LLM_BASE_URL", llm.url("/v1"))
        .args(["translate", SENTENCES, "--bilingual", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["engine"], "llm");
    assert_eq!(value["alignment"], "verified");
    assert_eq!(
        value["blocks"][0]["sentences"][1]["target"],
        "我还有一个苹果。"
    );
    assert_eq!(value["text"], "我有笔。我还有一个苹果。");
    assert_eq!(
        value["blocks"][0]["sentences"][0]["source"], "I have a pen.",
        "逐句对照的句子来自原文切分，不是模型复述"
    );
    assert_eq!(edge.count(), 0, "有密钥时不该去碰 Edge");
    assert_eq!(llm.count(), 1);
    assert!(llm.seen()[0].path.ends_with("/v1/chat/completions"));
    let body = &llm.seen()[0].body;
    assert!(
        body.contains("长度正好为 2"),
        "要逐句就必须钉死数组长度: {body}"
    );
    assert!(body.contains("\"temperature\":0.3"));
    assert!(!body.contains("test-only"), "密钥只进请求头");
}

#[test]
fn llm_without_a_key_explains_the_free_path() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let fake = FakeHttp::new(vec![]);
    let output = env
        .cli(&fake.url("/translatetext"))
        .args(["translate", "hello", "--engine", "llm"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("参数错误"), "实际: {stderr}");
    assert!(
        stderr.contains("--engine edge"),
        "要给出免密钥出路: {stderr}"
    );
    assert_eq!(fake.count(), 0);
}

#[test]
fn edge_engine_says_which_llm_flags_it_ignores() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let glossary = env.write("terms.tsv", "pen\t钢笔\n");
    let fake = FakeHttp::new(vec![edge_two_blocks()]);
    let output = env
        .cli(&fake.url("/translatetext"))
        .args([
            "translate",
            TWO_BLOCKS,
            "--engine",
            "edge",
            "--context",
            "购物",
            "--chunk",
            "500",
            "--translation-mode",
            "literal",
        ])
        .arg("--glossary")
        .arg(&glossary)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(stdout_of(&output), PLAIN);
    let stderr = stderr_of(&output);
    assert!(stderr.contains("--engine edge 不使用"), "实际: {stderr}");
    for flag in ["--glossary", "--context", "--chunk", "--translation-mode"] {
        assert!(stderr.contains(flag), "要点名 {flag}: {stderr}");
    }
}

#[test]
fn parameter_mistakes_exit_two_and_read_failures_exit_one() {
    let _cache = common::cache_dir();
    let env = Env::new();
    let missing = env.path("nope.txt");
    let big = env.write("big.txt", vec![b'a'; 4 * 1024 * 1024 + 1]);
    let bad_terms = env.write("bad.tsv", "pen 钢笔\n");
    let fake = FakeHttp::new(vec![FakeResponse::status(403)]);
    let cases: Vec<(Vec<String>, i32, &str)> = vec![
        (args(&["translate", "   "]), 2, "翻译文本为空"),
        (args(&["translate"]), 2, "required"),
        (args(&["translate", "hi", "--file"]), 2, "required"),
        (
            with_path(&["translate", "hi", "--file"], &missing),
            2,
            "cannot be used with",
        ),
        (
            args(&["translate", "hi", "--batch-units", "10"]),
            2,
            "--batch-units",
        ),
        (
            args(&["translate", "hi", "--to", "zh-Hans;echo"]),
            2,
            "非法语言代码",
        ),
        (
            with_path(&["translate", "--file"], &missing),
            1,
            "读取翻译文件",
        ),
        (with_path(&["translate", "--file"], &big), 2, "拆分"),
        (
            with_path(&["translate", "hi", "--glossary"], &bad_terms),
            2,
            "缺少制表符",
        ),
        (args(&["translate", "hi", "--engine", "edge"]), 1, "非公开"),
    ];
    for (index, (argv, want_code, needle)) in cases.into_iter().enumerate() {
        let mut cmd = env.cli(&fake.url("/translatetext"));
        for arg in &argv {
            cmd.arg(arg);
        }
        let output = cmd.output().unwrap();
        let stderr = stderr_of(&output);
        let label = format!("用例 #{index} {argv:?}");
        assert_eq!(output.status.code(), Some(want_code), "{label}: {stderr}");
        assert!(
            stderr.contains(needle),
            "{label}: 期望 stderr 含 {needle:?}，实际 {stderr}"
        );
        let prefix = if want_code == 2 {
            stderr.contains("参数错误") || stderr.contains("error")
        } else {
            stderr.contains("错误")
        };
        assert!(prefix, "{label}: 要说明原因: {stderr}");
    }
    assert_eq!(fake.count(), 1, "只有最后一个用例真的发了请求");
}

#[test]
fn closed_stdout_is_never_a_panic() {
    let _cache = common::cache_dir();
    let env = Env::new();
    // 输出超过管道缓冲区，子进程必然在被关闭的管道上写失败
    let long = "I have a pen.\n\n".repeat(3_000);
    let blocks = edge_blocks(&long);
    let batches = align::plan_batches(&blocks, 3600, align::MAX_BATCH_ITEMS);
    // 文本相同的块共用一个缓存条目，所以只有第一批会真的请求；响应仍按批备足
    let responses: Vec<FakeResponse> = batches
        .iter()
        .map(|batch| {
            edge_ok(
                batch
                    .iter()
                    .map(|&i| edge_whole(&blocks[i], "我有笔。"))
                    .collect(),
            )
        })
        .collect();
    let fake = FakeHttp::new(responses);
    let file = env.write("long.txt", long.as_bytes());
    let mut child = env
        .cli(&fake.url("/translatetext"))
        .args(["translate", "--file"])
        .arg(&file)
        .arg("--bilingual")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdout.take().unwrap();
    let mut one = [0u8; 1];
    let _ = pipe.read_exact(&mut one);
    drop(pipe);
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "管道被关闭不能算失败: {}",
        stderr_of(&output)
    );
}
