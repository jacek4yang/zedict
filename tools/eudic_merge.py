#!/usr/bin/env python3
"""Merge Eudic phrase.bin records into the combined.bin JSONL.

phrase.bin (111,302 entries, see tools/eudic_phrase.py) overlaps the combined
headword set by 17,309 normalized keys.  Most of those overlapping bodies are
the same text combined.bin already carries, so senses are appended only when
neither the flattened nor the punctuation/space-folded form is already present
in the card.  Every remaining phrase entry becomes a new card whose key does
not exist in the combined set; it is appended after the combined records.

The output keeps one line per card in zedict's RawEntry format.  Unchanged
combined lines are copied byte-for-byte.  Derived from copyrighted content;
keep data/ outside version control.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import html
import json
import os
import re
import sys
import unicodedata
from pathlib import Path

FORMAT = "zedict-eudic-merged-jsonl"
FORMAT_VERSION = 1

TAG_RE = re.compile(r"<[^>]*>")
NOISE_RE = re.compile(r"[\s,，;；、]+")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def normalize(word: str) -> str:
    """Mirror zedict's store::normalize (NFC, apostrophes, spaces, lowercase).

    Rust lowercases char by char, so capital sigma folds to sigma rather than
    to Python's context-sensitive final sigma.
    """

    value = unicodedata.normalize("NFC", word).replace("’", "'").replace("‘", "'")
    value = value.replace("Σ", "σ")
    return " ".join(value.split()).lower()


def flatten(sense: str) -> str:
    return " ".join(html.unescape(TAG_RE.sub("", sense)).split())


def compact(sense: str) -> str:
    return NOISE_RE.sub("", flatten(sense)).lower()


def load_phrases(path: Path) -> tuple[list[tuple[str, str, list[str]]], dict[str, int]]:
    records: list[tuple[str, str, list[str]]] = []
    by_key: dict[str, int] = {}
    with open(path, encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, 1):
            if not line.strip():
                raise SystemExit(f"{path}:{line_no}: blank line")
            record = json.loads(line)
            headword = record.get("headword")
            senses = record.get("senses")
            if not isinstance(headword, str) or not headword.strip():
                raise SystemExit(f"{path}:{line_no}: missing headword")
            if not isinstance(senses, list) or not senses:
                raise SystemExit(f"{path}:{line_no}: missing senses")
            if any(not isinstance(s, str) or not s.strip() for s in senses):
                raise SystemExit(f"{path}:{line_no}: empty sense")
            key = normalize(headword)
            if key in by_key:
                raise SystemExit(f"{path}:{line_no}: duplicate normalized key {key!r}")
            by_key[key] = len(records)
            records.append((key, headword, senses))
    return records, by_key


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--combined", type=Path, default=Path("data/eudic/all.jsonl"))
    parser.add_argument("--phrase", type=Path, default=Path("data/eudic-phrase/phrase.jsonl"))
    parser.add_argument("--out", type=Path, default=Path("data/eudic-merged/all.jsonl"))
    parser.add_argument("--report", type=Path, default=None)
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    report_path = args.report or args.out.parent / "manifest.json"
    args.out.parent.mkdir(parents=True, exist_ok=True)
    phrases, by_key = load_phrases(args.phrase)

    combined_keys: set[str] = set()
    matched_keys: set[str] = set()
    combined_records = 0
    matched_records = 0
    changed_records = 0
    added_senses = 0
    skipped_senses = 0

    tmp = args.out.with_name(args.out.name + ".tmp")
    try:
        with open(args.combined, encoding="utf-8") as src, open(tmp, "w", encoding="utf-8", newline="\n") as dst:
            for line_no, line in enumerate(src, 1):
                raw = line.rstrip("\n")
                if not raw.strip():
                    raise SystemExit(f"{args.combined}:{line_no}: blank line")
                record = json.loads(raw)
                headword = record.get("headword")
                if not isinstance(headword, str) or not headword.strip():
                    raise SystemExit(f"{args.combined}:{line_no}: missing headword")
                key = normalize(headword)
                combined_keys.add(key)
                combined_records += 1

                index = by_key.get(key)
                if index is None:
                    dst.write(raw + "\n")
                    continue

                matched_keys.add(key)
                matched_records += 1
                senses = record.get("senses")
                if not isinstance(senses, list):
                    raise SystemExit(f"{args.combined}:{line_no}: senses is not a list")
                existing_flat = [flatten(s) for s in senses]
                existing_compact = [compact(s) for s in existing_flat]
                additions: list[str] = []
                for sense in phrases[index][2]:
                    flat = flatten(sense)
                    folded = compact(flat)
                    if not flat:
                        continue
                    if flat in existing_flat or any(folded in seen for seen in existing_compact):
                        skipped_senses += 1
                        continue
                    additions.append(sense)
                    existing_flat.append(flat)
                    existing_compact.append(folded)
                if additions:
                    record["senses"] = senses + additions
                    dst.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n")
                    changed_records += 1
                    added_senses += len(additions)
                else:
                    dst.write(raw + "\n")
                if not args.quiet and combined_records % 100_000 == 0:
                    print(f"combined {combined_records} records, {added_senses} senses added", file=sys.stderr)

            phrase_only_records = 0
            phrase_only_senses = 0
            for key, headword, senses in phrases:
                if key in matched_keys:
                    continue
                if key in combined_keys:
                    raise SystemExit(f"phrase key {key!r} was skipped but exists in combined")
                dst.write(
                    json.dumps({"headword": headword, "senses": senses}, ensure_ascii=False, separators=(",", ":"))
                    + "\n"
                )
                phrase_only_records += 1
                phrase_only_senses += len(senses)
        os.replace(tmp, args.out)
    finally:
        if tmp.exists():
            tmp.unlink()

    # Verification pass: every phrase key must be covered exactly once, and the
    # output may not introduce normalized collisions beyond the combined set.
    output_records = 0
    output_keys: dict[str, int] = {}
    phrase_only_seen = 0
    combined_seen = 0
    with open(args.out, encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, 1):
            record = json.loads(line)
            headword = record.get("headword")
            if not isinstance(headword, str) or not headword.strip():
                raise SystemExit(f"{args.out}:{line_no}: missing headword")
            key = normalize(headword)
            output_keys[key] = output_keys.get(key, 0) + 1
            output_records += 1
            if combined_seen < combined_records:
                combined_seen += 1
            else:
                phrase_only_seen += 1
                if key in combined_keys:
                    raise SystemExit(f"{args.out}:{line_no}: phrase-only key {key!r} already in combined")
                if key not in by_key:
                    raise SystemExit(f"{args.out}:{line_no}: unexpected phrase-only key {key!r}")
    if output_records != combined_records + phrase_only_records:
        raise SystemExit(f"{args.out}: {output_records} rows, expected {combined_records + phrase_only_records}")
    if phrase_only_seen != phrase_only_records:
        raise SystemExit(f"{args.out}: {phrase_only_seen} phrase-only rows, expected {phrase_only_records}")
    collisions = sum(1 for count in output_keys.values() if count > 1)
    extra_rows = sum(count - 1 for count in output_keys.values() if count > 1)

    report = {
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "generator": "tools/eudic_merge.py",
        "created_utc": _dt.datetime.now(_dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z"),
        "inputs": {
            "combined": {
                "path": str(args.combined),
                "records": combined_records,
                "bytes": args.combined.stat().st_size,
                "sha256": sha256_file(args.combined),
            },
            "phrase": {
                "path": str(args.phrase),
                "records": len(phrases),
                "bytes": args.phrase.stat().st_size,
                "sha256": sha256_file(args.phrase),
            },
        },
        "merge": {
            "overlap_keys": len(matched_keys),
            "overlap_combined_records": matched_records,
            "records_with_added_senses": changed_records,
            "added_senses": added_senses,
            "skipped_duplicate_senses": skipped_senses,
            "phrase_only_records": phrase_only_records,
            "phrase_only_senses": phrase_only_senses,
        },
        "output": {
            "path": str(args.out),
            "records": output_records,
            "bytes": args.out.stat().st_size,
            "sha256": sha256_file(args.out),
        },
        "checks": {
            "unmatched_phrase_keys": len(phrases) - len(matched_keys) - phrase_only_records,
            "normalized_collision_keys": collisions,
            "duplicate_key_extra_rows": extra_rows,
        },
    }
    report_path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if not args.quiet:
        print(
            f"combined {combined_records} + phrase-only {phrase_only_records} = {output_records} records; "
            f"overlap {len(matched_keys)} keys, {added_senses} senses added, {skipped_senses} duplicates skipped",
            file=sys.stderr,
        )
        print(json.dumps(report["checks"], ensure_ascii=False), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
