//! 词头自证分级：一个词头能不能在它**自己那条记录**里找到证据。
//!
//! 三档的判据都只读本条记录的字段，不查外部词表，所以它是"自证"而不是"核对"：
//! - `ExampleKey`：词头等于本条记录某条例句英文侧的高亮关键词（`<span class="key">`）。
//!   高亮词是语料自己标出来的"这句在讲哪个词"，词头和它一致就说明这条记录确实属于这个词。
//! - `InGloss`：词头作为整词出现在本条记录**自己的中文释义**里。
//! - `Unevidenced`：两处都没有。这一档不是"没把握"，而是已知会出错——生成词头的那条
//!   规则假定"近义词表里包含词条自身"，实测只在 0.3% 的记录上成立，所以这里的词头
//!   常常是隔壁词条的（`angle` 的名下挂着一条"气骨导间距"）。
//!
//! 例句切分复用 `compile::example_sides`、键的比较复用 `store::normalize`，本模块
//! 不引入第二套解析器，也不引入新依赖。

use std::collections::HashSet;

use clap::ValueEnum;

use crate::core::model::strip_html;
use crate::data::compile;
use crate::data::store::normalize;

/// 词头证据的强度档。判别式与数值一并存进可选表 `key_evidence.tier`，改编号要
/// 同时改 `tools/validate_import.py` 里那份独立实现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// 等于本条记录例句英文侧的高亮关键词。
    ExampleKey = 0,
    /// 作为整词出现在本条记录自己的释义文本里。
    InGloss = 1,
    /// 两处都找不到：词头很可能是从别的词条抄来的。
    Unevidenced = 2,
    /// 人工核验清单点名的记录，优先级高于任何自动判据。
    Reviewed = 3,
}

/// 建库时对词头证据的取舍阈值。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum KeyEvidence {
    /// 不做任何过滤，也不建 `key_evidence` 表：今天的行为。
    #[default]
    All,
    /// 丢掉 `Unevidenced`，但保留人工核验过的记录。
    Some,
    /// 只留 `ExampleKey` 和人工核验过的记录。
    Strong,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::ExampleKey => "example-key",
            Tier::InGloss => "in-gloss",
            Tier::Unevidenced => "unevidenced",
            Tier::Reviewed => "reviewed",
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// 表里存的是数字；数字不认识就是库里出了问题，不能当成"某一档"糊过去。
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Tier::ExampleKey),
            1 => Some(Tier::InGloss),
            2 => Some(Tier::Unevidenced),
            3 => Some(Tier::Reviewed),
            _ => None,
        }
    }
}

impl KeyEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyEvidence::All => "all",
            KeyEvidence::Some => "some",
            KeyEvidence::Strong => "strong",
        }
    }

    /// 这条词头在这个阈值下能不能进库。人工核验过的记录任何阈值都收：它们是被
    /// 逐条确认过的，把确认过的内容再过滤掉没有道理，而且 `tools/validate_import.py`
    /// 会断言每一条修正记录都真的选入。
    pub fn accepts(self, tier: Tier) -> bool {
        match self {
            KeyEvidence::All => true,
            KeyEvidence::Some => tier != Tier::Unevidenced,
            KeyEvidence::Strong => matches!(tier, Tier::ExampleKey | Tier::Reviewed),
        }
    }

    /// 是否需要建 `key_evidence` 表并落盘每行的档位。
    pub fn records(self) -> bool {
        self != KeyEvidence::All
    }
}

/// 一条记录的词头属于哪一档。`senses`/`examples` 传的是源 JSON 里的原始字符串
/// 数组（带标签），不是词卡里已经切好的义项——分级要在建卡之前就能独立复算，
/// `tools/validate_import.py` 那边也是从原始行算的。
pub fn key_tier(headword: &str, senses: &[String], examples: &[String]) -> Tier {
    let key = normalize(headword);
    if key.is_empty() {
        return Tier::Unevidenced;
    }
    if example_keys(examples).contains(&key) {
        return Tier::ExampleKey;
    }
    if mentions(&key, &gloss_text(senses)) {
        return Tier::InGloss;
    }
    Tier::Unevidenced
}

/// 本条记录所有例句英文侧的高亮关键词，按词库的 normalize 规则归一。
fn example_keys(examples: &[String]) -> HashSet<String> {
    compile::example_sides(examples)
        .iter()
        .flat_map(|side| side.en_keys.iter())
        .map(|key| normalize(key))
        .filter(|key| !key.is_empty())
        .collect()
}

/// 释义文本去标签后拼成一段，供整词匹配。只读 `senses`，**不读 `english`**：
/// 那个字段是从英文词典侧附过来的释义池，讲的是隔壁词条（`angle` 名下那条
/// `abbr. Azerbaijan/Arizona` 记录的 `english` 里就写着 "the azimuth of a
/// celestial body"），拿它自证等于让错卡给自己盖章。实测放进 D 档会让
/// `angle` 从 3 行回到 23 行、`promotion` 从 0 回到 4。
fn gloss_text(senses: &[String]) -> String {
    normalize(
        &senses
            .iter()
            .map(|sense| strip_html(sense))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// 整词包含。边界只挡 ASCII 小写字母（和分级实测用的 `(?<![a-z])…(?![a-z])`
/// 一致），所以 `a` 不会命中 `cab`，但 `ab` 能命中 `A-B gap` 里的 `ab`。
/// 手写前后缀判断是为了不给这个判据再引一个正则依赖。
fn mentions(needle: &str, haystack: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    let haystack: Vec<char> = haystack.chars().collect();
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let mut at = 0usize;
    while at + needle.len() <= haystack.len() {
        if haystack[at] == needle[0] && haystack[at..].starts_with(&needle[..]) {
            let before = at == 0 || !blocks(haystack[at - 1]);
            let after = at + needle.len() == haystack.len() || !blocks(haystack[at + needle.len()]);
            if before && after {
                return true;
            }
        }
        at += 1;
    }
    false
}

fn blocks(c: char) -> bool {
    c.is_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_highlighted_example_word_is_the_strongest_evidence() {
        let examples = text(&["<span class=\"key\">angle</span> of view@视角"]);
        assert_eq!(
            key_tier("Angle", &text(&["视角"]), &examples),
            Tier::ExampleKey
        );
    }

    #[test]
    fn a_headword_inside_its_own_gloss_is_weaker_but_still_evidence() {
        assert_eq!(
            key_tier("promotion", &text(&["<i>n.</i> promotion 促销活动"]), &[]),
            Tier::InGloss
        );
    }

    #[test]
    fn a_one_letter_headword_does_not_match_inside_a_longer_word() {
        // `a` 在 `cab` 里是子串，但不是整词：裸 contains 会把这一档判成 D。
        assert_eq!(
            key_tier("a", &text(&["cab 出租车"]), &[]),
            Tier::Unevidenced
        );
        // 连字符不挡边界：`gap` 在 `A-B gap` 里算整词，而 `a-b` 这种带连字符的
        // 词头也一样能命中——两边都只挡 ASCII 小写字母。
        assert_eq!(
            key_tier("gap", &text(&["A-B gap 气骨导间距"]), &[]),
            Tier::InGloss
        );
        assert_eq!(
            key_tier("A-B", &text(&["A-B gap 气骨导间距"]), &[]),
            Tier::InGloss
        );
    }

    #[test]
    fn a_headword_named_nowhere_in_the_record_is_unevidenced() {
        // `angle` 名下那条记录：中文释义讲的是"气骨导间距"，词头是从隔壁抄的。
        assert_eq!(
            key_tier("angle", &text(&["气骨导间距"]), &[]),
            Tier::Unevidenced
        );
    }

    #[test]
    fn human_verified_records_always_pass_any_threshold() {
        for evidence in [KeyEvidence::All, KeyEvidence::Some, KeyEvidence::Strong] {
            assert!(evidence.accepts(Tier::Reviewed));
        }
        assert!(!KeyEvidence::Strong.accepts(Tier::InGloss));
        assert!(KeyEvidence::Some.accepts(Tier::InGloss));
        assert!(!KeyEvidence::Some.accepts(Tier::Unevidenced));
        assert_eq!(KeyEvidence::default(), KeyEvidence::All);
        assert!(!KeyEvidence::All.records());
    }

    #[test]
    fn tiers_round_trip_through_their_stored_numbers() {
        for tier in [
            Tier::ExampleKey,
            Tier::InGloss,
            Tier::Unevidenced,
            Tier::Reviewed,
        ] {
            assert_eq!(Tier::from_u8(tier.as_u8()), Some(tier));
        }
        assert_eq!(Tier::from_u8(4), None);
    }
}
