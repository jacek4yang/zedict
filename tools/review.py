"""Read-only inspection or lossless fragment cataloging. Never approves candidates.

Usage: python tools/review.py inspect data/dict_full.jsonl 143946
       python tools/review.py fragments data/combined.inflated review-work/fragments.jsonl
       python tools/review.py inventory data/combined.inflated data/dict_full.jsonl
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re


def digest(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def inspect(path, number):
    with path.open("rb") as handle:
        for line_number, raw in enumerate(handle, 1):
            if line_number != number:
                continue
            raw = raw.rstrip(b"\r\n")
            record = json.loads(raw)
            # Examples use flattened indices in review manifests.
            examples = []
            for group in record.get("examples", []):
                for example in group.split("²"):
                    if "@" in example:
                        examples.append(example)
            print(json.dumps({"line": number, "record_sha256": hashlib.sha256(raw).hexdigest(),
                              "source": record, "flattened_examples": list(enumerate(examples))},
                             ensure_ascii=False, indent=2))
            return
    raise SystemExit("No such source line")


def fragments(path, target):
    """FC spans are fragments, NOT inferred entries. Preserve the first span at offset 0."""
    data = path.read_bytes()
    sha = hashlib.sha256(data).hexdigest()
    target.parent.mkdir(parents=True, exist_ok=True)
    spans = list(re.finditer(rb"<FC>(.*?)</FC>", data, re.S))
    # Exclusive creation protects previous catalogs and raw data.
    with target.open("x", encoding="utf-8", newline="\n") as out:
        for index, match in enumerate(spans):
            end = spans[index + 1].start() if index + 1 < len(spans) else len(data)
            record = {"offset": match.start(), "end": match.end(), "source_sha256": sha,
                      "status": "unresolved", "headword": None,
                      "senses": [match[1].decode("utf-8", errors="replace")],
                      "phon_uk": None, "phon_us": None, "english": None, "examples": [],
                      "unbound_following_markup": data[match.end():end].decode("utf-8", errors="replace"),
                      "raw_span_base64": base64.b64encode(data[match.start():end]).decode("ascii"),
                      "unbound_prefix_base64": base64.b64encode(data[:match.start()]).decode("ascii") if index == 0 else None}
            out.write(json.dumps(record, ensure_ascii=False) + "\n")
    print(json.dumps({"fragments": len(spans), "first_offset": spans[0].start() if spans else None,
                      "unmatched_open_tags": data.count(b"<FC>") - len(spans),
                      "source_sha256": sha, "warning": "Fragments are not complete dictionary entries"}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    show = commands.add_parser("inspect")
    show.add_argument("source", type=Path)
    show.add_argument("line", type=int)
    split = commands.add_parser("fragments")
    split.add_argument("source", type=Path)
    split.add_argument("target", type=Path)
    inventory = commands.add_parser("inventory")
    inventory.add_argument("sources", type=Path, nargs="+")
    args = parser.parse_args()
    if args.command == "inspect":
        inspect(args.source, args.line)
    elif args.command == "fragments":
        fragments(args.source, args.target)
    else:
        print(json.dumps([{"path": str(p), "bytes": p.stat().st_size, "sha256": digest(p)}
                          for p in args.sources], ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
