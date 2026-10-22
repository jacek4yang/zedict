//! Edge 逐句对齐通路的离线测试：全部走 127.0.0.1 假服务，CI 无网络也能跑。
//!
//! 关键不变量：句长必须按 **UTF-16 码元** 计，且段落分隔符归属于前一块，
//! 所以构造夹具时用 `align::units` 现算，而不是把数字抄进测试里。

mod common;

use common::cache_dir;
use common::{edge_ok as ok, edge_whole as whole, FakeHttp, FakeResponse};
use serde_json::{json, Value};
use zedict::net::align::{self, Alignment};
use zedict::net::edge::{EdgeClient, Report};

const UNITS: usize = 3600;
const DOC: &str = "I have a pen. I also have an apple.\n\nYes.\n";

/// 端点响应项。句长数组可选，便于同时覆盖自洽与不自洽两类返回。
fn item(
    target: &str,
    src: Option<Vec<usize>>,
    dst: Option<Vec<usize>>,
    detected: Option<&str>,
) -> Value {
    let mut translation = json!({ "text": target, "to": "zh-Hans" });
    if let Some(src) = src {
        translation["sentLen"] = json!({
            "srcSentLen": src,
            "transSentLen": dst.unwrap_or_default(),
        });
    }
    let mut wire = json!({ "translations": [translation] });
    if let Some(lang) = detected {
        wire["detectedLanguage"] = json!({ "language": lang, "score": 0.99 });
    }
    wire
}

fn client(base: &str, from: Option<&str>) -> EdgeClient {
    EdgeClient::new(from.map(str::to_string), "zh-Hans")
        .unwrap()
        .with_endpoint(&format!("{base}/translatetext"))
}

async fn attempt(
    client: &EdgeClient,
    text: &str,
    batch_units: usize,
    use_cache: bool,
) -> anyhow::Result<Report> {
    let mut progress = |_done: usize, _total: usize| {};
    zedict::net::edge::translate_doc(client, text, batch_units, use_cache, &mut progress).await
}

async fn run(client: &EdgeClient, text: &str) -> Report {
    attempt(client, text, UNITS, true).await.unwrap()
}

#[tokio::test]
async fn aligns_sentences_one_to_one() {
    let _cache = cache_dir();
    let blocks = align::fit_blocks(DOC, UNITS);
    assert_eq!(
        blocks,
        vec![
            "I have a pen. I also have an apple.\n\n".to_string(),
            "Yes.\n".to_string(),
        ],
        "段落分隔符必须留在前一块里，否则句长加不上"
    );
    let fake = FakeHttp::new(vec![ok(vec![
        item(
            "我有笔。我还有一个苹果。",
            Some(vec![14, 23]),
            Some(vec![4, 8]),
            Some("en"),
        ),
        item("是的。", Some(vec![5]), Some(vec![3]), None),
    ])]);
    let report = run(&client(&fake.base, Some("en")), DOC).await;

    let seen = fake.seen();
    assert_eq!(seen.len(), 1, "两块都在同一批，只发一个请求");
    assert_eq!(seen[0].method, "POST");
    assert!(seen[0].path.contains("includeSentenceLength=true"));
    assert!(seen[0].path.ends_with("&to=zh-Hans&from=en"));
    let sent: Vec<String> =
        serde_json::from_str(&seen[0].body).expect("请求体是裸 JSON 字符串数组");
    assert_eq!(sent, blocks);

    assert_eq!(report.requests, 1);
    assert_eq!(report.cache_hits, 0);
    let doc = report.doc;
    assert_eq!(doc.engine, "edge");
    assert_eq!(doc.source_lang, "en");
    assert!(!doc.detected, "显式 --from 时不宣称识别");
    assert_eq!(doc.alignment, Alignment::Verified);
    assert_eq!(doc.blocks[0].sentences.len(), 2);
    assert_eq!(doc.blocks[1].sentences.len(), 1);
    assert_eq!(
        doc.bilingual_text(60),
        "I have a pen.\n我有笔。\n\nI also have an apple.\n我还有一个苹果。\n\nYes.\n是的。\n"
    );
    assert_eq!(doc.plain(), "我有笔。我还有一个苹果。\n\n是的。");
    let json = serde_json::to_value(&doc).unwrap();
    assert_eq!(
        json["blocks"][0]["sentences"][1]["source"],
        "I also have an apple."
    );
}

#[tokio::test]
async fn detected_language_labels_an_auto_source_doc() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![ok(vec![item(
        "我有笔。",
        Some(vec![14]),
        Some(vec![4]),
        Some("en"),
    )])]);
    let report = run(&client(&fake.base, None), "I have a pen.\n").await;
    assert!(report.doc.detected);
    assert_eq!(report.doc.source_lang, "en", "源语言取服务端识别结果");
}

#[tokio::test]
async fn small_budgets_split_into_multiple_requests() {
    let _cache = cache_dir();
    let text = "A.\n\nB.\n\nC.\n";
    let blocks = align::fit_blocks(text, 8);
    assert_eq!(blocks.len(), 3);
    let fake = FakeHttp::new(vec![
        ok(vec![whole(&blocks[0], "甲。"), whole(&blocks[1], "乙。")]),
        ok(vec![whole(&blocks[2], "丙。")]),
    ]);
    let report = attempt(&client(&fake.base, Some("en")), text, 8, true)
        .await
        .unwrap();
    assert_eq!(report.requests, 2, "码元预算小的批次要分开发");
    assert_eq!(fake.count(), 2);
    assert_eq!(report.doc.blocks.len(), 3, "段落结构不因分批而丢失");
    assert_eq!(report.doc.plain(), "甲。\n\n乙。\n\n丙。");
}

#[tokio::test]
async fn http_400_halves_the_batch_in_order() {
    let _cache = cache_dir();
    let blocks = align::fit_blocks(DOC, UNITS);
    let fake = FakeHttp::new(vec![
        FakeResponse::status(400),
        ok(vec![item(
            "我有笔。我还有一个苹果。",
            Some(vec![14, 23]),
            Some(vec![4, 8]),
            None,
        )]),
        ok(vec![item("是的。", Some(vec![5]), Some(vec![3]), None)]),
    ]);
    let report = run(&client(&fake.base, Some("en")), DOC).await;
    assert_eq!(fake.count(), 3, "400 先折半，再各发一条");
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[1], serde_json::to_string(&[&blocks[0]]).unwrap());
    assert_eq!(bodies[2], serde_json::to_string(&[&blocks[1]]).unwrap());
    assert_eq!(report.doc.alignment, Alignment::Verified);
    assert_eq!(report.doc.blocks[0].sentences.len(), 2);
}

#[tokio::test]
async fn oversized_single_block_reports_an_actionable_error() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![FakeResponse::status(400), FakeResponse::status(400)]);
    let err = attempt(&client(&fake.base, Some("en")), DOC, UNITS, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--batch-units"), "实际: {err}");
    assert_eq!(fake.count(), 2, "折半到单条仍被拒就立刻停下");
}

#[tokio::test]
async fn inconsistent_lengths_degrade_to_a_single_pair() {
    let _cache = cache_dir();
    for (label, response) in [
        (
            "累加与原文不符",
            item(
                "我有笔。我还有一个苹果。是的。",
                Some(vec![99, 99]),
                Some(vec![6, 6]),
                None,
            ),
        ),
        ("缺少 sentLen", item("整块译文。", None, None, None)),
        (
            "两侧句数不等",
            item(
                "我有笔。我还有一个苹果。是的。",
                Some(vec![14, 23]),
                Some(vec![4, 8, 3]),
                None,
            ),
        ),
        (
            "译文没有句末标点",
            item("无标点译文", Some(vec![14, 23]), Some(vec![5]), None),
        ),
    ] {
        let fake = FakeHttp::new(vec![ok(vec![response.clone(); 2])]);
        let report = run(&client(&fake.base, Some("en")), DOC).await;
        assert_eq!(report.doc.alignment, Alignment::Fallback, "{label}");
        assert_eq!(
            report.doc.blocks[0].sentences.len(),
            1,
            "{label}: 降级后整块只有一对"
        );
        for block in &report.doc.blocks {
            assert_eq!(block.alignment, Alignment::Fallback, "{label}");
            assert_eq!(block.sentences.len(), 1, "{label}");
        }
        assert_eq!(
            report.doc.blocks[0].sentences[0].source, "I have a pen. I also have an apple.",
            "{label}: 降级时原文必须完整保留"
        );
        assert!(!report.doc.blocks[0].sentences[0].target.is_empty());
    }
}

/// 真实端点的 `transSentLen` 会算错：实测返回过 `[6,8,0]`（零长句）也漏算过结尾换行。
/// 只要 `srcSentLen` 自洽且两侧句数相等，就必须照常给出逐句对照，而不是整篇降级。
#[tokio::test]
async fn quirky_target_lengths_still_align_by_the_source_array() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![ok(vec![
        item(
            "我有笔。我还有一个苹果。\n\n",
            Some(vec![14, 22, 1]),
            Some(vec![6, 8, 0]),
            None,
        ),
        item("是的。\n", Some(vec![5]), Some(vec![3]), None),
    ])]);
    let report = run(&client(&fake.base, Some("en")), DOC).await;
    assert_eq!(report.doc.alignment, Alignment::Verified);
    let pairs: Vec<(&str, &str)> = report
        .doc
        .blocks
        .iter()
        .flat_map(|b| {
            b.sentences
                .iter()
                .map(|p| (p.source.as_str(), p.target.as_str()))
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("I have a pen.", "我有笔。"),
            ("I also have an apple.", "我还有一个苹果。"),
            ("Yes.", "是的。"),
        ],
        "换行这类空白不该占一个句位"
    );
}

#[tokio::test]
async fn never_splits_a_surrogate_pair() {
    let _cache = cache_dir();
    let text = "I feel 😀 great.\n";
    let block = &align::fit_blocks(text, UNITS)[0];
    assert_eq!(align::units(block), 17);
    let fake = FakeHttp::new(vec![ok(vec![item(
        "我感觉很好。真好。",
        // 8 码元落在 😀 的代理对中间：必须整字符收口，不能 panic
        Some(vec![8, 9]),
        Some(vec![6, 3]),
        None,
    )])]);
    let report = run(&client(&fake.base, Some("en")), text).await;
    assert_eq!(report.doc.alignment, Alignment::Verified);
    let pairs = &report.doc.blocks[0].sentences;
    assert_eq!(pairs[0].source, "I feel 😀");
    assert_eq!(pairs[1].source, "great.");
    assert_eq!(pairs[0].target, "我感觉很好。");
    assert_eq!(pairs[1].target, "真好。");
}

#[tokio::test]
async fn rate_limit_is_retried_once_then_surfaced() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![FakeResponse::status(429), FakeResponse::status(429)]);
    let err = attempt(&client(&fake.base, Some("en")), DOC, UNITS, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("限流"), "实际: {err}");
    assert_eq!(fake.count(), 2, "退避一次就不再纠缠");
}

#[tokio::test]
async fn denied_access_points_at_the_llm_engine() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![FakeResponse::status(403)]);
    let err = attempt(&client(&fake.base, Some("en")), DOC, UNITS, false)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("--engine llm"), "实际: {text}");
    assert!(text.contains("非公开"), "要说明端点性质: {text}");
    assert_eq!(fake.count(), 1, "403 不重试");
}

#[tokio::test]
async fn malformed_or_short_response_array_is_not_guessed() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![
        FakeResponse::json(r#"{"oops":true}"#),
        FakeResponse::json(r#"[{"translations":[{"text":"一。"}]}]"#),
    ]);
    let client = client(&fake.base, Some("en"));
    let err = attempt(&client, DOC, UNITS, false).await.unwrap_err();
    assert!(err.to_string().contains("格式改变"), "实际: {err}");
    let err = attempt(&client, DOC, UNITS, false).await.unwrap_err();
    assert!(err.to_string().contains("拒绝猜测"), "实际: {err}");
    assert_eq!(fake.count(), 2);
}

#[tokio::test]
async fn cached_blocks_cost_zero_requests() {
    let cache = cache_dir();
    let blocks = align::fit_blocks(DOC, UNITS);
    let fake = FakeHttp::new(vec![
        ok(vec![
            item(
                "我有笔。我还有一个苹果。",
                Some(vec![14, 23]),
                Some(vec![4, 8]),
                Some("en"),
            ),
            item("是的。", Some(vec![5]), Some(vec![3]), None),
        ]),
        ok(vec![
            whole(&blocks[0], "J'ai un stylo."),
            whole(&blocks[1], "Oui."),
        ]),
    ]);
    let client = client(&fake.base, None);
    let first = run(&client, DOC).await;
    assert_eq!(
        first.doc.source_lang, "en",
        "未指定 --from 时用服务端识别结果"
    );
    assert!(first.doc.detected);

    let dir = cache.path().join("edge-translate");
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 2, "每块一个缓存文件");
    for name in &files {
        assert!(name.starts_with("edge-translate-"), "实际: {name}");
        assert!(
            name.ends_with(".json") && name.len() == "edge-translate-".len() + 64 + 5,
            "文件名只该是命名空间加哈希，不出现原文: {name}"
        );
    }

    let second = run(&client, DOC).await;
    assert_eq!(second.requests, 0);
    assert_eq!(second.cache_hits, 2);
    assert_eq!(fake.count(), 1, "命中缓存后一个请求都不发");
    assert_eq!(second.doc.bilingual_text(60), first.doc.bilingual_text(60));
    assert_eq!(
        second.doc.alignment,
        Alignment::Verified,
        "重读缓存也要能重新对齐"
    );
    assert_eq!(second.doc.source_lang, "en", "语种识别结果随缓存保留");
    assert!(second.doc.detected);

    let french = EdgeClient::new(None, "fr")
        .unwrap()
        .with_endpoint(&fake.url("/translatetext"));
    let third = run(&french, DOC).await;
    assert_eq!(third.requests, 1);
    assert_eq!(third.cache_hits, 0, "换目标语言不能复用旧译文");
    assert_eq!(third.doc.target_lang, "fr");
}

#[tokio::test]
async fn a_different_endpoint_does_not_read_my_cache() {
    let _cache = cache_dir();
    let response = || ok(vec![whole("I have a pen.\n", "我有笔。")]);
    let fake = FakeHttp::new(vec![response(), response()]);
    let mine = client(&fake.base, Some("en"));
    assert_eq!(run(&mine, "I have a pen.\n").await.requests, 1);
    let proxy = EdgeClient::new(Some("en".into()), "zh-Hans")
        .unwrap()
        .with_endpoint(&fake.url("/proxy"));
    assert_eq!(run(&proxy, "I have a pen.\n").await.requests, 1);
    assert_eq!(fake.count(), 2, "端点不同就不算命中");
}

#[tokio::test]
async fn empty_translation_is_never_cached() {
    let cache = cache_dir();
    let fake = FakeHttp::new(vec![
        ok(vec![
            item("   ", Some(vec![37, 5]), Some(vec![1, 1]), None);
            2
        ]),
        ok(vec![
            item("我有笔。", Some(vec![37]), Some(vec![4]), None),
            item("是的。", Some(vec![5]), Some(vec![3]), None),
        ]),
    ]);
    let client = client(&fake.base, Some("en"));
    let err = attempt(&client, DOC, UNITS, true).await.unwrap_err();
    assert!(err.to_string().contains("空译文"), "实际: {err}");
    let leftovers = std::fs::read_dir(cache.path().join("edge-translate"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "失败结果不得落盘");
    assert_eq!(run(&client, DOC).await.requests, 1, "下一次仍会重新请求");
}

#[tokio::test]
async fn empty_input_is_a_parameter_error_not_a_request() {
    let _cache = cache_dir();
    let fake = FakeHttp::new(vec![]);
    let err = attempt(&client(&fake.base, Some("en")), "", UNITS, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("没有可翻译的文本"));
    assert_eq!(fake.count(), 0);
}

/// 真实端点：非公开接口，随时可能变更，所以默认跳过，手工验收时才跑
#[tokio::test]
#[ignore = "需要访问 edge.microsoft.com"]
async fn real_endpoint_aligns_sentences() {
    let _cache = cache_dir();
    let client = EdgeClient::new(Some("en".into()), "zh-Hans").unwrap();
    let report = run(&client, DOC).await;
    assert_eq!(report.doc.alignment, Alignment::Verified);
    let pairs: Vec<(&str, &str)> = report
        .doc
        .blocks
        .iter()
        .flat_map(|b| {
            b.sentences
                .iter()
                .map(|p| (p.source.as_str(), p.target.as_str()))
        })
        .collect();
    assert_eq!(pairs.len(), 3, "两句一段落 + 一句，实际: {pairs:?}");
    assert_eq!(pairs[0].0, "I have a pen.");
    assert_eq!(pairs[1].0, "I also have an apple.");
    assert_eq!(pairs[2].0, "Yes.");
    for (_, target) in &pairs {
        assert!(
            target.chars().next().is_some_and(|c| !c.is_whitespace()),
            "每对译文都该以内容开头，实际: {target:?}"
        );
    }
    print!("{}", report.doc.bilingual_text(60));
}
