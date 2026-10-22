#!/usr/bin/env python3
"""Convert Eudic's phrase.bin into zedict JSONL.

Container layout (validated against the 4,996,687-byte shipping file):

    0 .. 1186          header (zlib page table starts at 1186)
    1186 .. 3322       page offset table, 267 x u64
    3323 .. 1482590    zlib pages, 16 KiB each, 4,369,855 bytes inflated
    1482590 .. 3263422 record table, 111,302 x (headword_off, text_off, 0, len)
    3263422 .. 3263430 u64 headword buffer size (1,730,449)
    3263430 .. 4993879 concatenated headwords, no separators
    4993879 .. 4996687 trailer, 702 x u32

Every record's (text_off, len) interval is checked: the 111,302 intervals
must tile the inflated text with zero gaps and zero overlaps. Headwords are
read from the exact byte range given by the record table.

The output is a lossless, project-format projection. Derived from copyrighted
content; keep data/ outside version control.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import html
import re
import struct
import sys
import unicodedata
import zlib
from pathlib import Path

FORMAT = "zedict-eudic-phrase-jsonl"
FORMAT_VERSION = 1

PAGE_TABLE_OFFSET = 1_186
PAGE_COUNT = 267
DATA_OFFSET = 3_323
INFLATED_SIZE = 4_369_855
RECORD_TABLE_OFFSET = 1_482_590
RECORD_COUNT = 111_302
HEADWORD_SIZE_OFFSET = RECORD_TABLE_OFFSET + RECORD_COUNT * 16
HEADWORD_BUFFER_OFFSET = 3_263_430
TRAILER_OFFSET = 4_993_879
TRAILER_COUNT = 702
EXPECTED_SIZE = TRAILER_OFFSET + TRAILER_COUNT * 4

I_RE = re.compile(r"<i>(.*?)</i>", re.S | re.I)
OL_RE = re.compile(r"<ol>(.*?)</ol>", re.S | re.I)
LI_RE = re.compile(r"<li>(.*?)</li>", re.S | re.I)
BR_RE = re.compile(r"<br\s*/?>", re.I)
DIV_RE = re.compile(r"</?div[^>]*>", re.I)
HAN_TAG_RE = re.compile(r"<([\u3400-\u9fff]{1,4})>")
POS_RE = re.compile(r"^[A-Za-z]+\.(?:[,，]\s*[A-Za-z]+\.)*$")
LATIN_RE = re.compile(r"^[A-Za-z][A-Za-z' .\-()]*$")
SEPARATOR = "\x00"
# One record ("abductor hallucis") embeds a glyph placeholder twice; the
# English headword and the surrounding 展肌/收肌 identify the glyph as 拇.
GLYPH_PLACEHOLDERS = {"<&5&>": "拇"}


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def normalize(word: str) -> str:
    value = unicodedata.normalize("NFC", word).replace("’", "'").replace("‘", "'")
    return " ".join(value.split()).lower()


def load_pages(source: Path, inflated_path: Path | None) -> bytes:
    if inflated_path is not None:
        data = inflated_path.read_bytes()
        if len(data) != INFLATED_SIZE:
            raise SystemExit(f"{inflated_path}: inflated size {len(data)} != {INFLATED_SIZE}")
        return data
    raw = source.read_bytes()
    if len(raw) != EXPECTED_SIZE:
        raise SystemExit(f"{source}: size {len(raw)} != expected {EXPECTED_SIZE}")
    offsets = struct.unpack_from("<" + "Q" * PAGE_COUNT, raw, PAGE_TABLE_OFFSET)
    if offsets[0] != DATA_OFFSET:
        raise SystemExit(f"phrase.bin: first page offset is {offsets[0]}, expected {DATA_OFFSET}")
    if any(a >= b for a, b in zip(offsets, offsets[1:])):
        raise SystemExit("phrase.bin: page offsets are not strictly ascending")
    if offsets[-1] >= RECORD_TABLE_OFFSET:
        raise SystemExit("phrase.bin: page offsets overlap the record table")
    pages = []
    cursor = DATA_OFFSET
    for index, offset in enumerate(offsets):
        if offset != cursor:
            raise SystemExit(f"phrase.bin: page {index} starts at {offset}, expected {cursor}")
        end = offsets[index + 1] if index + 1 < PAGE_COUNT else RECORD_TABLE_OFFSET
        chunk = raw[offset:end]
        try:
            page = zlib.decompress(chunk)
        except zlib.error as error:
            raise SystemExit(f"phrase.bin: page {index} is not zlib: {error}")
        if len(page) != 16_384 and index + 1 < PAGE_COUNT:
            raise SystemExit(f"phrase.bin: page {index} inflated to {len(page)} bytes")
        pages.append(page)
        cursor = end
    text = b"".join(pages)
    if len(text) != INFLATED_SIZE:
        raise SystemExit(f"phrase.bin: inflated size {len(text)} != {INFLATED_SIZE}")
    if cursor != RECORD_TABLE_OFFSET:
        raise SystemExit(f"phrase.bin: pages end at {cursor}, expected {RECORD_TABLE_OFFSET}")
    return text


def load_records(raw: bytes, text: bytes) -> tuple[list[tuple[int, int, int, int]], bytes, int]:
    table_end = HEADWORD_SIZE_OFFSET
    if table_end + 8 > len(raw):
        raise SystemExit("phrase.bin: record table is truncated")
    records = [struct.unpack_from("<4I", raw, RECORD_TABLE_OFFSET + i * 16) for i in range(RECORD_COUNT)]
    if any(c != 0 for _, _, c, _ in records):
        raise SystemExit("phrase.bin: record table has non-zero reserved field")
    if any(records[i][0] > records[i + 1][0] for i in range(RECORD_COUNT - 1)):
        raise SystemExit("phrase.bin: headword offsets are not nondecreasing")

    intervals = sorted((b, d) for _, b, _, d in records)
    cursor = 0
    for start, length in intervals:
        if start != cursor:
            raise SystemExit(f"phrase.bin: text intervals do not tile at {cursor} (next {start})")
        cursor += length
    if cursor != len(text):
        raise SystemExit(f"phrase.bin: text intervals end at {cursor}, expected {len(text)}")

    buffer_size = struct.unpack_from("<Q", raw, HEADWORD_SIZE_OFFSET)[0]
    buffer_end = HEADWORD_BUFFER_OFFSET + buffer_size
    if buffer_end != TRAILER_OFFSET:
        raise SystemExit(
            f"phrase.bin: headword buffer ends at {buffer_end}, expected {TRAILER_OFFSET}"
        )
    if TRAILER_OFFSET + TRAILER_COUNT * 4 != len(raw):
        raise SystemExit("phrase.bin: trailer size does not match the file")
    trailer = struct.unpack_from("<" + "I" * TRAILER_COUNT, raw, TRAILER_OFFSET)
    if max(trailer) != RECORD_COUNT:
        raise SystemExit(f"phrase.bin: unexpected trailer maximum {max(trailer)}")
    return records, raw[HEADWORD_BUFFER_OFFSET:buffer_end], buffer_size


def body_senses(headword: str, body: str) -> list[str]:
    """Split a phrase body into senses.

    Layout observed in all 111,302 records: an optional ``<div class="exp">``
    gloss, ``<br>``-separated alternative glosses, and optional ``<ol><li>``
    lists.  ``<i>`` either marks a POS label (4,671) or repeats the headword
    in display capitalization (666); the latter is dropped, never emitted as
    a translation.
    """

    def i_tag(match: re.Match[str]) -> str:
        inner = " ".join(html.unescape(match.group(1)).split())
        if not inner:
            return ""
        if POS_RE.match(inner):
            following = body[match.end() : match.end() + 1]
            if following and not following.isspace():
                return inner + " "
            return inner
        if LATIN_RE.match(inner) and inner.casefold() == headword.casefold():
            return ""
        return inner

    fragment = I_RE.sub(i_tag, body)
    fragment = OL_RE.sub(
        lambda m: SEPARATOR + SEPARATOR.join(LI_RE.findall(m.group(1))) + SEPARATOR,
        fragment,
    )
    fragment = BR_RE.sub(SEPARATOR, fragment)
    fragment = DIV_RE.sub("", fragment)
    fragment = HAN_TAG_RE.sub(lambda m: f"[{m.group(1)}]", fragment)
    for placeholder, character in GLYPH_PLACEHOLDERS.items():
        fragment = fragment.replace(placeholder, character)
    senses: list[str] = []
    for part in fragment.split(SEPARATOR):
        cleaned = " ".join(html.unescape(part).split())
        if cleaned and cleaned not in senses:
            senses.append(cleaned)
    return senses


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--source", type=Path, required=True, help="path to dic/phrase.bin")
    parser.add_argument("--inflated", type=Path, default=None, help="reuse data/phrase.inflated")
    parser.add_argument("--out", type=Path, default=Path("data/eudic-phrase"), help="output directory")
    parser.add_argument("--limit", type=int, default=None, help="debug: stop after N records")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    text = load_pages(args.source, args.inflated)
    raw = args.source.read_bytes()
    records, buffer, buffer_size = load_records(raw, text)
    if not args.quiet:
        print(f"index {RECORD_COUNT} rows, headword buffer {buffer_size} bytes", file=sys.stderr)

    total = args.limit if args.limit is not None else RECORD_COUNT
    path = out / "phrase.jsonl"
    empty_senses = 0
    total_senses = 0
    pos_senses = 0
    region_labels = 0
    list_senses = 0
    markup_senses = 0
    seen: dict[str, str] = {}
    collisions = 0
    count = 0
    with open(path, "wb") as handle:
        for index in range(total):
            head_off, text_off, _, length = records[index]
            head_end = records[index + 1][0] if index + 1 < RECORD_COUNT else buffer_size
            try:
                headword = buffer[head_off:head_end].decode("utf-8", "strict")
            except UnicodeDecodeError as error:
                raise SystemExit(f"row {index}: headword is not UTF-8: {error}")
            if not headword or "\x00" in headword or headword != headword.strip():
                raise SystemExit(f"row {index}: invalid headword {headword!r}")
            if any(ord(ch) < 0x20 for ch in headword):
                raise SystemExit(f"row {index}: control character in headword")
            try:
                body = text[text_off:text_off + length].decode("utf-8", "strict")
            except UnicodeDecodeError as error:
                raise SystemExit(f"row {index}: body is not UTF-8: {error}")
            senses = body_senses(headword, body)
            if not senses:
                empty_senses += 1
            total_senses += len(senses)
            if any(POS_RE.match(s.split(" ", 1)[0]) for s in senses):
                pos_senses += 1
            if any("[" in s and "]" in s for s in senses):
                region_labels += 1
            if len(senses) > 1 and "<li>" in body:
                list_senses += 1
            if any("<" in s and ">" in s for s in senses):
                markup_senses += 1
            key = normalize(headword)
            if key in seen:
                collisions += 1
            else:
                seen[key] = headword
            record = {"index": index, "headword": headword, "senses": senses}
            handle.write(
                json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
            )
            count += 1

    payload = path.read_bytes()
    manifest = {
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "generator": "tools/eudic_phrase.py",
        "created_utc": _dt.datetime.now(_dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z"),
        "source": {
            "name": args.source.name,
            "bytes": args.source.stat().st_size,
            "sha256": sha256_file(args.source),
        },
        "container": {
            "page_table_offset": PAGE_TABLE_OFFSET,
            "page_count": PAGE_COUNT,
            "data_offset": DATA_OFFSET,
            "inflated_bytes": len(text),
            "record_table_offset": RECORD_TABLE_OFFSET,
            "record_count": RECORD_COUNT,
            "headword_buffer_offset": HEADWORD_BUFFER_OFFSET,
            "headword_buffer_size": buffer_size,
            "trailer_count": TRAILER_COUNT,
        },
        "records": count,
        "checks": {
            "empty_senses": empty_senses,
            "total_senses": total_senses,
            "with_pos_sense": pos_senses,
            "with_region_label": region_labels,
            "with_list_sense": list_senses,
            "senses_with_markup": markup_senses,
            "normalized_collisions": collisions,
        },
        "output": {
            "path": path.name,
            "bytes": len(payload),
            "sha256": sha256_bytes(payload),
        },
    }
    (out / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    if not args.quiet:
        print(f"wrote {count} phrase records to {path}", file=sys.stderr)
        print(json.dumps(manifest["checks"], ensure_ascii=False), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
