"""Traditional->Simplified fold tables for query normalisation.

The dictionary's Chinese definitions are simplified, so a traditional query
matches nothing: the reverse index folds the *query string* first. The library's
own keys stay untouched, so no rebuild is needed to gain this.

Two OpenCC dictionaries (Apache-2.0) feed one generated file, both read from the
locally installed python package. The name encodes the direction: T->S, i.e.
traditional key, simplified value; the ``ST*`` files are the reverse and are
useless here.

``TSCharacters.txt`` -- single characters. A row is kept when

  * the traditional key is exactly one character;
  * the first simplified candidate is exactly one character;
  * the two characters differ (identity pairs carry no information).

``TSPhrases.txt`` -- multi-character phrases, tried before the character table
so a phrase can also *refuse* a fold the characters alone would make. A row is
kept when

  * the traditional key is at least two characters (single-character keys are
    the character table's job);
  * there is a simplified candidate.

  Identity pairs are kept here on purpose, unlike in the character table:
  乾元 -> 乾元 is in the file precisely to stop 乾 -> 干 from mangling it.
  Keys run 2..14 characters long.

56 of the 4113 character rows and 4 of the 277 phrase rows list several
simplified candidates separated by whitespace or commas (乾 -> "干 乾",
剋 -> "克 剋"). The first is taken and the rest dropped, because
choosing among them would mean guessing a reading the query never stated. The
simplified->traditional direction is genuinely one-to-many (干 <- 幹/乾) and is
deliberately not modelled, and neither is variant characters beyond 繁简.

``--stats`` also measures what the phrase table is worth against the character
table alone: of the 216 non-identity phrase pairs, 205 are not reproducible by
folding character by character, and 53 of the 61 identity pairs would be mangled
by that character-only fold.

Usage: python tools/variant_dump.py [--out src/variant.rs] [--stats]
"""

import argparse
import hashlib
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src/variant.rs"
CHARS_DICTIONARY = "TSCharacters.txt"
PHRASES_DICTIONARY = "TSPhrases.txt"


def dictionary_path(name: str) -> Path:
    try:
        import opencc
    except ImportError as exc:
        raise SystemExit(
            "需要本机的 OpenCC 词典（pip install opencc，只在生成时用到；"
            "生成结果 src/variant.rs 随仓库维护，构建不依赖它）"
        ) from exc
    return Path(os.path.dirname(opencc.__file__)) / "dictionary" / name


def candidates(value: str):
    """OpenCC separates several simplified candidates by whitespace or commas."""
    return [part for part in re.split(r"[,\s]+", value.strip()) if part]


def fold_with_chars(pairs: dict, text: str) -> str:
    """Character-by-character fold, for measuring what the phrase table adds."""
    return "".join(pairs.get(character, character) for character in text)


def load_pairs(path: Path):
    """Return ({traditional: simplified}, rows read, rows skipped, rows with choices)."""
    kept = {}
    rows = skipped = multi = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        rows += 1
        fields = line.split("\t")
        if len(fields) != 2 or len(fields[0]) != 1:
            skipped += 1
            continue
        traditional, value = fields
        options = candidates(value)
        if len(options) > 1:
            multi += 1
        first = options[0] if options else ""
        if len(first) != 1 or first == traditional:
            skipped += 1
            continue
        if kept.get(traditional, first) != first:
            sys.exit(f"词典自相矛盾：U+{ord(traditional):04X} 有两个简体值")
        kept[traditional] = first
    return kept, rows, skipped, multi


def load_phrases(path: Path):
    """Return ({traditional: simplified}, rows read, rows skipped, rows with choices).

    Identity rows survive on purpose: they are the over-fold guards.
    """
    kept = {}
    rows = skipped = multi = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        rows += 1
        fields = line.split("\t")
        if len(fields) != 2 or len(fields[0]) < 2:
            skipped += 1
            continue
        traditional, value = fields
        options = candidates(value)
        if len(options) > 1:
            multi += 1
        first = options[0] if options else ""
        if not first:
            skipped += 1
            continue
        if any(mark in traditional + first for mark in ('"', "\\")):
            # 词组要写成 Rust 字符串字面量，引号/反斜杠得转义；词典里一个都没有，
            # 与其生成转义串不如拒绝，免得产物悄悄坏掉。
            sys.exit(f"词典含引号或反斜杠，无法生成字面量：{traditional}")
        if kept.get(traditional, first) != first:
            sys.exit(f"词典自相矛盾：{traditional} 有两个简体值")
        kept[traditional] = first
    return kept, rows, skipped, multi


HEADER = """//! 繁体→简体折叠表；由 `tools/variant_dump.py` 生成，请勿手改。
//!
//! 数据源：OpenCC `TSCharacters.txt`（Apache-2.0），sha256 = `{characters_digest}`；
//! `TSPhrases.txt`（Apache-2.0），sha256 = `{phrases_digest}`。
//!
//! 词库的释义是简体，反向索引又按单字建，所以繁体查询要先折成简体才可能命中。
//! 折叠分两层：先试词组表，命中就用词组的简体值并整段跳过；没命中再逐字折单字表；
//! 两张表都没有的字符原样搬运。词组表里既有「一目瞭然→一目了然」这种单字表折不出
//! 来的，也有「乾元→乾元」这种**故意不折**的——单字表会把「乾」折成「干」，词组表
//! 在这里的作用是拦住它。所以词组表允许恒等行，单字表不允许。
//! 表里只收 OpenCC 列在最前的那个简体候选（乾→干／乾），不做取舍。反方向
//! （简体→繁体）本来就是一对多，例如「干」对应「幹／乾」，本表不提供，也不做繁简
//! 之外的异体字转换。折叠只作用于查询串，绝不改动库里任何键。

/// 按繁体码位升序排列，供 `binary_search_by_key` 使用。
/// 用 `static` 而非 `const`：三十多 KB 的表若声明成 `const`，每一处使用都会重新内联一份。
/// `rustfmt::skip`：本文件是生成物，跳过格式才能让产物一次成型——否则 rustfmt 会把长
/// 词条折成四行，重新生成就和 `cargo fmt --check` 打一架。
#[rustfmt::skip]
static FOLD: [(char, char); {characters}] = [
"""

MIDDLE = """];

/// 按**键的字符数降序**、同长按键升序排列：折叠时第一次 `starts_with` 命中就是最长命中，
/// 不需要按长度来回试。键长 2 字起（单字键归 `FOLD` 管），恒等行是有意的过度折叠防线。
/// 同样用 `static`；几百行对查询串做线性前缀扫描，比较在头一个字节就失败，可忽略。
#[rustfmt::skip]
static PHRASE: [(&str, &str); {phrases}] = [
"""

FOOTER = """];

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
"""


def render(char_pairs: dict, phrase_pairs: dict, characters: Path, phrases: Path) -> str:
    rows = "".join(
        f"    ('{traditional}', '{simple}'),\n"
        for traditional, simple in sorted(char_pairs.items())
    )
    phrase_rows = "".join(
        f'    ("{traditional}", "{simple}"),\n'
        for traditional, simple in sorted(
            phrase_pairs.items(), key=lambda pair: (-len(pair[0]), pair[0])
        )
    )
    return (
        HEADER.format(
            characters_digest=hashlib.sha256(characters.read_bytes()).hexdigest(),
            phrases_digest=hashlib.sha256(phrases.read_bytes()).hexdigest(),
            characters=len(char_pairs),
        )
        + rows
        + MIDDLE.format(phrases=len(phrase_pairs))
        + phrase_rows
        + FOOTER
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--stats", action="store_true")
    args = parser.parse_args()

    characters = dictionary_path(CHARS_DICTIONARY)
    phrases = dictionary_path(PHRASES_DICTIONARY)
    char_pairs, char_rows, char_skipped, char_multi = load_pairs(characters)
    phrase_pairs, phrase_rows, phrase_skipped, phrase_multi = load_phrases(phrases)
    if args.stats:
        identity = [key for key, value in phrase_pairs.items() if key == value]
        folded = [key for key, value in phrase_pairs.items() if key != value]
        # 净增益：单字表逐字折给不出词组值的对数；防过度折叠：单字表会折坏的恒等对数。
        gain = sum(
            1
            for key, value in phrase_pairs.items()
            if key != value and fold_with_chars(char_pairs, key) != value
        )
        guards = sum(
            1 for key in identity if fold_with_chars(char_pairs, key) != key
        )
        print(
            f"{CHARS_DICTIONARY} rows={char_rows} kept={len(char_pairs)} "
            f"skipped={char_skipped} multi_candidate={char_multi}",
            file=sys.stderr,
        )
        print(
            f"{PHRASES_DICTIONARY} rows={phrase_rows} kept={len(phrase_pairs)} "
            f"skipped={phrase_skipped} multi_candidate={phrase_multi} "
            f"identity={len(identity)} folded={len(folded)} "
            f"gain_over_chars={gain} over_fold_guards={guards}",
            file=sys.stderr,
        )
    args.out.write_text(
        render(char_pairs, phrase_pairs, characters, phrases),
        encoding="utf-8",
        # .gitattributes 要求 *.rs 用 LF；Python 在 Windows 上默认会写成 CRLF，
        # 那样 cargo fmt 会和生成器互相改来改去。
        newline="\n",
    )
    print(
        f"wrote {args.out} ({len(char_pairs)} characters, {len(phrase_pairs)} phrases)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
