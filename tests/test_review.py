import base64
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("review", Path(__file__).resolve().parents[1] / "tools/review.py")
review = importlib.util.module_from_spec(spec)
spec.loader.exec_module(review)


class FragmentTests(unittest.TestCase):
    def test_first_fragment_and_invalid_utf8_remain_unbound_and_lossless(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.bin"
            target = Path(directory) / "fragments.jsonl"
            raw = b"<FC>first</FC><phon>x</phon><FC>\xff</FC>trailer"
            source.write_bytes(raw)
            with contextlib.redirect_stdout(io.StringIO()):
                review.fragments(source, target)
            records = [json.loads(line) for line in target.read_text(encoding="utf-8").splitlines()]
            self.assertEqual(records[0]["offset"], 0)
            self.assertEqual(len(records), 2)
            self.assertTrue(all(r["status"] == "unresolved" and r["headword"] is None for r in records))
            self.assertEqual(b"".join(base64.b64decode(r["raw_span_base64"]) for r in records), raw)
            with self.assertRaises(FileExistsError):
                review.fragments(source, target)


if __name__ == "__main__":
    unittest.main()
