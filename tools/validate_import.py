"""Verify every archived record and every indexed identity against a structured source.

This checks import fidelity, not the linguistic correctness of source definitions.

`--key-evidence` is checked by *recomputing* it here, in plain Python, from the raw
source lines. The dictionary is never asked whether it pruned the right records:
the tier rule below is an independent implementation of `src/evidence.rs`, and the
two results have to agree line by line.
"""
import argparse
import hashlib
import html
import json
from pathlib import Path
import re
import sqlite3
import unicodedata
import zlib


def normalize(word):
    return " ".join(unicodedata.normalize("NFC", word).replace("’", "'").replace("‘", "'").split()).lower()


BLOCKS = re.compile(r"(?i)</?(?:p|div|li|ul|ol|br|h[1-6])\b[^>]*>")


def strip_html(text):
    """`model::strip_html`: block tags become spaces, then a char scan that treats a
    stray `<` as opening a tag that never closes (so the rest of the string is gone)."""
    text = BLOCKS.sub(" ", text)
    out, in_tag = [], False
    for char in text:
        if char == "<":
            in_tag = True
        elif char == ">" and in_tag:
            in_tag = False
        elif not in_tag:
            out.append(char)
    return " ".join(html.unescape("".join(out)).split())


LJID_OPEN = '<span class="ljid"'
KEY_OPEN = '<span class="key">'
CLOSE = "</span>"


def remove_ljid(text):
    """`RegexLite::remove_ljid`, including its give-up-and-truncate-on-unclosed behaviour."""
    out = []
    while True:
        start = text.find(LJID_OPEN)
        if start < 0:
            out.append(text)
            return "".join(out)
        out.append(text[:start])
        end = text[start:].find(CLOSE)
        if end < 0:
            return "".join(out)
        text = text[start + end + len(CLOSE):]


def extract_keys(text):
    """`RegexLite::extract_keys`: inner HTML of each key span, stripped, empties dropped."""
    keys, rest = [], text
    while True:
        start = rest.find(KEY_OPEN)
        if start < 0:
            return keys
        rest = rest[start + len(KEY_OPEN):]
        end = rest.find(CLOSE)
        if end < 0:
            return keys
        key = strip_html(rest[:end])
        if key:
            keys.append(key)
        rest = rest[end + len(CLOSE):]


def english_keys(examples):
    """Highlighted words on the English side of every example `compile::example_sides` keeps."""
    keys = set()
    for segment in examples:
        for chunk in remove_ljid(segment).split("²"):
            chunk = chunk.strip()
            at = chunk.find("@")
            if at < 0:
                continue
            en, zh = strip_html(chunk[:at]), strip_html(chunk[at + 1:])
            if len(en.encode()) >= 4 and zh:
                keys.update(normalize(key) for key in extract_keys(chunk[:at]) if normalize(key))
    return keys


_MENTIONS = {}


def mentions(needle, haystack):
    """Whole-word containment. Only ASCII lowercase letters block a boundary, so `a`
    never matches inside `cab` while `ab` still matches inside `A-B gap`."""
    if not needle:
        return False
    pattern = _MENTIONS.setdefault(needle, re.compile(r"(?<![a-z])" + re.escape(needle) + r"(?![a-z])"))
    return pattern.search(haystack) is not None


def key_tier(record):
    """`evidence::key_tier`: only the record's own example highlights (C) and its own
    Chinese glosses (D) count. `english` is deliberately unread — it is an English
    definition pool that describes the neighbouring entry, so letting it attest a
    headword would let a wrong card stamp itself valid."""
    headword = normalize((record.get("headword") or "").strip())
    if not headword:
        return "unevidenced"
    examples = [x for x in record.get("examples") or [] if isinstance(x, str)]
    if headword in english_keys(examples):
        return "example-key"
    senses = [x for x in record.get("senses") or [] if isinstance(x, str)]
    gloss = normalize(" ".join(strip_html(sense) for sense in senses))
    if mentions(headword, gloss):
        return "in-gloss"
    return "unevidenced"


TIERS = {"example-key": 0, "in-gloss": 1, "unevidenced": 2, "reviewed": 3}


def accepts(threshold, tier):
    if threshold == "all":
        return True
    if threshold == "some":
        return tier != "unevidenced"
    return tier in ("example-key", "reviewed")


def validate(source, database, corrections=None):
    source_digest = hashlib.file_digest(source.open("rb"), "sha256").hexdigest()
    overrides = {}
    if corrections:
        manifest = json.loads(corrections.read_text(encoding="utf-8"))
        assert manifest["source_sha256"] == source_digest, "Correction source hash mismatch"
        overrides = {r["line"]: r for r in manifest["reviews"]}
    conn = sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)
    assert conn.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
    report = json.loads(conn.execute("SELECT value FROM meta WHERE key='report'").fetchone()[0])
    assert report["source_sha256"] == source_digest
    # Older artifacts predate the switch; a missing field means "nothing was pruned".
    threshold = report.get("key_evidence") or "all"
    assert threshold in ("all", "some", "strong"), f"Unknown key_evidence threshold: {threshold}"
    archived = iter(conn.execute("SELECT line,record_sha256,raw FROM source_records ORDER BY line"))
    expected = {}
    tiers = {}
    hashes = {}
    count = 0
    with source.open("rb") as handle:
        for count, line in enumerate(handle, 1):
            raw = line.rstrip(b"\r\n")
            number, digest, packed = next(archived)
            assert number == count and zlib.decompress(packed) == raw, f"Archive mismatch at {count}"
            hashes[count] = hashlib.sha256(raw).hexdigest()
            assert digest == hashes[count]
            record = json.loads(raw)
            correction = overrides.get(count)
            if correction:
                assert correction["record_sha256"] == digest
                if correction["status"] != "verified":
                    continue
                headword = correction["headword"]
            else:
                headword = record.get("headword") or ""
            if not headword.strip() or len(normalize(headword)) > 256 or any(unicodedata.category(c) == "Cc" for c in headword):
                continue
            expected[count] = normalize(headword)
            # 人工签过的记录由人负责，不参与自动判据；其余按源行独立复算一遍档位。
            tiers[count] = "reviewed" if correction else key_tier(record)
    assert next(archived, None) is None
    dropped = {number for number, tier in tiers.items() if not accepts(threshold, tier)}
    if threshold != "all":
        expected = {n: key for n, key in expected.items() if n not in dropped}
    stored = conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='key_evidence'"
    ).fetchone()
    assert (stored is not None) == (threshold != "all"), (
        f"阈值 {threshold} 与 {database.name} 里的 key_evidence 表是否相符"
    )
    if threshold != "all":
        table = dict(conn.execute("SELECT line, tier FROM key_evidence"))
        assert table == {n: TIERS[t] for n, t in tiers.items()}, (
            "词头证据档位与独立复算不一致: "
            f"{sorted(set(table.items()) ^ {(n, TIERS[t]) for n, t in tiers.items()})[:5]}"
        )
        for name, tier in (("tier_example_key", "example-key"), ("tier_in_gloss", "in-gloss"),
                           ("tier_unevidenced", "unevidenced")):
            assert report[name] == sum(1 for t in tiers.values() if t == tier), name
    assert report.get("dropped_unevidenced_cards", 0) == len(dropped)
    actual = {}
    for number, key, payload in conn.execute("SELECT id,key,card FROM entries ORDER BY id"):
        assert int.from_bytes(payload[:4], "little") == len(payload) - 4
        card = json.loads(zlib.decompress(payload[4:]))
        assert normalize(card["headword"]) == key
        assert card["source"]["line"] == number
        assert card["source"]["record_sha256"] == hashes[number]
        actual[number] = key
    assert actual == expected, "Missing, extra, or incorrectly indexed source records"
    for number, correction in overrides.items():
        if correction["status"] != "verified":
            continue
        key = normalize(correction["headword"])
        selected = conn.execute("SELECT id FROM entries WHERE key=? AND priority=(SELECT max(priority) FROM entries WHERE key=?)", (key, key)).fetchall()
        assert (number,) in selected, f"Correction not selected: {key}"
    assert report["archived_records"] == count
    assert report["indexed_cards"] == len(actual)
    assert report["indexed_headwords"] == len(set(actual.values()))
    conn.close()
    return {"status": "ok", "source_sha256": source_digest,
            "all_archived_records_checked": count, "all_indexed_cards_checked": len(actual),
            "distinct_headwords": len(set(actual.values())), "corrections_checked": len(overrides),
            "key_evidence": threshold,
            "key_evidence_tiers": {tier: sum(1 for t in tiers.values() if t == tier)
                                   for tier in TIERS},
            "key_evidence_dropped": len(dropped),
            "scope": "byte-exact archive, complete index, card identities, correction "
                     "precedence, independently recomputed headword evidence"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("database", type=Path)
    parser.add_argument("--corrections", type=Path)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    result = json.dumps(validate(args.source, args.database, args.corrections), ensure_ascii=False, indent=2)
    if args.out:
        args.out.write_text(result + "\n", encoding="utf-8")
    print(result)
