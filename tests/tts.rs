//! `speak` 兜底后端的离线测试：假服务返回音频或网页，验证落盘、报错与缓存复用。
//!
//! 有道端点免密钥也没有配额，所以每条断言都要能回答两件事：失败了会不会留下半个文件，
//! 以及同一个词会不会被反复请求。

mod common;

use common::{cache_dir, FakeHttp, FakeResponse, Fixture};
use serde_json::{json, Value};
use zedict::net::tts::{self, Accent, SpeechSource};

/// 带 ID3v2 标签的假 mp3：头部够嗅探，长度够断言字节数
fn mp3() -> Vec<u8> {
    let mut body = b"ID3\x03\x00\x00\x00\x00\x00\x21".to_vec();
    body.extend_from_slice(&[0xFFu8, 0xFB, 0x90, 0x44]);
    body.resize(512, 0);
    body
}

fn html() -> Vec<u8> {
    b"<html><body>quota exceeded</body></html>".to_vec()
}

fn audio(status: u16, content_type: &'static str, body: Vec<u8>) -> FakeResponse {
    FakeResponse::body(status, content_type, body)
}

fn youdao_cli(f: &Fixture, base: &str) -> std::process::Command {
    let mut cmd = f.cli();
    cmd.env("ZEDICT_YOUDAO_URL", format!("{base}/dictvoice"));
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

/// 端点与缓存目录都由进程级环境变量指定，所以共用 cache_dir() 的全局锁
struct FakeSpeech {
    fake: FakeHttp,
    cache: common::CacheDir,
}

impl FakeSpeech {
    fn new(responses: Vec<FakeResponse>) -> Self {
        let cache = cache_dir();
        let fake = FakeHttp::new(responses);
        std::env::set_var("ZEDICT_YOUDAO_URL", fake.url("/dictvoice"));
        Self { fake, cache }
    }

    /// 缓存里的音频条目；失败路径必须留不下东西
    fn audio_files(&self) -> Vec<String> {
        std::fs::read_dir(self.cache.path().join("audio"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for FakeSpeech {
    fn drop(&mut self) {
        std::env::remove_var("ZEDICT_YOUDAO_URL");
    }
}

fn us() -> SpeechSource {
    SpeechSource::Youdao { accent: Accent::Us }
}

#[test]
fn youdao_saves_audio_and_reports_the_backend() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("us.mp3");
    let fake = FakeHttp::new(vec![audio(200, "audio/mpeg", mp3())]);
    let (code, stdout, stderr) = run(youdao_cli(&f, &fake.base)
        .arg("speak")
        .arg("apple")
        .args(["--engine", "youdao"])
        .arg("--out")
        .arg(&out)
        .arg("--json"));
    assert_eq!(code, Some(0), "{stderr}");

    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "GET", "这个端点只认 GET");
    assert!(
        seen[0].path.starts_with("/dictvoice?audio=apple&type=1"),
        "实际: {}",
        seen[0].path
    );
    assert!(seen[0].body.is_empty(), "GET 不该带请求体");

    assert_eq!(std::fs::read(&out).unwrap(), mp3(), "字节要原样落盘");
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["bytes"], json!(512));
    assert_eq!(value["out"], json!(out.to_string_lossy()));
    assert_eq!(value["source"], json!({"youdao": {"accent": "us"}}));
}

#[test]
fn accent_picks_the_type_and_spaces_survive_the_query_string() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeHttp::new(vec![audio(200, "audio/mpeg", mp3()); 2]);
    for (index, (accent, want_type)) in [("uk", "type=2"), ("us", "type=1")].into_iter().enumerate()
    {
        let (code, stdout, stderr) = run(youdao_cli(&f, &fake.base)
            .arg("speak")
            .arg("take into account")
            .args(["--engine", "youdao", "--accent", accent])
            .arg("--out")
            .arg(dir.path().join(format!("{accent}.mp3"))));
        assert_eq!(code, Some(0), "{stderr}");
        assert!(stdout.contains("音频已保存"), "{stdout}");
        assert!(
            fake.seen()[index].path.ends_with(want_type),
            "实际: {}",
            fake.seen()[index].path
        );
    }
    let paths: Vec<String> = fake.seen().into_iter().map(|s| s.path).collect();
    assert_eq!(
        paths,
        vec![
            "/dictvoice?audio=take%20into%20account&type=2",
            "/dictvoice?audio=take%20into%20account&type=1",
        ],
        "空格必须百分号编码，否则查询串会断"
    );
}

#[test]
fn a_bad_response_is_refused_and_leaves_no_file() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    for (label, response) in [
        ("page", audio(200, "text/html", html())),
        ("empty", audio(200, "audio/mpeg", Vec::new())),
        ("missing", audio(404, "text/html", html())),
    ] {
        let out = dir.path().join(format!("{label}.mp3"));
        let fake = FakeHttp::new(vec![response]);
        let (code, _stdout, stderr) = run(youdao_cli(&f, &fake.base)
            .arg("speak")
            .arg("apple")
            .args(["--engine", "youdao"])
            .arg("--out")
            .arg(&out));
        assert_eq!(code, Some(1), "{label} 应当是运行失败");
        assert!(!out.exists(), "{label}: 失败了就不该留下文件");
        assert!(stderr.contains("--engine edge"), "{label}: {stderr}");
        assert_eq!(fake.count(), 1, "{label}: 不重试、不纠缠");
    }
}

#[test]
fn crossing_engine_flags_is_a_parameter_error() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("x.mp3");
    for (args, expect) in [
        (["--engine", "youdao", "--voice", "x"], "--voice"),
        (["--engine", "youdao", "--rate", "x"], "--rate"),
        (["--engine", "edge", "--accent", "uk"], "--accent"),
    ] {
        let fake = FakeHttp::new(vec![audio(200, "audio/mpeg", mp3())]);
        let mut cmd = youdao_cli(&f, &fake.base);
        cmd.arg("speak").arg("apple").arg("--out").arg(&out);
        cmd.args(args);
        let (code, _stdout, stderr) = run(&mut cmd);
        assert_eq!(code, Some(2), "{expect} 是跨引擎串用");
        assert!(stderr.contains(expect), "实际: {stderr}");
        assert_eq!(fake.count(), 0, "{expect}: 参数错误不该发请求");
        assert!(!out.exists());
    }
}

/// 把假 HTTP 服务伪装成 Edge 的 WebSocket 端点；它的 403 会让握手直接失败
fn edge_url(fake: &FakeHttp) -> String {
    format!("{}/edge", fake.base.replacen("http://", "ws://", 1))
}

#[test]
fn auto_falls_back_to_youdao_when_edge_is_rejected() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("fallback.mp3");
    let fake = FakeHttp::new(vec![
        FakeResponse::status(403),
        audio(200, "audio/mpeg", mp3()),
    ]);
    let (code, stdout, stderr) = run(youdao_cli(&f, &fake.base)
        .env("ZEDICT_EDGE_TTS_URL", edge_url(&fake))
        .arg("speak")
        .arg("apple")
        .arg("--out")
        .arg(&out)
        .arg("--json"));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("兜底"), "{stderr}");
    assert_eq!(std::fs::read(&out).unwrap(), mp3());

    let seen = fake.seen();
    assert_eq!(seen.len(), 2, "先试 Edge，失败后才找有道");
    assert!(seen[0].path.starts_with("/edge?"), "实际: {}", seen[0].path);
    assert!(
        seen[0].path.contains("Sec-MS-GEC="),
        "实际: {}",
        seen[0].path
    );
    assert!(
        seen[1].path.starts_with("/dictvoice?audio=apple&type=1"),
        "实际: {}",
        seen[1].path
    );
    let value: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(value["source"], json!({"youdao": {"accent": "us"}}));
    assert_eq!(value["bytes"], json!(512));
}

#[test]
fn auto_keeps_long_text_off_youdao() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("long.mp3");
    let fake = FakeHttp::new(vec![FakeResponse::status(403)]);
    let long = "word ".repeat(250);
    let (code, _stdout, stderr) = run(youdao_cli(&f, &fake.base)
        .env("ZEDICT_EDGE_TTS_URL", edge_url(&fake))
        .arg("speak")
        .arg(long.trim_end())
        .arg("--out")
        .arg(&out));
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("1000") && stderr.contains("兜底"),
        "{stderr}"
    );
    assert!(!out.exists());
    assert_eq!(fake.count(), 1, "超长文本不该发给有道");
}

#[test]
fn explicit_edge_does_not_fall_back() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("edge.mp3");
    let fake = FakeHttp::new(vec![FakeResponse::status(403)]);
    let (code, _stdout, stderr) = run(youdao_cli(&f, &fake.base)
        .env("ZEDICT_EDGE_TTS_URL", edge_url(&fake))
        .args(["speak", "apple", "--engine", "edge"])
        .arg("--out")
        .arg(&out));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(!stderr.contains("兜底"), "强制 Edge 时不该改道: {stderr}");
    assert_eq!(fake.count(), 1);
    assert!(!out.exists());
}

#[test]
fn an_over_long_phrase_is_rejected_before_the_request() {
    let f = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let long = "word ".repeat(300);
    let fake = FakeHttp::new(vec![audio(200, "audio/mpeg", mp3())]);
    let (code, _stdout, stderr) = run(youdao_cli(&f, &fake.base)
        .arg("speak")
        .arg(long.trim_end())
        .args(["--engine", "youdao"])
        .arg("--out")
        .arg(dir.path().join("x.mp3")));
    assert_eq!(code, Some(2));
    assert!(stderr.contains("1000"), "要说清上限: {stderr}");
    assert_eq!(fake.count(), 0);
}

#[tokio::test]
async fn a_replayed_word_is_downloaded_once() {
    let speech = FakeSpeech::new(vec![audio(200, "audio/mpeg", mp3()); 2]);
    let first = tts::cached_audio("apple", &us()).await.unwrap();
    let second = tts::cached_audio("apple", &us()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(speech.fake.count(), 1, "同一个词不该再要一遍");
    assert_eq!(std::fs::metadata(&first).unwrap().len(), 512);

    let name = first.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name.len(), 64 + ".mp3".len(), "实际: {name}");
    assert!(
        name[..64].chars().all(|c| c.is_ascii_hexdigit()),
        "缓存名只能是哈希，不许带原文: {name}"
    );

    // 换口音就是换键：不能把美音当英音放出来
    let uk = tts::cached_audio("apple", &SpeechSource::Youdao { accent: Accent::Uk })
        .await
        .unwrap();
    assert_ne!(first, uk);
    assert_eq!(speech.audio_files().len(), 2);
    assert_eq!(speech.fake.count(), 2);
}

#[test]
fn each_backend_has_its_own_cache_key_and_edge_keeps_the_old_shape() {
    let edge = SpeechSource::Edge {
        voice: "en-US-AriaNeural".into(),
        rate: "+0%".into(),
    };
    assert_ne!(
        tts::cache_key("apple", &edge).unwrap(),
        tts::cache_key("apple", &us()).unwrap(),
        "两个后端的音频不该互相顶替"
    );
    // 历史键形状是 (text, voice, rate)；改了就不认识用户已经攒下的缓存
    let legacy = zedict::data::ingest::hash_bytes(
        &serde_json::to_vec(&("apple", "en-US-AriaNeural", "+0%")).unwrap(),
    );
    assert_eq!(tts::cache_key("apple", &edge).unwrap(), legacy);
    assert_eq!(
        tts::cache_key("apple", &edge).unwrap(),
        tts::cache_key("apple", &edge).unwrap()
    );
}

#[tokio::test]
async fn a_failed_download_does_not_poison_the_cache() {
    let speech = FakeSpeech::new(vec![audio(200, "text/html", html())]);
    let err = tts::cached_audio("apple", &us()).await.unwrap_err();
    assert!(err.to_string().contains("MP3"), "实际: {err}");
    assert!(speech.audio_files().is_empty(), "失败结果不得留在缓存里");
}

#[tokio::test]
async fn a_custom_endpoint_must_be_a_url() {
    let _cache = cache_dir();
    std::env::set_var("ZEDICT_YOUDAO_URL", "dict.youdao.com/dictvoice");
    let err = tts::youdao_url("apple", Accent::Us).unwrap_err();
    std::env::remove_var("ZEDICT_YOUDAO_URL");
    assert!(err.to_string().contains("http"), "实际: {err}");
}

#[tokio::test]
#[ignore = "需要访问 dict.youdao.com"]
async fn real_endpoint_returns_mp3_for_both_accents() {
    let dir = tempfile::tempdir().unwrap();
    for (accent, label) in [(Accent::Us, "us"), (Accent::Uk, "uk")] {
        let out = dir.path().join(format!("{label}.mp3"));
        let bytes = tts::synthesize("apple", &SpeechSource::Youdao { accent }, &out)
            .await
            .unwrap();
        assert!(bytes > 1024, "{label} 只有 {bytes} 字节");
        assert!(tts::looks_like_mp3(&std::fs::read(&out).unwrap()));
        println!("{label}: {bytes} bytes");
    }
}
