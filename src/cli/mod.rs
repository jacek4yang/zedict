use crate::{
    app::config::Settings,
    core::model::AffixHint,
    data::store::{DictStore, QueryRequest, QueryResponse, SearchMode},
};
use anyhow::{ensure, Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

mod query;
mod support;

use query::*;
use support::*;

#[derive(Parser)]
#[command(name = "zedict", version, about = "离线英汉词典：查询本地结构化词库")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, conflicts_with = "full")]
    brief: bool,
    #[arg(long, global = true, conflicts_with = "brief")]
    full: bool,
    /// 终端配色：auto 只在支持 ANSI 的交互终端上着色，ascii 始终纯排版，rich 强制彩色
    #[arg(long, global = true, value_enum)]
    theme: Option<crate::render::ThemeChoice>,
    #[arg(long, global = true, value_enum, default_value = "auto")]
    mode: SearchMode,
    #[arg(long, global=true, default_value_t=8, value_parser=limit_parser)]
    limit: usize,
}

fn limit_parser(s: &str) -> std::result::Result<usize, String> {
    let n = s.parse::<usize>().map_err(|_| "需要整数".to_string())?;
    if (1..=1000).contains(&n) {
        Ok(n)
    } else {
        Err("须为 1–1000".into())
    }
}

#[derive(Subcommand)]
enum Command {
    /// 查询英文单词/短语或中文释义
    Query {
        #[arg(required_unless_present = "stdin", conflicts_with = "stdin")]
        word: Option<String>,
        /// 每行一条查询；--json 输出 JSONL
        #[arg(long)]
        stdin: bool,
        /// 强制给英英释义和例句补中文翻译；交互终端默认自动开启（--stdin 批量除外），需联网
        #[arg(long, conflicts_with = "no_en_zh")]
        en_zh: bool,
        /// 关闭英英释义和例句的自动翻译
        #[arg(long)]
        no_en_zh: bool,
    },
    /// 生成待核对的资料清单；不会把猜测的词名直接当成正式词条
    Audit {
        #[arg(long, default_value = "data/dict_full.jsonl")]
        input: PathBuf,
        #[arg(long, default_value = "review-work")]
        out: PathBuf,
    },
    /// 全量导入结构化词库；可选用修正清单
    Compile {
        #[arg(long, default_value = "data/dict_full.jsonl")]
        input: PathBuf,
        #[arg(long)]
        reviews: Option<PathBuf>,
        /// 在全量导入基础上应用已有修正
        #[arg(long, conflicts_with = "reviews")]
        corrections: Option<PathBuf>,
        /// 词形变化表（tools/inflect_aliases.py 生成），让 ran/running 查到 run
        #[arg(long)]
        inflections: Option<PathBuf>,
        /// 中文释义表（词<TAB>中文），给近义/反义词补上中文
        #[arg(long)]
        related: Option<PathBuf>,
        /// 词级表（tools/word_levels.py 生成），给 zedict hint 提供难度档
        #[arg(long)]
        levels: Option<PathBuf>,
        /// 派生例句索引，给 zedict examples 用；句子本来就在词卡里，不加输入文件
        #[arg(long)]
        examples: bool,
        /// 派生删字符索引，给 query --mode fuzzy 补位用；单词本来就在库里
        #[arg(long)]
        spell: bool,
        /// 派生拼音索引，给 query --mode pinyin 用；中文词卡的拼音本来就在库里
        #[arg(long)]
        pinyin: bool,
        /// 收录依据阈值。all=全部保留；some=丢掉在本条记录里找不到任何依据的词名；
        /// strong=只保留与例句高亮词一致的。人工核对过的记录不受阈值影响
        #[arg(long, value_enum, default_value = "all")]
        key_evidence: crate::core::evidence::KeyEvidence,
        /// 参考词表（一行一个英文词）：查询未命中时据此说明"这词很常见，只是词库没收"
        #[arg(long)]
        vocab: Option<PathBuf>,
        #[arg(long, default_value = "store")]
        out: PathBuf,
        /// 校验后安装到当前配置的数据路径
        #[arg(long)]
        install: bool,
    },
    /// 校验本地词库并显示路径、版本与覆盖统计；不联网
    Doctor,
    /// 生成 shell 补全
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// 浏览词条
    Words {
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long)]
        random: bool,
    },
    /// 检索例句库：先按关键词精确命中，一个没有再全文扫描
    Examples {
        /// 词、短语或中文；按原样匹配，不做繁简折叠
        text: String,
    },
    /// 长文/整篇翻译：Edge 免密钥逐句对齐，或已配置的大模型
    Translate(TranslateArgs),
    /// 基于本地词库释义的大模型讲解；词库没有的义项不会编造
    Explain(ExplainArgs),
    /// 超纲词统计：本地词级表比对，不联网
    Hint(HintArgs),
    /// 导出 Anki 可直接导入的 CSV/TSV 卡片表
    Export(ExportArgs),
    /// 本地生词本、复习计划与 Anki .apkg 导出；不写词典库
    Study(StudyArgs),
    /// 显式请求在线发音；--out 保存 MP3
    Speak {
        text: String,
        /// 发音后端：auto（默认）= Edge，HTTP 失败且有道能兜住时自动改用有道；
        /// edge/youdao 为强制指定，失败不再改道
        #[arg(long, value_enum, default_value = "auto")]
        engine: SpeakEngine,
        /// Edge 语音名（--engine auto|edge；默认 en-US-AriaNeural）
        #[arg(long)]
        voice: Option<String>,
        /// Edge 语速（--engine auto|edge；默认 +0%）
        #[arg(long, allow_hyphen_values = true)]
        rate: Option<String>,
        /// 有道口音（--engine youdao，或 auto 兜底时；默认 us）
        #[arg(long, value_enum)]
        accent: Option<crate::net::tts::Accent>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    #[cfg(feature = "server")]
    /// 启动可选的本地兼容 API
    Serve {
        #[arg(long, default_value = "127.0.0.1:8686")]
        addr: std::net::SocketAddr,
        #[arg(long)]
        llm: bool,
    },
}

#[derive(Args)]
struct TranslateArgs {
    #[arg(required_unless_present = "file", conflicts_with = "file")]
    text: Option<String>,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(
        long = "translation-mode",
        alias = "style",
        value_enum,
        default_value = "natural"
    )]
    style: crate::net::llm::TranslateMode,
    #[arg(long)]
    context: Option<String>,
    #[arg(long)]
    summary: Option<String>,
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long, hide = true)]
    api_key: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, default_value_t=1200, value_parser=clap::value_parser!(u32).range(1..=100000))]
    chunk: u32,
    /// 翻译引擎：auto = 配了 LLM 密钥就用大模型，否则用 Edge 逐句对齐
    #[arg(long, value_enum, default_value = "auto")]
    engine: TranslateEngine,
    /// 逐句对照输出（原文行 / 译文行）
    #[arg(long)]
    bilingual: bool,
    /// 目标语言代码
    #[arg(long, default_value = "zh-Hans")]
    to: String,
    /// 源语言代码；auto 交给服务端识别（仅 --engine edge 使用）
    #[arg(long, default_value = "auto")]
    from: String,
    /// 单个请求的 UTF-16 码元预算（撞上端点限制时会自动折半）
    #[arg(long, default_value_t = 3600, value_parser = batch_units_parser)]
    batch_units: u32,
    /// TSV 术语表：英文<TAB>中文（仅 --engine llm 使用）
    #[arg(long)]
    glossary: Option<PathBuf>,
    /// 自动术语表：从原文探测词库收录的短语并按其释义固定译法。some 接受"词名出现
    /// 在自己释义里"的弱依据，默认 strong 只认例句高亮词（仅 --engine llm 使用）
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "strong")]
    glossary_auto: Option<crate::core::evidence::KeyEvidence>,
    /// 提示词模板文件，替换内置的用户消息措辞（仅 --engine llm 使用）
    #[arg(long)]
    style_file: Option<PathBuf>,
    /// 分段翻译时不带上一段译文尾部（仅 --engine llm 使用）
    #[arg(long)]
    no_prev_output: bool,
    /// 保存译文
    #[arg(long)]
    out: Option<PathBuf>,
    /// 不使用本地缓存，强制重新请求
    #[arg(long)]
    no_cache: bool,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum TranslateEngine {
    Auto,
    Edge,
    Llm,
}

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
enum SpeakEngine {
    Auto,
    Edge,
    Youdao,
}

struct SpeakPlan {
    primary: crate::net::tts::SpeechSource,
    fallback: Option<crate::net::tts::SpeechSource>,
}

/// 把 `--engine/--voice/--rate/--accent` 组装成后端；跨引擎串用按参数错误处理，
/// 不静默忽略——用户以为换了口音、其实没有，比报错更难查。
fn speak_source(
    engine: SpeakEngine,
    voice: Option<String>,
    rate: Option<String>,
    accent: Option<crate::net::tts::Accent>,
) -> Result<SpeakPlan> {
    let edge = |voice: Option<String>, rate: Option<String>| crate::net::tts::SpeechSource::Edge {
        voice: voice.unwrap_or_else(|| "en-US-AriaNeural".into()),
        rate: rate.unwrap_or_else(|| "+0%".into()),
    };
    match engine {
        SpeakEngine::Auto => Ok(SpeakPlan {
            primary: edge(voice, rate),
            fallback: Some(crate::net::tts::SpeechSource::Youdao {
                accent: accent.unwrap_or(crate::net::tts::Accent::Us),
            }),
        }),
        SpeakEngine::Edge => {
            ensure!(
                accent.is_none(),
                "--accent 只对 --engine youdao 有效，或留给 auto 的兜底；Edge 用 --voice 选声音"
            );
            Ok(SpeakPlan {
                primary: edge(voice, rate),
                fallback: None,
            })
        }
        SpeakEngine::Youdao => {
            ensure!(
                voice.is_none() && rate.is_none(),
                "--engine youdao 不支持 --voice/--rate；它只有 --accent us|uk"
            );
            Ok(SpeakPlan {
                primary: crate::net::tts::SpeechSource::Youdao {
                    accent: accent.unwrap_or(crate::net::tts::Accent::Us),
                },
                fallback: None,
            })
        }
    }
}

fn batch_units_parser(s: &str) -> std::result::Result<u32, String> {
    let n = s.parse::<u32>().map_err(|_| "需要整数".to_string())?;
    if (200..=8000).contains(&n) {
        Ok(n)
    } else {
        Err("须为 200–8000".into())
    }
}

#[derive(Args)]
struct ExplainArgs {
    /// 要讲解的词或词形；有独立词卡按原词，无卡词形才经词形归并到原形
    word: String,
    /// 该词出现的原句，用来判断讲的是哪一条义项
    #[arg(long)]
    in_context: Option<String>,
    /// 讲解侧重：definition 逐义项、context 判义项、examples 搭配与例句
    #[arg(long, value_enum, default_value = "definition")]
    style: crate::app::explain::Style,
    /// 讲解用词的难易档位；本地词库没有逐词 CEFR 标注，这只影响措辞
    #[arg(long, value_enum)]
    cefr: Option<crate::app::explain::Cefr>,
    /// 用哪种语言讲解
    #[arg(long, default_value = "zh-Hans")]
    to: String,
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long, hide = true)]
    api_key: Option<String>,
    #[arg(long)]
    model: Option<String>,
    /// 不使用本地缓存，强制重新请求
    #[arg(long)]
    no_cache: bool,
}

#[derive(Args)]
struct HintArgs {
    #[arg(required_unless_present = "file", conflicts_with = "file")]
    text: Option<String>,
    #[arg(long)]
    file: Option<PathBuf>,
    /// 目标难度档（源表带号，1 = 最常见）；比它更靠后的带算超纲
    #[arg(long, default_value_t = 4, value_parser = level_parser)]
    level: u8,
    /// 把难度档标回原文（〖档号〗/〖大纲〗/〖?〗＝没有档）
    #[arg(long)]
    annotate: bool,
    /// 只列词级表给不出档位的词，不列超纲词
    #[arg(long)]
    only_unknown: bool,
    /// 词库收录的多词条目按一个词计，避免把短语数成一串生词
    #[arg(long)]
    include_phrase: bool,
}

fn level_parser(s: &str) -> std::result::Result<u8, String> {
    let n = s.parse::<u8>().map_err(|_| "需要整数".to_string())?;
    if (1..=10).contains(&n) {
        Ok(n)
    } else {
        Err("须为 1–10".into())
    }
}

fn rows_parser(s: &str) -> std::result::Result<usize, String> {
    let n = s.parse::<usize>().map_err(|_| "需要整数".to_string())?;
    if (1..=100_000).contains(&n) {
        Ok(n)
    } else {
        Err("须为 1–100000".into())
    }
}

#[derive(Args)]
struct ExportArgs {
    /// csv 的字段可含换行（每条义项一行）；tsv 会把换行压成空格
    #[arg(long, value_enum, default_value = "csv")]
    format: crate::render::export::Format,
    /// 导出多少条（按字母顺序，从 --offset 起）
    #[arg(long, default_value_t = 200, value_parser = rows_parser)]
    rows: usize,
    /// 从 --offset 开始取；--all 时忽略 --rows
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// 导出全部词条：逐条读取，量大时慢
    #[arg(long)]
    all: bool,
    /// 随机抽样（最多 1000 条）
    #[arg(long)]
    random: bool,
    /// 保存路径；不填就打到标准输出
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct StudyArgs {
    /// 学习数据路径；默认用户数据目录的 study.sqlite，可用 ZEDICT_STUDY_DB 覆盖
    #[arg(long, global = true)]
    file: Option<PathBuf>,
    #[command(subcommand)]
    action: StudyAction,
}

#[derive(Subcommand)]
enum StudyAction {
    /// 加入生词本；已在生词本里时不改变复习进度
    Add { word: String },
    /// 从生词本移除
    Remove { word: String },
    /// 列出词条，按到期时间排序
    List {
        /// 只看已经到期的
        #[arg(long)]
        due: bool,
        #[arg(long, default_value_t = 20, value_parser = limit_parser)]
        limit: usize,
    },
    /// 记录一次复习评分并安排下次
    Review {
        word: String,
        #[arg(long, value_enum)]
        grade: crate::app::study::Grade,
    },
    /// 生词本统计
    Stats,
    /// 导出 Anki .apkg（牌组 + 基础卡）
    Export {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "zedict")]
        deck: String,
        /// 只导出已到期的词
        #[arg(long)]
        due: bool,
    },
}

fn output(s: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(s.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

pub fn entry() -> std::process::ExitCode {
    let cli = match Cli::try_parse_from(normalized_args()) {
        Ok(cli) => cli,
        Err(err) => {
            let code = err.exit_code() as u8;
            let _ = err.print();
            return code.into();
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("错误: {err}");
            return 1.into();
        }
    };
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = runtime.block_on(async {
        tokio::select! {
            result = run(cli, cancel.clone()) => result,
            signal = tokio::signal::ctrl_c() => { signal?; cancel.store(true, std::sync::atomic::Ordering::Relaxed); Ok(130) }
        }
    });
    match result {
        Ok(code) => code.into(),
        Err(err)
            if err.chain().any(|e| {
                e.downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
            }) =>
        {
            0.into()
        }
        Err(err) => {
            let _ = writeln!(io::stderr().lock(), "错误: {err:#}");
            1.into()
        }
    }
}

// Preserve the existing `translate --mode natural|literal|explain` spelling,
// while query --mode remains a single global, typed search option.
fn normalized_args() -> Vec<std::ffi::OsString> {
    let mut args: Vec<_> = std::env::args_os().collect();
    let commands = [
        "query",
        "audit",
        "compile",
        "doctor",
        "completions",
        "words",
        "examples",
        "translate",
        "explain",
        "hint",
        "export",
        "study",
        "speak",
        "serve",
        "help",
    ];
    let mut first = None;
    let mut i = 1;
    while i < args.len() {
        let s = args[i].to_string_lossy();
        if ["--db", "--config", "--theme", "--mode", "--limit"].contains(&s.as_ref()) {
            i += 2;
            continue;
        }
        if s == "--" {
            args.insert(i, "query".into());
            first = Some(i);
            break;
        }
        if !s.starts_with('-') {
            if !commands.contains(&s.as_ref()) {
                args.insert(i, "query".into());
            }
            first = Some(i);
            break;
        }
        i += 1;
    }
    if let Some(i) = first.filter(|i| args[*i] == "translate") {
        let mut j = i + 1;
        while j < args.len() {
            if args[j] == "--" {
                break;
            }
            if args[j] == "--mode" {
                args[j] = "--translation-mode".into();
                j += 2;
                continue;
            }
            if let Some(s) = args[j].to_str().and_then(|s| s.strip_prefix("--mode=")) {
                args[j] = format!("--translation-mode={s}").into();
            }
            // Skip option values so a translation text equal to '--mode' is not rewritten.
            else if args[j].to_str().is_some_and(|s| {
                [
                    "--file",
                    "--context",
                    "--summary",
                    "--base-url",
                    "--api-key",
                    "--model",
                    "--chunk",
                    "--db",
                    "--config",
                    "--translation-mode",
                    "--engine",
                    "--to",
                    "--from",
                    "--batch-units",
                    "--glossary",
                    "--style-file",
                    "--out",
                ]
                .contains(&s)
            }) {
                j += 2;
                continue;
            }
            j += 1;
        }
    }
    args
}

async fn run(cli: Cli, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<u8> {
    if cli.command.is_none() {
        Cli::command().write_long_help(&mut io::stdout().lock())?;
        output("\n")?;
        return Ok(0);
    }
    if let Some(Command::Completions { shell }) = &cli.command {
        clap_complete::generate(
            *shell,
            &mut Cli::command(),
            "zedict",
            &mut io::stdout().lock(),
        );
        return Ok(0);
    }
    // Input errors are reported before opening a dictionary.
    let word = match &cli.command {
        Some(Command::Query { word, .. }) => word.as_ref(),
        Some(Command::Explain(args)) => Some(&args.word),
        Some(Command::Examples { text }) => Some(text),
        _ => None,
    };
    if let Some(word) = word {
        if let Err(err) = validate_query(word) {
            let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
            return Ok(2);
        }
    }
    let settings = Settings::load(cli.config.as_deref(), cli.db.as_deref())?;
    let brief = if cli.full {
        false
    } else {
        cli.brief || settings.config.brief.unwrap_or(false)
    };
    let width = if io::stdout().is_terminal() {
        textwrap::termwidth()
    } else {
        100
    };
    let theme = cli
        .theme
        .or_else(|| {
            std::env::var("ZEDICT_THEME")
                .ok()
                .and_then(|value| crate::render::ThemeChoice::parse(&value))
        })
        .or(settings.config.theme)
        .unwrap_or_default()
        .resolve();
    match cli.command {
        None | Some(Command::Query { .. }) => {
            let (word, stdin, en_zh, no_en_zh) = match cli.command {
                Some(Command::Query {
                    word,
                    stdin,
                    en_zh,
                    no_en_zh,
                }) => (word, stdin, en_zh, no_en_zh),
                _ => (None, false, false, false),
            };
            if cli.json && en_zh {
                let _ = writeln!(
                    io::stderr().lock(),
                    "提示: --en-zh 只补充文本输出，--json 的字段和形状保持不变"
                );
            }
            let translate = should_translate(
                en_zh,
                no_en_zh,
                settings.config.en_zh,
                stdin,
                cli.json,
                io::stdout().is_terminal(),
            );
            let store = DictStore::open(&settings.db)?;
            let ctx = QueryOutput {
                store: &store,
                mode: cli.mode,
                limit: cli.limit,
                json: cli.json,
                settings: &settings,
                brief,
                width,
                theme,
                translate,
                auto_translate: translate && !en_zh,
            };
            let mut code = 0u8;
            if stdin {
                // A detached reader keeps Ctrl+C responsive even while a pipe is idle.
                let (send, mut receive) = tokio::sync::mpsc::channel(16);
                std::thread::spawn(move || {
                    for line in io::stdin().lock().lines() {
                        if send.blocking_send(line).is_err() {
                            break;
                        }
                    }
                });
                while let Some(line) = receive.recv().await {
                    let line = line?;
                    if !line.trim().is_empty() {
                        if let Err(err) = validate_query(&line) {
                            let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                            return Ok(2);
                        }
                        emit_query(line, &ctx, &mut code).await?;
                    }
                }
            } else {
                emit_query(word.context("缺少查询词")?, &ctx, &mut code).await?;
            }
            Ok(code)
        }
        Some(Command::Audit { input, out }) => {
            let report = tokio::task::spawn_blocking(move || {
                crate::data::ingest::audit_cancellable(&input, &out, Some(&cancel))
            })
            .await??;
            output(&(serde_json::to_string_pretty(&report)? + "\n"))?;
            Ok(0)
        }
        Some(Command::Compile {
            input,
            reviews,
            corrections,
            inflections,
            related,
            levels,
            examples,
            spell,
            pinyin,
            key_evidence,
            vocab,
            out,
            install,
        }) => {
            let target = if install {
                settings.db
            } else {
                out.join("dict.sqlite")
            };
            let build_target = target.clone();
            let report = tokio::task::spawn_blocking(move || {
                // Extras 借用这两个路径，只能在闭包里组装：spawn_blocking 要求闭包
                // 'static，在外面构造会带着对本地变量的引用跨过去。
                let extras = crate::data::ingest::Extras {
                    inflections: inflections.as_deref(),
                    related: related.as_deref(),
                    levels: levels.as_deref(),
                    examples,
                    spell,
                    pinyin,
                    key_evidence,
                    vocab: vocab.as_deref(),
                };
                if let Some(reviews) = reviews.as_deref() {
                    crate::data::ingest::build_with_extras(
                        &input,
                        reviews,
                        &extras,
                        &build_target,
                        Some(&cancel),
                    )
                } else {
                    crate::data::ingest::import_structured_with_extras(
                        &input,
                        corrections.as_deref(),
                        &extras,
                        &build_target,
                        Some(&cancel),
                    )
                }
            })
            .await??;
            if cli.json {
                output(&(serde_json::to_string(&report)? + "\n"))?;
            } else {
                // 没带 --levels 就不报词级：报 0 会让人以为表建了但是空的。
                let levels = if report.word_levels > 0 {
                    format!("词级: {} 个词形\n", report.word_levels)
                } else {
                    String::new()
                };
                let index = if report.examples > 0 {
                    format!(
                        "例句索引: {} 句、{} 条关键词链；剔除 {} 个挂太多例句的中文关键词\n",
                        report.examples, report.example_links, report.dropped_noisy_keys
                    )
                } else {
                    String::new()
                };
                let spell = if report.spell_rows > 0 {
                    format!("拼写候选索引: {} 条删字符邻域\n", report.spell_rows)
                } else {
                    String::new()
                };
                let pinyin = if report.pinyin_rows > 0 {
                    format!(
                        "拼音索引: {} 个词条、{} 条读音键\n",
                        report.pinyin_cards, report.pinyin_rows
                    )
                } else {
                    String::new()
                };
                // 档位普查在任何阈值下都报，包括 all：用户要先看见"库里有多少词头
                // 在本条记录里找不到自己"，才谈得上决定要不要丢。
                let evidence = if report.key_evidence == "all" || report.key_evidence.is_empty() {
                    format!(
                        "收录依据: 例句可核对 {}；释义中可核对 {}；无直接依据 {}；当前全收，一条不丢\n",
                        report.tier_example_key, report.tier_in_gloss, report.tier_unevidenced
                    )
                } else {
                    format!(
                        "收录依据: 例句可核对 {}；释义中可核对 {}；无直接依据 {}；当前阈值 {}，未收录 {} 条\n",
                        report.tier_example_key,
                        report.tier_in_gloss,
                        report.tier_unevidenced,
                        report.key_evidence,
                        report.dropped_unevidenced_cards
                    )
                };
                let vocab = if report.vocab_rows > 0 {
                    format!("参考词表: {} 个词\n", report.vocab_rows)
                } else {
                    String::new()
                };
                output(&format!(
                    "词库: {}\n可查询词条: {}；词条记录: {}；缺英文词名的记录: {}\n词形变化: {} 条入库，{} 条在词库中找不到原形\n近义/反义中文: {} 条\n{levels}{index}{spell}{pinyin}{evidence}{vocab}",
                    target.display(),
                    report.indexed_headwords,
                    report.indexed_cards,
                    report.missing_headword_records,
                    report.inflections,
                    report.inflections_unmatched,
                    report.related_glosses
                ))?;
            }
            Ok(0)
        }
        Some(Command::Doctor) => {
            let store = DictStore::open(&settings.db)?;
            store.check_integrity()?;
            // datasets 里有记录不等于表非空，所以三张可选表在不在要单独说清。
            let result = serde_json::json!({"status":"ok","db":settings.db,"config":settings.config_path,"schema_version":crate::data::store::SCHEMA_VERSION,"tables":{"levels":store.has_levels(),"examples":store.has_examples(),"spell":store.has_spell(),"pinyin":store.has_pinyin(),"key_evidence":store.has_key_evidence(),"vocab":store.has_vocab()},"report":store.report()?,"datasets":store.datasets()?,"levels":store.levels_meta()?});
            output(&(serde_json::to_string_pretty(&result)? + "\n"))?;
            Ok(0)
        }
        Some(Command::Words { offset, random }) => {
            let store = DictStore::open(&settings.db)?;
            let words = if random {
                store.random(cli.limit)?
            } else {
                store.words(offset, cli.limit)?
            };
            if cli.json {
                output(&(serde_json::to_string(&words)? + "\n"))?;
            } else {
                for word in &words {
                    output(&(word.to_owned() + "\n"))?;
                }
            }
            Ok(if words.is_empty() { 3 } else { 0 })
        }
        Some(Command::Examples { text }) => {
            let store = DictStore::open(&settings.db)?;
            ensure!(
                store.has_examples(),
                "词库没有例句索引；重建：zedict compile --examples --input <源.jsonl> --out <目录>"
            );
            let needle = crate::data::store::normalize(&text);
            // 先按关键词精确命中：`in spite of`、`病假条` 这种就是它自己的索引。
            // 一个没有再全文扫描，那时 `matched_by` 要如实写 scan。
            let mut rows = store.examples_for(std::slice::from_ref(&needle), cli.limit)?;
            let mut matched_by = "key";
            if rows.is_empty() {
                rows = store.examples_scan(&needle, cli.limit)?;
                matched_by = "scan";
            }
            let matches = if matched_by == "key" {
                store.example_key_matches(&needle)?
            } else {
                store.example_scan_matches(&needle)?
            };
            if cli.json {
                let value = serde_json::json!({
                    "query": text,
                    "matched_by": matched_by,
                    "matches": matches,
                    "rows": rows.len(),
                    "examples": rows,
                });
                output(&(serde_json::to_string(&value)? + "\n"))?;
            } else if rows.is_empty() {
                output(&format!("例句库里没有: {text}\n"))?;
            } else {
                output(&format!(
                    "{} 句命中，显示 {} 句（{}）\n",
                    matches,
                    rows.len(),
                    if matched_by == "key" {
                        "关键词"
                    } else {
                        "全文扫描"
                    }
                ))?;
                output(&crate::render::examples_themed(&rows, !brief, width, theme))?;
                // 一行可以直接复制的下一步。句子里带引号时不打印——那样复制回来的
                // 参数是断的，比少一行提示更糟。
                if let Some(hit) = rows.first().filter(|hit| !hit.en.contains('"')) {
                    if let Some(word) = hit.headwords.first() {
                        output(&format!(
                            "下一步: zedict explain {word} --in-context \"{}\"\n",
                            hit.en
                        ))?;
                    }
                }
            }
            Ok(if rows.is_empty() { 3 } else { 0 })
        }
        Some(Command::Translate(args)) => {
            let input = match translation_input(&args)? {
                Ok(text) => text,
                Err(message) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {message}");
                    return Ok(2);
                }
            };
            // 语言代码在选引擎之前就要成立：`--to` 对两条通路都是承诺，不是只对
            // Edge 生效的选项。
            let to = match crate::net::llm::Target::parse(&args.to) {
                Ok(target) => target,
                Err(err) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                    return Ok(2);
                }
            };
            let from = if args.from == "auto" {
                None
            } else {
                match crate::net::llm::Target::parse(&args.from) {
                    Ok(target) => Some(target),
                    Err(err) => {
                        let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                        return Ok(2);
                    }
                }
            };
            let template = match args
                .style_file
                .as_deref()
                .map(crate::net::llm::Template::load)
            {
                Some(Ok(template)) => Some(template),
                Some(Err(err)) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {err:#}");
                    return Ok(2);
                }
                None => None,
            };
            let glossary = match args.glossary.as_deref().map(crate::net::llm::load_glossary) {
                Some(Ok(glossary)) => Some(glossary),
                Some(Err(err)) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {err:#}");
                    return Ok(2);
                }
                None => None,
            };
            let llm = configured_llm(&args, &settings);
            let use_edge = match args.engine {
                TranslateEngine::Edge => true,
                TranslateEngine::Llm => {
                    if llm.is_none() {
                        let _ = writeln!(
                            io::stderr().lock(),
                            "参数错误: 缺少 API key (--api-key 或 LLM_API_KEY)；免密钥请改用 --engine edge"
                        );
                        return Ok(2);
                    }
                    false
                }
                TranslateEngine::Auto => llm.is_none(),
            };
            if use_edge {
                let from = if args.from == "auto" {
                    None
                } else {
                    Some(args.from.clone())
                };
                let client = match crate::net::edge::EdgeClient::new(from, &args.to) {
                    Ok(client) => client,
                    Err(err) => {
                        let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                        return Ok(2);
                    }
                };
                warn_edge_ignores(&args);
                let mut progress = progress_printer();
                let report = crate::net::edge::translate_doc(
                    &client,
                    &input,
                    args.batch_units as usize,
                    !args.no_cache,
                    &mut progress,
                )
                .await?;
                return emit_doc(
                    &args,
                    cli.json,
                    width,
                    &report.doc,
                    report.requests,
                    report.cache_hits,
                );
            }
            let glossary = match args.glossary_auto {
                None => glossary,
                Some(evidence) => {
                    let store = DictStore::open(&settings.db)?;
                    let merged = crate::net::llm::merge_glossary(
                        glossary,
                        crate::net::llm::auto_glossary(&store, &input, evidence)?,
                    );
                    let _ = writeln!(
                        io::stderr().lock(),
                        "提示: 自动术语表 {} 条（收录依据阈值 {}）",
                        merged.0.len(),
                        evidence.as_str()
                    );
                    Some(merged)
                }
            };
            let cfg = llm.expect("--engine llm 已确认配置可用");
            let base = crate::net::llm::Prompt {
                mode: args.style,
                from,
                to,
                context: args.context.as_deref(),
                summary: args.summary.as_deref(),
                glossary: glossary.as_ref(),
                template: template.as_ref(),
                ..Default::default()
            };
            if args.bilingual {
                let mut progress = progress_printer();
                let job = crate::net::llm::DocJob {
                    prompt: base,
                    text: &input,
                    from: &args.from,
                    to: &args.to,
                    chunk: args.chunk as usize,
                    prev_output: !args.no_prev_output,
                };
                let doc = crate::net::llm::translate_doc(&cfg, &job, &mut progress).await?;
                return emit_doc(&args, cli.json, width, &doc, 0, 0);
            }
            let mut pieces = Vec::new();
            let mut write_error = None;
            let mut previous: Option<String> = None;
            for part in crate::net::llm::split_paragraphs(&input, args.chunk as usize) {
                let prompt = crate::net::llm::Prompt {
                    prev_output: (!args.no_prev_output)
                        .then_some(previous.as_deref())
                        .flatten(),
                    ..base.clone()
                };
                let result = crate::net::llm::translate_stream(&cfg, &prompt, &part, |delta| {
                    if !cli.json && write_error.is_none() {
                        if let Err(err) = output(delta) {
                            write_error = Some(err);
                        }
                    }
                })
                .await?;
                if let Some(err) = write_error {
                    return Err(err);
                }
                if !cli.json {
                    output("\n")?;
                }
                previous =
                    (!result.trim().is_empty()).then(|| crate::net::llm::translation_tail(&result));
                pieces.push(result);
            }
            let text = pieces.join("\n");
            if cli.json {
                output(
                    &(serde_json::to_string(&serde_json::json!({"engine":"llm","text":text}))?
                        + "\n"),
                )?;
            }
            if let Some(path) = &args.out {
                crate::app::cache::write_bytes(path, format!("{text}\n").as_bytes())?;
            }
            Ok(0)
        }
        Some(Command::Explain(args)) => {
            if let Err(err) = crate::app::explain::validate_context(args.in_context.as_deref()) {
                let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                return Ok(2);
            }
            let Some(cfg) = llm_config(
                args.base_url.clone(),
                args.api_key.clone(),
                args.model.clone(),
                &settings,
            ) else {
                let _ = writeln!(
                    io::stderr().lock(),
                    "参数错误: 缺少 API key (--api-key 或 LLM_API_KEY)；讲解必须走大模型，离线查询请用 zedict query"
                );
                return Ok(2);
            };
            let store = DictStore::open(&settings.db)?;
            let to = match crate::net::llm::Target::parse(&args.to) {
                Ok(target) => target,
                Err(err) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                    return Ok(2);
                }
            };
            let grounding = crate::app::explain::ground(&store, &args.word)?;
            // 未命中不是错误：照常讲解，但告诉用户这次没有词库依据。
            if grounding.is_none() {
                let _ = writeln!(
                    io::stderr().lock(),
                    "提示: 本地词库未收录 {}，本次讲解不基于词库释义",
                    args.word
                );
            }
            let grounded = grounding.is_some();
            let headword = grounding.as_ref().map(|g| g.headword.clone());
            let match_type = grounding.as_ref().map(|g| g.match_type.clone());
            let senses = grounding.as_ref().map(|g| g.senses);
            let job = crate::app::explain::ExplainJob {
                word: &args.word,
                style: args.style,
                cefr: args.cefr,
                context: args.in_context.as_deref(),
                grounding,
                to,
            };
            let mut write_error = None;
            let result = crate::app::explain::explain(&cfg, &job, !args.no_cache, |delta| {
                if !cli.json && write_error.is_none() {
                    if let Err(err) = output(delta) {
                        write_error = Some(err);
                    }
                }
            })
            .await?;
            if let Some(err) = write_error {
                return Err(err);
            }
            if cli.json {
                output(
                    &(serde_json::to_string(&serde_json::json!({
                        "word": args.word,
                        "grounded": grounded,
                        "headword": headword,
                        "match_type": match_type,
                        "senses": senses,
                        "style": serde_json::to_value(args.style)?,
                        "cefr": serde_json::to_value(args.cefr)?,
                        "explanation": result.text,
                        "engine": "llm",
                        "model": cfg.model,
                        "cache": result.cached
                    }))? + "\n"),
                )?;
            } else {
                output("\n")?;
            }
            Ok(0)
        }
        Some(Command::Hint(args)) => {
            let input = match text_input("统计", args.text.clone(), args.file.as_deref())? {
                Ok(text) => text,
                Err(message) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {message}");
                    return Ok(2);
                }
            };
            let store = DictStore::open(&settings.db)?;
            ensure!(
                store.has_levels(),
                "词库没有词级表；先跑 python tools/word_levels.py --input /path/to/dic/level.bin 生成 data/word_levels.jsonl，再用 zedict compile --levels data/word_levels.jsonl 重建"
            );
            let mut analysis = crate::core::lexicon::analyze(
                &store,
                &input,
                args.level,
                args.include_phrase,
                args.annotate,
            )?;
            if args.only_unknown {
                analysis.keep_without_level();
            }
            if cli.json {
                output(&(serde_json::to_string(&analysis)? + "\n"))?;
            } else {
                output(&(crate::core::lexicon::summary_line(&analysis) + "\n\n"))?;
                if analysis.items.is_empty() {
                    output("没有要列出的词。\n")?;
                } else {
                    for item in &analysis.items {
                        output(&crate::core::lexicon::item_line(item))?;
                    }
                }
                if let Some(annotated) = &analysis.annotated {
                    output("\n标注:\n")?;
                    output(annotated)?;
                    if !annotated.ends_with('\n') {
                        output("\n")?;
                    }
                }
            }
            Ok(0)
        }
        Some(Command::Export(args)) => {
            if args.all && args.random {
                let _ = writeln!(
                    io::stderr().lock(),
                    "参数错误: --all 是全部词条，--random 是抽样，不能同时用"
                );
                return Ok(2);
            }
            if cli.json && args.out.is_none() {
                let _ = writeln!(
                    io::stderr().lock(),
                    "参数错误: --json 只报元数据，卡片正文是分隔符文本，请用 --out 指定保存路径"
                );
                return Ok(2);
            }
            let store = DictStore::open(&settings.db)?;
            if args.random && args.rows > crate::data::store::MAX_LIMIT {
                let _ = writeln!(
                    io::stderr().lock(),
                    "提示: 随机抽样单次最多 {} 条，已按 {} 条处理",
                    crate::data::store::MAX_LIMIT,
                    crate::data::store::MAX_LIMIT
                );
            }
            let keys = export_keys(&store, &args)?;
            let rows = crate::render::export::rows(&store, &keys)?;
            let text = crate::render::export::render(&rows, args.format);
            if let Some(path) = &args.out {
                crate::app::cache::write_bytes(path, text.as_bytes())?;
            } else {
                output(&text)?;
            }
            if cli.json {
                output(
                    &(serde_json::to_string(&serde_json::json!({
                        "format": serde_json::to_value(args.format)?,
                        "keys": keys.len(),
                        "rows": rows.len(),
                        "bytes": text.len(),
                        "out": args.out,
                        "levels": store.has_levels(),
                    }))? + "\n"),
                )?;
            } else if let Some(path) = &args.out {
                output(&format!(
                    "已导出 {} 行 {} → {}（标签含词级: {}）\n",
                    rows.len(),
                    args.format.label(),
                    path.display(),
                    if store.has_levels() { "是" } else { "否" }
                ))?;
            }
            Ok(if rows.is_empty() { 3 } else { 0 })
        }
        Some(Command::Study(args)) => {
            let path = match args.file {
                Some(path) => path,
                None => crate::app::study::default_path()?,
            };
            let study = crate::app::study::StudyStore::open(&path)?;
            let now = crate::app::study::unix_now();
            match args.action {
                StudyAction::Add { word } => {
                    if let Err(err) = validate_query(&word) {
                        let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                        return Ok(2);
                    }
                    let store = DictStore::open(&settings.db)?;
                    let (key, headword) = resolve_study_word(&store, &word)?;
                    let added = study.add(&key, &headword, now)?;
                    let saved = study.get(&key)?.context("刚写入的生词读不回来")?;
                    if cli.json {
                        output(
                            &(serde_json::to_string(
                                &serde_json::json!({"added": added, "word": saved}),
                            )? + "\n"),
                        )?;
                    } else if added {
                        output(&format!("已加入生词本: {} ({key})\n", saved.headword))?;
                    } else {
                        output(&format!(
                            "生词本里已有: {} ({key})，复习进度未变\n",
                            saved.headword
                        ))?;
                    }
                }
                StudyAction::Remove { word } => {
                    if let Err(err) = validate_query(&word) {
                        let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                        return Ok(2);
                    }
                    let store = DictStore::open(&settings.db)?;
                    let key = resolve_study_word(&store, &word)?.0;
                    let removed = study.remove(&key)?;
                    if cli.json {
                        output(
                            &(serde_json::to_string(
                                &serde_json::json!({"removed": removed, "key": key}),
                            )? + "\n"),
                        )?;
                    } else if removed {
                        output(&format!("已从生词本移除: {key}\n"))?;
                    } else {
                        output(&format!("生词本里没有: {key}\n"))?;
                    }
                    if !removed {
                        return Ok(3);
                    }
                }
                StudyAction::List { due, limit } => {
                    let words = study.list(due, limit, now)?;
                    let stats = study.stats(now)?;
                    if cli.json {
                        output(
                            &(serde_json::to_string(&serde_json::json!({
                                "due_only": due,
                                "total": stats.total,
                                "due": stats.due,
                                "words": words,
                            }))? + "\n"),
                        )?;
                    } else if words.is_empty() {
                        output(if due {
                            "没有到期的生词\n"
                        } else {
                            "生词本是空的；用 zedict study add <词> 加入\n"
                        })?;
                    } else {
                        output(&format!(
                            "生词本 {} 条（到期 {}）：\n",
                            stats.total, stats.due
                        ))?;
                        for word in &words {
                            output(&format!(
                                "{:<24} {:>10}  {}  间隔 {:.1} 天  ease {:.2}  复习 {} 次\n",
                                word.headword,
                                crate::app::study::due_label(word.due_at, now),
                                state_label(&word.state),
                                word.interval_days,
                                word.ease,
                                word.reps
                            ))?;
                        }
                    }
                }
                StudyAction::Review { word, grade } => {
                    if let Err(err) = validate_query(&word) {
                        let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                        return Ok(2);
                    }
                    let store = DictStore::open(&settings.db)?;
                    let key = resolve_study_word(&store, &word)?.0;
                    let reviewed = study.review(&key, grade, now)?;
                    if cli.json {
                        output(
                            &(serde_json::to_string(&serde_json::json!({"word": reviewed}))?
                                + "\n"),
                        )?;
                    } else {
                        output(&format!(
                            "{}: 已记录 {}，下次 {}（间隔 {:.2} 天，ease {:.2}）\n",
                            reviewed.headword,
                            grade.label(),
                            crate::app::study::due_label(reviewed.due_at, now),
                            reviewed.interval_days,
                            reviewed.ease
                        ))?;
                    }
                }
                StudyAction::Stats => {
                    let stats = study.stats(now)?;
                    if cli.json {
                        output(&(serde_json::to_string(&stats)? + "\n"))?;
                    } else {
                        output(&format!(
                            "生词本 {} 条：新词 {} / 学习中 {} / 复习 {}；到期 {}；累计复习 {} 次\n",
                            stats.total,
                            stats.new,
                            stats.learning,
                            stats.review,
                            stats.due,
                            stats.reviews
                        ))?;
                    }
                }
                StudyAction::Export { out, deck, due } => {
                    let mut words = study.all()?;
                    if due {
                        words.retain(|word| word.due_at <= now);
                    }
                    ensure!(
                        !words.is_empty(),
                        "没有可导出的生词；先 zedict study add <词>"
                    );
                    let store = DictStore::open(&settings.db)?;
                    let mut notes = Vec::with_capacity(words.len());
                    for word in &words {
                        let back = match store.lookup(&word.key)? {
                            Some(hit) => hit.cards.first().map(crate::render::export::back),
                            None => None,
                        }
                        .unwrap_or_else(|| "（词库未收录）".into());
                        notes.push(crate::render::anki::Note {
                            front: word.headword.clone(),
                            back,
                            tags: vec!["zedict".into(), "study".into()],
                        });
                    }
                    let count = crate::render::anki::write_apkg(&out, &deck, &notes)?;
                    if cli.json {
                        output(
                            &(serde_json::to_string(&serde_json::json!({
                                "out": out,
                                "deck": deck,
                                "notes": count,
                                "due_only": due,
                            }))? + "\n"),
                        )?;
                    } else {
                        output(&format!(
                            "已导出 {count} 张卡片 → {}（牌组 {deck}）\n",
                            out.display()
                        ))?;
                    }
                }
            }
            Ok(0)
        }
        Some(Command::Speak {
            text,
            engine,
            voice,
            rate,
            accent,
            out,
        }) => {
            let SpeakPlan { primary, fallback } = match speak_source(engine, voice, rate, accent) {
                Ok(plan) => plan,
                Err(err) => {
                    let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                    return Ok(2);
                }
            };
            if let Err(err) = crate::net::tts::validate_for(&text, &primary) {
                let _ = writeln!(io::stderr().lock(), "参数错误: {err}");
                return Ok(2);
            }
            let attempt = |src: &crate::net::tts::SpeechSource| {
                let out = out.clone();
                let text = text.clone();
                let src = src.clone();
                async move {
                    match &out {
                        Some(path) => crate::net::tts::synthesize(&text, &src, path).await,
                        None => crate::net::tts::speak(&text, &src).await,
                    }
                }
            };
            let (bytes, src) = match attempt(&primary).await {
                Ok(bytes) => (bytes, primary),
                Err(primary_err) => match fallback {
                    Some(fallback) => {
                        if let Err(unusable) = crate::net::tts::validate_for(&text, &fallback) {
                            return Err(primary_err
                                .context(format!("Edge 发音失败，且有道兜底不可用: {unusable}")));
                        }
                        let _ = writeln!(
                            io::stderr().lock(),
                            "Edge 发音失败: {primary_err:#}；改用 {} 兜底",
                            fallback.describe()
                        );
                        let bytes = attempt(&fallback).await.with_context(|| {
                            format!("有道发音兜底也失败（Edge 原始错误: {primary_err:#}）")
                        })?;
                        (bytes, fallback)
                    }
                    None => return Err(primary_err),
                },
            };
            if cli.json {
                output(
                    &(serde_json::to_string(&serde_json::json!({
                        "bytes": bytes,
                        "out": out,
                        "source": serde_json::to_value(&src)?
                    }))? + "\n"),
                )?;
            } else {
                output(&format!(
                    "音频已{} ({bytes} bytes, {})\n",
                    if out.is_some() {
                        "保存"
                    } else {
                        "交给系统播放器"
                    },
                    src.describe()
                ))?;
            }
            Ok(0)
        }
        #[cfg(feature = "server")]
        Some(Command::Serve { addr, llm }) => {
            crate::app::server::serve(
                addr,
                settings.db,
                if llm {
                    Some(crate::net::llm::LlmConfig::from_env()?)
                } else {
                    None
                },
            )
            .await?;
            Ok(0)
        }
        Some(Command::Completions { .. }) => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::{edge_fragment, should_translate};

    #[test]
    fn automatic_translation_is_on_only_for_interactive_text_queries() {
        // 交互终端、无显式开关：自动翻译。
        assert!(should_translate(false, false, None, false, false, true));
        // 管道/重定向、--json、--stdin 批量：默认不联网，输出保持确定。
        assert!(!should_translate(false, false, None, false, false, false));
        assert!(!should_translate(false, false, None, false, true, true));
        assert!(!should_translate(false, false, None, true, false, true));
        // 显式开关优先于终端判断与配置。
        assert!(should_translate(true, false, None, false, false, false));
        assert!(should_translate(
            true,
            false,
            Some(false),
            true,
            false,
            false
        ));
        assert!(!should_translate(
            false,
            true,
            Some(true),
            false,
            false,
            true
        ));
        // 配置可让管道也翻译，也可关掉自动翻译。
        assert!(should_translate(
            false,
            false,
            Some(true),
            false,
            false,
            false
        ));
        assert!(!should_translate(
            false,
            false,
            Some(false),
            false,
            false,
            true
        ));
        // JSON 永不翻译，哪怕显式加了 --en-zh（CLI 只提示，不改变输出契约）。
        assert!(!should_translate(
            true,
            false,
            Some(true),
            false,
            true,
            true
        ));
    }

    #[test]
    fn edge_fragments_get_a_sentence_stop() {
        assert_eq!(
            edge_fragment("have sexual intercourse with"),
            "have sexual intercourse with."
        );
        assert_eq!(
            edge_fragment("a thing (especially one)"),
            "a thing (especially one)."
        );
        assert_eq!(
            edge_fragment("his love for his work"),
            "his love for his work."
        );
        assert_eq!(edge_fragment("He said \"go\""), "He said \"go\".");
    }

    #[test]
    fn edge_leaves_terminated_lines_unchanged() {
        for line in ["He left.", "Why?", "Stop!", "done...", "“quoted”"] {
            assert_eq!(edge_fragment(line), line);
        }
    }
}
