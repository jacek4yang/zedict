import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "dict_shards", Path(__file__).resolve().parents[1] / "tools/dict_shards.py"
)
dict_shards = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dict_shards)


class ShardTests(unittest.TestCase):
    def test_split_and_join_are_byte_identical(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "all.jsonl"
            raw = (
                '{"headword": "love", "gloss": "爱"}\n'
                '{"headword": "苹果", "gloss": "apple"}\n'
                + '{"headword": "long", "gloss": "' + "x" * 400 + '"}\n'
                + '{"headword": "tail", "gloss": "no final newline"}'
            ).encode("utf-8")
            source.write_bytes(raw)
            out = root / "shards"
            manifest = dict_shards.split(source, out, 0.0002)
            self.assertGreater(len(manifest["parts"]), 1)
            self.assertEqual(manifest["lines"], 4)
            self.assertEqual(manifest["bytes"], len(raw))
            merged = root / "joined.jsonl"
            result = dict_shards.join(out, merged, False)
            self.assertEqual(merged.read_bytes(), raw)
            self.assertEqual(result["sha256"], manifest["sha256"])

    def test_a_tampered_shard_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "all.jsonl"
            source.write_text('{"headword": "a"}\n{"headword": "b"}\n', encoding="utf-8")
            out = root / "shards"
            dict_shards.split(source, out, 0.0001)
            part = sorted(out.glob("part-*.jsonl"))[0]
            part.write_bytes(part.read_bytes() + b"x")
            with self.assertRaises(SystemExit):
                dict_shards.join(out, root / "joined.jsonl", False)
            self.assertFalse((root / "joined.jsonl").exists())

    def test_manifest_cannot_escape_the_shard_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            out = root / "shards"
            out.mkdir()
            (out / "manifest.json").write_text(
                json.dumps(
                    {
                        "format": dict_shards.FORMAT,
                        "bytes": 0,
                        "sha256": "",
                        "lines": 0,
                        "parts": [{"name": "../escape.jsonl", "bytes": 0, "lines": 0, "sha256": ""}],
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaises(SystemExit):
                dict_shards.join(out, root / "joined.jsonl", False)


if __name__ == "__main__":
    unittest.main()
