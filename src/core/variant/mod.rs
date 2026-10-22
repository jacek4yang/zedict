//! 繁体→简体折叠表；由 `tools/variant_dump.py` 生成，请勿手改。
//!
//! 数据源：OpenCC `TSCharacters.txt`（Apache-2.0），sha256 = `6b5a0a799bea2bb22c001f635eaa3fc2904310f0c08addbff275477a80ecf09a`；
//! `TSPhrases.txt`（Apache-2.0），sha256 = `b2ef895dd4953b4bb77fc8ef8d26a2a9ca6d43a760ed9a1d767672cfafa6324f`。
//!
//! 词库的释义是简体，反向索引又按单字建，所以繁体查询要先折成简体才可能命中。
//! 折叠分两层：先试词组表，命中就用词组的简体值并整段跳过；没命中再逐字折单字表；
//! 两张表都没有的字符原样搬运。词组表里既有「一目瞭然→一目了然」这种单字表折不出
//! 来的，也有「乾元→乾元」这种**故意不折**的——单字表会把「乾」折成「干」，词组表
//! 在这里的作用是拦住它。所以词组表允许恒等行，单字表不允许。
//! 表里只收 OpenCC 列在最前的那个简体候选（乾→干／乾），不做取舍。反方向
//! （简体→繁体）本来就是一对多，例如「干」对应「幹／乾」，本表不提供，也不做繁简
//! 之外的异体字转换。折叠只作用于查询串，绝不改动库里任何键。

mod data;

use data::{FOLD, PHRASE};

/// 把查询串里的繁体折成简体：词组优先，再单字，表里没有的字符原样返回。
pub fn fold_traditional(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut rest = query;
    while let Some(character) = rest.chars().next() {
        if let Some((traditional, simple)) = PHRASE
            .iter()
            .find(|(traditional, _)| rest.starts_with(traditional))
        {
            out.push_str(simple);
            rest = &rest[traditional.len()..];
            continue;
        }
        out.push(
            match FOLD.binary_search_by_key(&character, |&(traditional, _)| traditional) {
                Ok(index) => FOLD[index].1,
                Err(_) => character,
            },
        );
        rest = &rest[character.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_free_of_duplicates() {
        // 生成器已经排序，这里复核一次：乱序会让二分静默漏字。
        let codes: Vec<u32> = FOLD
            .iter()
            .map(|(traditional, _)| *traditional as u32)
            .collect();
        assert!(
            codes.windows(2).all(|pair| pair[0] < pair[1]),
            "FOLD 必须严格升序"
        );
        assert!(
            FOLD.iter()
                .all(|(traditional, simple)| traditional != simple),
            "不该有恒等映射"
        );
    }

    #[test]
    fn phrase_rows_may_be_identity() {
        // 键长的严格序就是最长匹配优先的依据；相邻严格有序同时排除了重复键。
        for pair in PHRASE.windows(2) {
            let (left, right) = (pair[0].0, pair[1].0);
            let (left_len, right_len) = (left.chars().count(), right.chars().count());
            assert!(
                left_len > right_len || (left_len == right_len && left < right),
                "PHRASE 必须按（字符数降序，键升序）排列"
            );
            assert!(left_len > 1, "单字键该由 FOLD 承担");
        }
        assert!(
            PHRASE
                .iter()
                .any(|(traditional, simple)| traditional == simple),
            "恒等行是本表存在的理由之一，一行都没有说明生成器漏了"
        );
    }

    #[test]
    fn folds_traditional_queries_and_leaves_the_rest_alone() {
        assert_eq!(fold_traditional("測試"), "测试");
        assert_eq!(fold_traditional("計算機"), "计算机");
        assert_eq!(fold_traditional("angle 角度"), "angle 角度");
    }

    #[test]
    fn a_traditional_char_with_several_candidates_takes_the_first() {
        // 乾 在表里是「干 乾」，取第一个；點 是干净的一对一。
        assert_eq!(fold_traditional("乾"), "干");
        assert_eq!(fold_traditional("一點"), "一点");
    }

    #[test]
    fn folds_multiword_traditional_phrases() {
        // 三条单字表逐字折都给不出：瞭 折完还是 瞭，擲 那条会变成「干坤一掷」，
        // 鍊 那条会变成「上炼」。
        assert_eq!(fold_traditional("一目瞭然"), "一目了然");
        assert_eq!(fold_traditional("乾坤一擲"), "乾坤一掷");
        assert_eq!(fold_traditional("上鍊"), "上链");
    }

    #[test]
    fn a_phrase_that_would_over_fold_stays_intact() {
        // 逐字折会把恒等的词组折坏：「乾元」变「干元」，「乾坤再造」变「干坤再造」。
        assert_eq!(fold_traditional("乾元"), "乾元");
        assert_eq!(fold_traditional("乾坤再造"), "乾坤再造");
        // 「乾乾淨淨」的词组值和逐字折恰好一致，留着它验证长键命中后按字符数前进。
        assert_eq!(fold_traditional("乾乾淨淨"), "干干净净");
    }
}
