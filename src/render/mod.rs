//! 终端与文件输出：彩色词卡、CSV/TSV 导出与 Anki `.apkg`。
pub mod anki;
pub mod export;

use crate::{
    core::model::{AffixHint, Example, Relation, Sense, WordCard},
    data::store::{ExampleHit, QueryResponse},
};
use std::collections::HashMap;
use std::io::IsTerminal;

/// 终端配色开关。`auto` 只在确实支持 ANSI 的交互终端上启用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Auto,
    Ascii,
    Rich,
}

impl ThemeChoice {
    /// 供 `ZEDICT_THEME` 使用；拼错时返回 None，让下一级配置接手。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "ascii" | "plain" => Some(Self::Ascii),
            "rich" | "color" | "colour" => Some(Self::Rich),
            _ => None,
        }
    }

    pub fn resolve(self) -> Theme {
        match self {
            Self::Ascii => Theme::Ascii,
            Self::Rich => Theme::Rich,
            Self::Auto if color_allowed() => Theme::Rich,
            Self::Auto => Theme::Ascii,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Ascii,
    Rich,
}

/// 自动配色要同时满足：标准输出是终端、用户没禁用颜色、终端确实识别 ANSI。
/// Windows 的旧版 conhost 默认不处理 ANSI，不能只凭 `is_terminal` 就放行。
fn color_allowed() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return false;
    }
    if std::env::var("CLICOLOR").is_ok_and(|value| value.trim() == "0") {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|value| value.eq_ignore_ascii_case("dumb")) {
        return false;
    }
    #[cfg(windows)]
    {
        let ansi = std::env::var_os("WT_SESSION").is_some_and(|value| !value.is_empty())
            || std::env::var("ConEmuANSI").is_ok_and(|value| value.eq_ignore_ascii_case("on"))
            || std::env::var_os("ANSICON").is_some()
            || std::env::var_os("TERM_PROGRAM").is_some_and(|value| !value.is_empty())
            || std::env::var_os("TERM").is_some_and(|value| !value.is_empty());
        if !ansi {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy)]
enum Ink {
    Plain,
    Title,
    Rule,
    Label,
    Phonetic,
    Number,
    Pos,
    Marker,
    Error,
    Muted,
    Accent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Background {
    Dark,
    Light,
}

/// 配色只用终端标准的 16 色，不写死 RGB：换终端主题时颜色仍由终端决定。
/// 深浅背景各选一套索引，避免浅色主题上亮黄/亮青几乎看不见。
struct Palette {
    title: &'static str,
    rule: &'static str,
    label: &'static str,
    phonetic: &'static str,
    number: &'static str,
    pos: &'static str,
    marker: &'static str,
    error: &'static str,
    muted: &'static str,
    accent: &'static str,
}

const DARK: Palette = Palette {
    title: "1;96",
    rule: "2",
    label: "1;94",
    phonetic: "92",
    number: "1;93",
    pos: "95",
    marker: "96",
    error: "1;91",
    muted: "2",
    accent: "93",
};

const LIGHT: Palette = Palette {
    title: "1;35",
    rule: "2",
    label: "1;34",
    phonetic: "32",
    number: "1;31",
    pos: "35",
    marker: "34",
    error: "1;31",
    muted: "2",
    accent: "35",
};

fn palette() -> &'static Palette {
    static PALETTE: std::sync::OnceLock<&'static Palette> = std::sync::OnceLock::new();
    PALETTE.get_or_init(|| {
        if background() == Background::Light {
            &LIGHT
        } else {
            &DARK
        }
    })
}

fn background() -> Background {
    if let Some(background) = background_from_override() {
        return background;
    }
    if let Some(background) = std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| background_from_colorfgbg(&value))
    {
        return background;
    }
    #[cfg(windows)]
    if let Some(background) = windows_background() {
        return background;
    }
    // 没有可靠信息时按最常见的深色终端处理；浅色主题可用
    // ZEDICT_BACKGROUND=light 明确指定。
    Background::Dark
}

fn background_from_override() -> Option<Background> {
    match std::env::var("ZEDICT_BACKGROUND")
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "dark" => Some(Background::Dark),
        "light" => Some(Background::Light),
        _ => None,
    }
}

/// `COLORFGBG` 通常是 `前景;背景` 的调色板索引。0-6 和 8 是深色，
/// 7 和 9-15 是浅色；解析不了就交给下一级探测。
fn background_from_colorfgbg(value: &str) -> Option<Background> {
    let index = value.rsplit(';').next()?.trim().parse::<u8>().ok()?;
    match index {
        0..=6 | 8 => Some(Background::Dark),
        7 | 9..=15 => Some(Background::Light),
        _ => None,
    }
}

#[cfg(windows)]
fn windows_background() -> Option<Background> {
    use std::ffi::c_void;

    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: SmallRect,
        maximum: Coord,
    }

    extern "system" {
        fn GetStdHandle(id: u32) -> *mut c_void;
        fn GetConsoleScreenBufferInfo(
            handle: *mut c_void,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    let mut info = std::mem::MaybeUninit::<ConsoleScreenBufferInfo>::zeroed();
    // SAFETY: 句柄来自系统调用；输出指针指向已分配、对齐的本地结构体。
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() || handle as isize == -1 {
            return None;
        }
        if GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) == 0 {
            return None;
        }
        let background = (info.assume_init().attributes >> 4) & 0x0F;
        match background {
            7 | 9..=15 => Some(Background::Light),
            _ => Some(Background::Dark),
        }
    }
}

fn paint(theme: Theme, ink: Ink, text: &str) -> String {
    if text.is_empty() || theme == Theme::Ascii {
        return text.to_owned();
    }
    let code = match ink {
        Ink::Plain => return text.to_owned(),
        Ink::Title => palette().title,
        Ink::Rule => palette().rule,
        Ink::Label => palette().label,
        Ink::Phonetic => palette().phonetic,
        Ink::Number => palette().number,
        Ink::Pos => palette().pos,
        Ink::Marker => palette().marker,
        Ink::Error => palette().error,
        Ink::Muted => palette().muted,
        Ink::Accent => palette().accent,
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn safe(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// 先按可见宽度折行，再上色；ANSI 字节不参与宽度计算。
fn wrap(text: &str, width: usize, first: &str, rest: &str) -> String {
    let text = safe(text);
    textwrap::fill(
        &text,
        textwrap::Options::new(width.max(20))
            .initial_indent(first)
            .subsequent_indent(rest)
            .break_words(true),
    )
}

fn body(out: &mut String, text: &str, width: usize, indent: &str, theme: Theme, ink: Ink) {
    if text.trim().is_empty() {
        return;
    }
    let block = wrap(text, width, indent, indent);
    out.push_str(&paint(theme, ink, &block));
    out.push('\n');
}

/// 区块之间恰好空一行；响应为空时不留前导空行。
fn section(out: &mut String, label: &str, theme: Theme) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&section_title(label, theme));
}

pub fn section_title(label: &str, theme: Theme) -> String {
    format!(
        "{}\n",
        paint(theme, Ink::Label, &format!("[{}]", safe(label)))
    )
}

fn meta(out: &mut String, text: &str, width: usize, theme: Theme) {
    let block = wrap(text, width, "", "");
    out.push_str(&paint(theme, Ink::Phonetic, &block));
    out.push('\n');
}

fn bullet(out: &mut String, text: &str, width: usize, theme: Theme) {
    let block = wrap(text, width, "  - ", "    ");
    if block.is_empty() {
        return;
    }
    if theme == Theme::Rich && block.starts_with("  - ") {
        out.push_str(&paint(theme, Ink::Marker, "  - "));
        out.push_str(&block[4..]);
    } else {
        out.push_str(&block);
    }
    out.push('\n');
}

fn quote(out: &mut String, text: &str, width: usize, theme: Theme) {
    let block = wrap(text, width, "  > ", "    ");
    if block.is_empty() {
        return;
    }
    if theme == Theme::Rich && block.starts_with("  > ") {
        out.push_str(&paint(theme, Ink::Marker, "  > "));
        out.push_str(&block[4..]);
    } else {
        out.push_str(&block);
    }
    out.push('\n');
}

/// 关系词必须保持单行，否则分栏的宽度计算会被换行符打乱。
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn relation_label(relation: &Relation) -> String {
    match relation
        .gloss
        .as_deref()
        .map(one_line)
        .filter(|gloss| !gloss.is_empty())
    {
        Some(gloss) => format!("{}（{}）", one_line(&relation.word), gloss),
        None => one_line(&relation.word),
    }
}

/// 关系词列表：宽度够时排两列，窄终端退回单列折行。
/// 分栏只使用空格，不依赖制表符或框线，换终端和复制文本都不会走形。
fn relation_list(out: &mut String, words: &[Relation], width: usize, theme: Theme) {
    let labels: Vec<String> = words.iter().map(relation_label).collect();
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| textwrap::core::display_width(label))
        .collect();
    let marker = "  - ";
    let marker_width = textwrap::core::display_width(marker);
    let gap = 4;
    let rows = words.len().div_ceil(2);
    let left_width = widths[..rows].iter().copied().max().unwrap_or(0);
    let right_width = widths[rows..].iter().copied().max().unwrap_or(0);
    let two_columns =
        words.len() >= 4 && marker_width * 2 + left_width + gap + right_width <= width;

    if !two_columns {
        for label in &labels {
            bullet(out, label, width, theme);
        }
        return;
    }

    for (row, label) in labels[..rows].iter().enumerate() {
        out.push_str(&paint(theme, Ink::Marker, marker));
        out.push_str(label);
        if rows + row < labels.len() {
            out.push_str(&" ".repeat(left_width - widths[row] + gap));
            out.push_str(&paint(theme, Ink::Marker, marker));
            out.push_str(&labels[rows + row]);
        }
        out.push('\n');
    }
}

fn relation_section(out: &mut String, label: &str, words: &[Relation], width: usize, theme: Theme) {
    if words.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&paint(theme, Ink::Label, &format!("[{}]", safe(label))));
    out.push_str(&paint(theme, Ink::Muted, &format!(" {} 个", words.len())));
    out.push('\n');
    relation_list(out, words, width, theme);
}

/// 编号和词性单独上色；续行缩进和首行正文对齐。
fn sense_line(index: usize, sense: &Sense, pos_width: usize, width: usize, theme: Theme) -> String {
    let number = format!("{:>2}. ", index + 1);
    let pos = sense.pos.map(|p| p.as_str()).unwrap_or("");
    let pos_part = if pos.is_empty() {
        String::new()
    } else {
        format!("{pos:<pos_width$} ")
    };
    let lead = format!("  {number}{pos_part}");
    let indent = " ".repeat(lead.chars().count());
    if sense.gloss.trim().is_empty() {
        return lead.trim_end().to_string();
    }
    let block = wrap(&sense.gloss, width, &lead, &indent);
    if theme == Theme::Ascii || !block.starts_with(&lead) {
        return block;
    }
    let (first, rest) = block.split_once('\n').unwrap_or((&block, ""));
    let number_start = 2;
    let number_end = number_start + number.len();
    let pos_end = number_end + pos_part.len();
    let mut styled = String::with_capacity(block.len() + 24);
    styled.push_str(&first[..number_start]);
    styled.push_str(&paint(theme, Ink::Number, &first[number_start..number_end]));
    styled.push_str(&paint(theme, Ink::Pos, &first[number_end..pos_end]));
    styled.push_str(&first[pos_end..]);
    if !rest.is_empty() {
        styled.push('\n');
        styled.push_str(rest);
    }
    styled
}

fn split_definition(line: &str) -> Option<(usize, &str)> {
    let (number, rest) = line.split_once(". ")?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((number.parse().ok()?, rest))
}

fn looks_like_pos_header(line: &str) -> bool {
    let bare = line.trim().trim_end_matches('.');
    !bare.is_empty()
        && matches!(
            bare.to_ascii_lowercase().as_str(),
            "n" | "v"
                | "vt"
                | "vi"
                | "adj"
                | "adv"
                | "prep"
                | "pron"
                | "conj"
                | "int"
                | "interj"
                | "art"
                | "num"
                | "aux"
        )
}

/// 英英块里值得翻译的行：定义和例句，不含词性标题与 `syn:` 词表。
/// 收集与渲染共用同一套判定，避免“翻译了却不显示”。
pub fn translatable_gloss_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut group_start = true;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            group_start = true;
            continue;
        }
        let source = if let Some(rest) = line.strip_prefix("> ") {
            Some(rest)
        } else if line.starts_with("syn: ") {
            None
        } else if let Some((_, rest)) = split_definition(line) {
            Some(rest)
        } else if group_start && looks_like_pos_header(line) {
            None
        } else {
            Some(line)
        };
        if let Some(source) = source {
            if !source.trim().is_empty() && !out.iter().any(|seen| seen == source) {
                out.push(source.to_string());
            }
        }
        group_start = false;
    }
    out
}

fn translation_line(
    out: &mut String,
    source: &str,
    width: usize,
    theme: Theme,
    indent: usize,
    translations: &HashMap<String, String>,
) {
    let Some(zh) = translations.get(source).map(|zh| zh.trim()) else {
        return;
    };
    if zh.is_empty() {
        return;
    }
    let lead = format!("{}译：", " ".repeat(indent));
    let block = wrap(
        zh,
        width,
        &lead,
        &" ".repeat(textwrap::core::display_width(&lead)),
    );
    out.push_str(&paint(theme, Ink::Muted, &block));
    out.push('\n');
}

/// 英英块的行为 `compile::parse_english_gloss` 写下的四类行：
/// 词性、`1.` 定义、`> ` 例句、`syn: ` 同义词。旧词库是一整段，
/// 没有换行时会自然落到最后的纯文本分支。
fn english_gloss(
    out: &mut String,
    text: &str,
    width: usize,
    theme: Theme,
    translations: &HashMap<String, String>,
) {
    let mut group_start = true;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            group_start = true;
            continue;
        }
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("> ") {
            english_example(out, rest, width, theme);
            translation_line(out, rest, width, theme, 7, translations);
        } else if let Some(rest) = line.strip_prefix("syn: ") {
            english_synonyms(out, rest, width, theme);
        } else if let Some((number, rest)) = split_definition(line) {
            english_sense(out, number, rest, width, theme);
            translation_line(out, rest, width, theme, 5, translations);
        } else if group_start {
            out.push_str(&paint(theme, Ink::Pos, &format!("  {}", safe(line))));
            out.push('\n');
        } else {
            body(out, line, width, "  ", theme, Ink::Plain);
            translation_line(out, line, width, theme, 2, translations);
        }
        group_start = false;
    }
}

fn english_sense(out: &mut String, number: usize, text: &str, width: usize, theme: Theme) {
    let marker = format!("{number}.");
    let lead = format!("  {marker} ");
    let indent = " ".repeat(lead.chars().count());
    let block = wrap(text, width, &lead, &indent);
    if theme == Theme::Ascii || !block.starts_with(&lead) {
        out.push_str(&block);
    } else {
        out.push_str("  ");
        out.push_str(&paint(theme, Ink::Number, &marker));
        out.push(' ');
        out.push_str(&block[lead.len()..]);
    }
    out.push('\n');
}

fn english_example(out: &mut String, text: &str, width: usize, theme: Theme) {
    let block = wrap(text, width, "     > ", "       ");
    if theme == Theme::Rich && block.starts_with("     > ") {
        out.push_str("     ");
        out.push_str(&paint(theme, Ink::Marker, "> "));
        out.push_str(&block[7..]);
    } else {
        out.push_str(&block);
    }
    out.push('\n');
}

fn english_synonyms(out: &mut String, text: &str, width: usize, theme: Theme) {
    let block = wrap(text, width, "     syn: ", "          ");
    if theme == Theme::Rich && block.starts_with("     syn: ") {
        out.push_str("     ");
        out.push_str(&paint(theme, Ink::Label, "syn: "));
        out.push_str(&block[10..]);
    } else {
        out.push_str(&block);
    }
    out.push('\n');
}

fn example(out: &mut String, example: &Example, width: usize, theme: Theme) {
    quote(out, &example.en, width, theme);
    if !example.zh.trim().is_empty() {
        let block = wrap(&example.zh, width, "    ", "    ");
        out.push_str(&paint(theme, Ink::Muted, &block));
        out.push('\n');
    }
}

pub fn card(card: &WordCard, brief: bool, width: usize) -> String {
    card_themed(card, brief, width, Theme::Ascii)
}

pub fn card_themed(card: &WordCard, brief: bool, width: usize, theme: Theme) -> String {
    card_themed_with_translations(card, brief, width, theme, &HashMap::new())
}

fn card_themed_with_translations(
    card: &WordCard,
    brief: bool,
    width: usize,
    theme: Theme,
    translations: &HashMap<String, String>,
) -> String {
    let width = width.max(20);
    let mut out = String::new();

    let headword = wrap(&card.headword, width, "", "");
    out.push_str(&paint(theme, Ink::Title, &headword));
    out.push('\n');
    let first = headword.lines().next().unwrap_or("");
    let rule = textwrap::core::display_width(first).clamp(8, 56).min(width);
    out.push_str(&paint(theme, Ink::Rule, &"=".repeat(rule)));
    out.push('\n');

    let phon = [
        card.phonetic.uk.as_ref().map(|p| format!("UK {p}")),
        card.phonetic.us.as_ref().map(|p| format!("US {p}")),
        card.phonetic.en.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ");
    if !phon.is_empty() {
        meta(&mut out, &phon, width, theme);
    }
    if let Some(pinyin) = &card.pinyin {
        meta(&mut out, &format!("拼音: {pinyin}"), width, theme);
    }
    if !brief {
        if let Some(syllables) = &card.phonetic.syllables {
            meta(
                &mut out,
                &format!("音节: {}", syllables.join("·")),
                width,
                theme,
            );
        }
    }

    if !card.senses.is_empty() {
        section(&mut out, "释义", theme);
        let limit = if brief { 3 } else { card.senses.len() };
        let pos_width = card
            .senses
            .iter()
            .take(limit)
            .filter_map(|sense| sense.pos.map(|pos| pos.as_str().len()))
            .max()
            .unwrap_or(0)
            .min(5);
        for (i, sense) in card.senses.iter().take(limit).enumerate() {
            out.push_str(&sense_line(i, sense, pos_width, width, theme));
            out.push('\n');
        }
    }
    if let Some(en) = &card.gloss_en {
        section(&mut out, "英英", theme);
        english_gloss(&mut out, en, width, theme, translations);
    }

    if !brief {
        if !card.forms.is_empty() {
            section(&mut out, "词形", theme);
            for form in &card.forms {
                body(
                    &mut out,
                    &format!("{}: {}", form.kind.as_str(), form.words.join(", ")),
                    width,
                    "  ",
                    theme,
                    Ink::Plain,
                );
            }
        }
        if !card.phrases.is_empty() {
            section(&mut out, "短语", theme);
            for phrase in &card.phrases {
                let text = match &phrase.gloss {
                    Some(gloss) => format!("{}（{gloss}）", phrase.word),
                    None => phrase.word.clone(),
                };
                bullet(&mut out, &text, width, theme);
            }
        }
        if !card.examples.is_empty() {
            section(&mut out, "例句", theme);
            for e in &card.examples {
                example(&mut out, e, width, theme);
            }
        }
        for (label, words) in [
            ("近义", &card.synonyms),
            ("反义", &card.antonyms),
            ("相关", &card.related),
        ] {
            relation_section(&mut out, label, words, width, theme);
        }
    }
    out
}

fn affix_hint_block(out: &mut String, hints: &[AffixHint], width: usize, theme: Theme) {
    if hints.is_empty() {
        return;
    }
    section(out, "记忆提示", theme);
    body(
        out,
        "按常见前缀拆开，和已经认识的基础词挂钩；这是联想记忆，不是严格词源。",
        width,
        "  ",
        theme,
        Ink::Muted,
    );
    for hint in hints {
        let base = if hint.base_gloss.is_empty() {
            hint.base.clone()
        } else {
            format!("{}（{}）", hint.base, hint.base_gloss)
        };
        let mut line = format!("{}（{}）+ {}", hint.prefix, hint.meaning, base);
        if !hint.word_gloss.is_empty() {
            line.push_str(&format!(" → {}", hint.word_gloss));
        }
        body(out, &line, width, "  ", theme, Ink::Plain);
    }
}

pub fn response(response: &QueryResponse, brief: bool, width: usize) -> String {
    response_themed(response, brief, width, Theme::Ascii)
}

pub fn response_themed(
    response: &QueryResponse,
    brief: bool,
    width: usize,
    theme: Theme,
) -> String {
    response_themed_with_hints(response, &HashMap::new(), brief, width, theme)
}

/// 与 [`response_themed`] 相同，但在每张词卡后追加可选的构词提示。
/// 提示只在文本模式使用，JSON 响应形状保持不变。
pub fn response_themed_with_hints(
    response: &QueryResponse,
    hints: &HashMap<String, Vec<AffixHint>>,
    brief: bool,
    width: usize,
    theme: Theme,
) -> String {
    response_themed_with_hints_and_translations(
        response,
        hints,
        &HashMap::new(),
        brief,
        width,
        theme,
    )
}

/// 在上一函数的基础上，给 `[英英]` 的定义和例句补中文译文。
/// 译文是文本模式的增益，JSON 里不会出现。
pub fn response_themed_with_hints_and_translations(
    response: &QueryResponse,
    hints: &HashMap<String, Vec<AffixHint>>,
    translations: &HashMap<String, String>,
    brief: bool,
    width: usize,
    theme: Theme,
) -> String {
    let width = width.max(20);
    let mut out = String::new();
    for hit in &response.hits {
        if hit.match_type == "lemma" {
            let relations = if hit.relations.is_empty() {
                String::new()
            } else {
                format!(" ({})", hit.relations.join(", "))
            };
            let block = wrap(
                &format!("[词形候选] {} -> {}{relations}", response.query, hit.key),
                width,
                "",
                "",
            );
            out.push_str(&paint(theme, Ink::Accent, &block));
            out.push('\n');
        }
        for sense in &hit.matched_senses {
            let block = wrap(&format!("[匹配释义] {sense}"), width, "", "");
            out.push_str(&paint(theme, Ink::Accent, &block));
            out.push('\n');
        }
        for c in &hit.cards {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&card_themed_with_translations(
                c,
                brief,
                width,
                theme,
                translations,
            ));
            out.push('\n');
        }
        if !brief {
            if let Some(list) = hints.get(&hit.key) {
                affix_hint_block(&mut out, list, width, theme);
            }
        }
    }
    if response.status == "not_found" {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        let block = wrap(&format!("[未收录] {}", response.query), width, "", "");
        out.push_str(&paint(theme, Ink::Error, &block));
        out.push('\n');
    }
    if !response.suggestions.is_empty() {
        section(&mut out, "拼写建议", theme);
        for suggestion in &response.suggestions {
            let via = suggestion
                .via
                .as_ref()
                .map(|form| format!("，词形 {form}"))
                .unwrap_or_default();
            bullet(
                &mut out,
                &format!("{}  距离 {}{via}", suggestion.word, suggestion.distance),
                width,
                theme,
            );
        }
    }
    out
}

pub fn examples(rows: &[ExampleHit], full: bool, width: usize) -> String {
    examples_themed(rows, full, width, Theme::Ascii)
}

pub fn examples_themed(rows: &[ExampleHit], full: bool, width: usize, theme: Theme) -> String {
    let width = width.max(20);
    let mut out = String::new();
    for hit in rows {
        quote(&mut out, &hit.en, width, theme);
        if !hit.zh.trim().is_empty() {
            let block = wrap(&hit.zh, width, "    ", "    ");
            out.push_str(&paint(theme, Ink::Muted, &block));
            out.push('\n');
        }
        if full && !hit.headwords.is_empty() {
            let block = wrap(
                &format!("    相关词条: {}", hit.headwords.join(", ")),
                width,
                "",
                "",
            );
            out.push_str(&paint(theme, Ink::Phonetic, &block));
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        background_from_colorfgbg, english_gloss, relation_section, translatable_gloss_sentences,
        Background, Theme,
    };
    use crate::core::model::Relation;
    use std::collections::HashMap;

    fn relation(word: &str, gloss: Option<&str>) -> Relation {
        Relation {
            word: word.to_string(),
            gloss: gloss.map(str::to_string),
        }
    }

    #[test]
    fn colorfgbg_maps_to_background_lightness() {
        assert_eq!(background_from_colorfgbg("15;0"), Some(Background::Dark));
        assert_eq!(background_from_colorfgbg("0;15"), Some(Background::Light));
        assert_eq!(background_from_colorfgbg("7"), Some(Background::Light));
        assert_eq!(background_from_colorfgbg("1;8"), Some(Background::Dark));
        assert_eq!(background_from_colorfgbg("1;16"), None);
        assert_eq!(background_from_colorfgbg(""), None);
    }

    #[test]
    fn wide_relation_sections_use_counted_two_column_lists() {
        let words = [
            relation("emotion", Some("情感")),
            relation("object", Some("宾语")),
            relation("lover", Some("爱人，恋人")),
            relation("detest", None),
        ];
        let mut out = String::new();
        relation_section(&mut out, "近义", &words, 80, Theme::Ascii);

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "[近义] 4 个");
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines[1].contains("emotion（情感）"), "{out}");
        assert!(lines[1].contains("lover（爱人，恋人）"), "{out}");
        assert!(lines[2].contains("object（宾语）"), "{out}");
        assert!(lines[2].contains("detest"), "{out}");
        assert!(out
            .lines()
            .all(|line| textwrap::core::display_width(line) <= 80));
    }

    #[test]
    fn english_gloss_shows_translations_under_definitions_and_examples() {
        let translations = HashMap::from([
            ("a thing".to_string(), "一件事".to_string()),
            ("an example".to_string(), "一个例子".to_string()),
            ("probe".to_string(), "探针".to_string()),
        ]);
        let mut out = String::new();
        english_gloss(
            &mut out,
            "n.\n1. a thing\n> an example\nsyn: probe",
            80,
            Theme::Ascii,
            &translations,
        );

        assert!(out.contains("  1. a thing\n     译：一件事\n"), "{out}");
        assert!(
            out.contains("     > an example\n       译：一个例子\n"),
            "{out}"
        );
        assert!(!out.contains("译：探针"), "词表不逐词翻译: {out}");
        assert!(!out.contains("n.\n     译"), "词性标题不翻译: {out}");
    }

    #[test]
    fn only_definitions_and_examples_are_collected_for_translation() {
        assert_eq!(
            translatable_gloss_sentences(
                "n.\n1. a thing\n> an example\nsyn: probe\n\nv.\n1. a thing"
            ),
            vec!["a thing".to_string(), "an example".to_string()]
        );
        assert_eq!(
            translatable_gloss_sentences("Plain fallback text."),
            vec!["Plain fallback text.".to_string()]
        );
    }

    #[test]
    fn narrow_relation_sections_wrap_one_entry_per_line_without_loss() {
        let words = [
            relation("emotion", Some("情感")),
            relation("object", Some("宾语")),
            relation("lover", Some("爱人，恋人")),
            relation("concupiscence", Some("强烈的性欲，贪欲，渴望")),
        ];
        let mut out = String::new();
        relation_section(&mut out, "近义", &words, 28, Theme::Ascii);

        assert!(out.starts_with("[近义] 4 个\n"), "{out}");
        assert_eq!(out.matches("  - ").count(), 4, "{out}");
        for needle in [
            "emotion",
            "情感",
            "object",
            "宾语",
            "lover",
            "concupiscence",
            "渴望",
        ] {
            assert!(out.contains(needle), "缺少 {needle}: {out}");
        }
        assert!(out
            .lines()
            .all(|line| textwrap::core::display_width(line) <= 28));
    }
}
