"""`tools/validate_import.py` 里那份词头分档的独立实现。

它必须和 `src/evidence.rs` 判得一模一样，否则"两侧各算一遍再比"就成了两边各错一遍。
所以这里钉的是判据本身：C 档只认例句高亮词、D 档只认中文释义、`english` 一个字都不
许读、整词边界只挡 ASCII 小写字母。
"""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "validate_import", Path(__file__).resolve().parents[1] / "tools/validate_import.py"
)
validate_import = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validate_import)

key_tier = validate_import.key_tier
accepts = validate_import.accepts


def record(headword=None, senses=(), english=None, examples=()):
    body = {}
    if headword is not None:
        body["headword"] = headword
    if senses:
        body["senses"] = list(senses)
    if english is not None:
        body["english"] = english
    if examples:
        body["examples"] = list(examples)
    return body


class TierTests(unittest.TestCase):
    def test_an_unmarked_english_side_mention_is_not_evidence(self):
        self.assertEqual(
            key_tier(record("sick", examples=["She was <span class=\"key\">sick</span>@她病了"])),
            "example-key",
        )
        self.assertEqual(
            key_tier(record("sick", examples=["She was sick@她病了"])), "unevidenced"
        )

    def test_the_english_definition_pool_never_attests_a_headword(self):
        # 真实形状：释义讲的是"气骨导间距"，english 讲的是隔壁词条的 angle。
        junk = record("angle", senses=["<i>n.</i> 气骨导间距"], english="the angle formed by the x-axis")
        self.assertEqual(key_tier(junk), "unevidenced")
        self.assertEqual(key_tier(record("angle", senses=["<i>n.</i> angle 角"])), "in-gloss")

    def test_whole_word_rule_blocks_letters_but_not_hyphens(self):
        self.assertEqual(key_tier(record("a", senses=["cab 出租车"])), "unevidenced")
        self.assertEqual(key_tier(record("gap", senses=["A-B gap 气骨导间距"])), "in-gloss")
        self.assertEqual(key_tier(record("A-B", senses=["A-B gap 气骨导间距"])), "in-gloss")

    def test_a_headword_with_nothing_to_classify_is_unevidenced(self):
        self.assertEqual(key_tier(record()), "unevidenced")
        self.assertEqual(key_tier(record("   ")), "unevidenced")

    def test_thresholds_only_ever_decide_what_is_indexed(self):
        for tier in ("example-key", "in-gloss", "unevidenced", "reviewed"):
            self.assertTrue(accepts("all", tier), tier)
        self.assertTrue(accepts("some", "in-gloss"))
        self.assertFalse(accepts("some", "unevidenced"))
        self.assertFalse(accepts("strong", "in-gloss"))
        self.assertTrue(accepts("strong", "reviewed"))


class HtmlTests(unittest.TestCase):
    def test_block_tags_become_spaces_and_a_stray_tag_opener_eats_the_rest(self):
        self.assertEqual(validate_import.strip_html("a<p>b</p>c"), "a b c")
        self.assertEqual(validate_import.strip_html("x<b"), "x")
        self.assertEqual(validate_import.extract_keys('<span class="key"><b>x</b></span>y'), ["x"])
        self.assertEqual(validate_import.extract_keys('<span class="key">x</span>'), ["x"])

    def test_an_unclosed_ljid_span_truncates_the_example_just_like_the_rust_scan(self):
        self.assertEqual(validate_import.remove_ljid('a<span class="ljid" x>b'), "a")

    def test_the_english_side_of_an_example_has_to_be_long_enough_to_be_one(self):
        # `en.len() >= 4` 量的是字节，所以短英文侧里的高亮词不算证据。
        self.assertEqual(validate_import.english_keys(['ab<span class="key">a</span>@甲']), set())
        self.assertEqual(
            validate_import.english_keys(['was ill <span class="key">sick</span>@她病了']),
            {"sick"},
        )


if __name__ == "__main__":
    unittest.main()
