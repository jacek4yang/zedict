#!/usr/bin/env python3
"""Convert Eudic's combined.bin into zedict JSONL shards.

The container layout was recovered from the shipped binary (the exact
offsets are documented in README.md, "实现边界").  Every row of the
428,682-entry index is decoded independently: the headword comes from the
virtual headword buffer, the entry body comes from the exact (offset,
length) pair in the inflated text, so no headword is guessed and no entry
is split heuristically.

The output is a lossless, project-format projection:

    data/eudic/part-00000.jsonl ... part-00021.jsonl
    data/eudic/all.jsonl            (with --merge)
    data/eudic/related.tsv          (word<TAB>Chinese gloss, for --related)
    data/eudic/manifest.json        (counts, SHA-256, integrity checks)

Only data the user already owns is read.  The extracted dictionary is
derived from copyrighted content; keep the output outside version control
(the repository ignores /data).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import html
import json
import re
import struct
import sys
import urllib.parse
import zlib
from pathlib import Path

FORMAT = "zedict-eudic-jsonl"
FORMAT_VERSION = 1

# combined.bin layout (validated against the 97,174,509-byte shipping file).
PAGE_TABLE_OFFSET = 1_218
PAGE_COUNT = 12_191
PAGE_SIZE = 16_384
DATA_OFFSET = 98_747
INDEX_OFFSET = 69_126_817
RECORD_COUNT = 428_682
EN_RECORDS = 268_834
INFLATED_SIZE = 199_737_344
EXPECTED_LAST_PAGE = 69_124_866
STORE_SIZE_HINT = 14_784_127
TRAILING_ZERO_BYTES = 8_152

H5_RE = re.compile(r"<h5>(.*?)</h5>", re.S)
LI_RE = re.compile(r"<li[^>]*>(.*?)</li>", re.S | re.I)
TRANS_LABEL_RE = re.compile(
    r'<span class="txtDisabled">([^<]{1,30}):\s*</span>(.*?)(?=<span class="txtDisabled">|</div>|$)',
    re.S,
)
TRANS_VALUE_RE = re.compile(r'<span class="trans">(.*?)</span>', re.S)
DIC_LINK_RE = re.compile(r'<a\s+[^>]*href="dic://([^"]+)"[^>]*>(.*?)</a>', re.S | re.I)
RELATED_WORD_RE = re.compile(r"<w><b>(.*?)</b><span>(.*?)</span></w>", re.S)
PHRASE_SPLIT_RE = re.compile(r'<div id="phrase">|<p id="phrase">', re.I)
PY_RE = re.compile(r'<div class="PY">(.*?)</div>', re.S)
SYL_RE = re.compile(r"<SYL>(.*?)</SYL>", re.S)
POS_LEAD_RE = re.compile(
    r"^\s*(vt\.\s*&\s*vi\.|vt\.|vi\.|adj\.|adv\.|prep\.|pron\.|conj\.|interj\.|int\.|abbr\.|n\.|v\.|art\.|num\.|aux\.)\s*",
    re.I,
)
POS_TAG_MAP = {
    "vt. & vi.": "vt. & vi",
    "vt.": "vt",
    "vi.": "vi",
    "adj.": "adj",
    "adv.": "adv",
    "prep.": "prep",
    "pron.": "pron",
    "conj.": "conj",
    "interj.": "interj",
    "int.": "interj",
    "abbr.": "abbr",
    "n.": "n",
    "v.": "v",
    "art.": "art",
    "num.": "num",
    "aux.": "aux",
}
FORM_LABELS = {
    "名词": "名词",
    "形容词": "形容词",
    "副词": "副词",
    "复数": "复数",
    "比较级": "比较级",
    "最高级": "最高级",
    "动词": "动词",
    "缩写": "缩写",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def block_text(value: str) -> str:
    """Strip markup for a human-readable field, keeping word separations."""
    value = re.sub(r"(?i)<(?:br|/p|/div|/li|/ul|/ol|/h[1-6])\b[^>]*>", " ", value)
    value = re.sub(r"<[^>]+>", "", value)
    return " ".join(html.unescape(value).split())


def one_section(body: str, tag: str) -> str:
    pattern = re.compile(r"<" + tag + r">(.*?)</" + tag + r">", re.S)
    match = pattern.search(body)
    return match.group(1) if match else ""


def all_sections(body: str, tag: str) -> list[str]:
    pattern = re.compile(r"<" + tag + r">(.*?)</" + tag + r">", re.S)
    return pattern.findall(body)


def inflate_pages(source: Path) -> bytes:
    with open(source, "rb") as handle:
        handle.seek(PAGE_TABLE_OFFSET)
        table = handle.read(PAGE_COUNT * 8)
        if len(table) != PAGE_COUNT * 8:
            raise SystemExit("combined.bin: page offset table is truncated")
        offsets = struct.unpack("<" + "Q" * PAGE_COUNT, table)
        if offsets[0] != DATA_OFFSET or offsets[-1] != EXPECTED_LAST_PAGE:
            raise SystemExit(
                f"combined.bin: unexpected page table ({offsets[0]}, {offsets[-1]})"
            )
        if any(a >= b for a, b in zip(offsets, offsets[1:])):
            raise SystemExit("combined.bin: page offsets are not strictly ascending")
        pages = []
        for index, offset in enumerate(offsets):
            end = offsets[index + 1] if index + 1 < PAGE_COUNT else INDEX_OFFSET
            handle.seek(offset)
            chunk = handle.read(end - offset)
            try:
                page = zlib.decompress(chunk)
            except zlib.error as error:
                raise SystemExit(f"combined.bin: page {index} is not zlib: {error}")
            if len(page) != PAGE_SIZE:
                raise SystemExit(
                    f"combined.bin: page {index} inflated to {len(page)} bytes, expected {PAGE_SIZE}"
                )
            pages.append(page)
    data = b"".join(pages)
    if len(data) != INFLATED_SIZE:
        raise SystemExit(
            f"combined.bin: inflated size {len(data)} != expected {INFLATED_SIZE}"
        )
    return data


def load_inflated(source: Path, inflated_path: Path | None) -> bytes:
    if inflated_path is not None:
        data = inflated_path.read_bytes()
        if len(data) != INFLATED_SIZE:
            raise SystemExit(
                f"{inflated_path}: inflated size {len(data)} != expected {INFLATED_SIZE}"
            )
        return data
    return inflate_pages(source)


def load_container(source: Path, inflated: bytes):
    with open(source, "rb") as handle:
        handle.seek(INDEX_OFFSET)
        raw = handle.read(RECORD_COUNT * 16)
        if len(raw) != RECORD_COUNT * 16:
            raise SystemExit("combined.bin: record index is truncated")
        records = [struct.unpack_from("<IIII", raw, i * 16) for i in range(RECORD_COUNT)]

        pool = INDEX_OFFSET + RECORD_COUNT * 16
        handle.seek(pool)
        header = handle.read(20)
        if len(header) != 20:
            raise SystemExit("combined.bin: store header is truncated")
        store_size, store_hint, reserved = struct.unpack("<QQI", header)
        if store_hint != 18_227 or reserved != 0:
            raise SystemExit(
                f"combined.bin: unexpected store header ({store_size}, {store_hint}, {reserved})"
            )
        if store_size > STORE_SIZE_HINT + 4096 or store_size < STORE_SIZE_HINT:
            raise SystemExit(f"combined.bin: unexpected store size {store_size}")
        handle.seek(pool + 8)
        prelude = handle.read(12)
        handle.seek(pool + 20)
        store = handle.read(store_size)
        if len(store) != store_size:
            raise SystemExit("combined.bin: store is truncated")
    buffer = prelude + store
    if buffer[:3] != b"3G\x00":
        raise SystemExit("combined.bin: headword buffer prelude is not '3G'")

    # Entry intervals must tile the text exactly (zero gaps, zero overlaps).
    intervals = sorted((b, d) for _, b, _, d in records)
    cursor = 0
    for start, length in intervals:
        if start != cursor:
            raise SystemExit(
                f"combined.bin: entry intervals do not tile the text at {cursor} (next {start})"
            )
        cursor += length
    if cursor + TRAILING_ZERO_BYTES != len(inflated):
        raise SystemExit(
            f"combined.bin: tiling ends at {cursor}, expected {len(inflated) - TRAILING_ZERO_BYTES}"
        )
    return records, buffer


def english_headword(buffer: bytes, row: int, record) -> tuple[str, int, int, str, str | None]:
    a, _, _, _ = record
    end = buffer.find(b"\x00", a)
    if end < 0:
        raise SystemExit(f"row {row}: headword is not NUL-terminated")
    word = buffer[a:end].decode("utf-8", "strict")
    header = buffer[end + 1 : end + 9]
    if len(header) != 8:
        raise SystemExit(f"row {row}: record header is truncated")
    extra, index = struct.unpack("<II", header)
    if index != row:
        raise SystemExit(f"row {row}: header index says {index}")
    gloss_end = buffer.find(b"\x00", end + 9)
    if gloss_end < 0:
        raise SystemExit(f"row {row}: short gloss is not NUL-terminated")
    gloss = buffer[end + 9 : gloss_end].decode("utf-8", "strict")
    return word, extra, index, gloss, None


def chinese_headword(buffer: bytes, row: int, records) -> tuple[str, int, int, str, str | None]:
    start = records[row][0]
    end = records[row + 1][0] if row + 1 < RECORD_COUNT else len(buffer) - 12
    if end <= start:
        raise SystemExit(f"row {row}: empty Chinese headword range")
    word = buffer[start:end].decode("utf-8", "strict")
    return word, 0, row, "", None


def parse_links(fragment: str) -> list[str]:
    words = []
    for raw, text in DIC_LINK_RE.findall(fragment or ""):
        word = block_text(text) or urllib.parse.unquote(raw)
        word = " ".join(word.split())
        if word and word not in words:
            words.append(word)
    return words


def split_h5(section: str) -> dict[str, str]:
    headings = list(H5_RE.finditer(section or ""))
    result: dict[str, str] = {}
    for index, heading in enumerate(headings):
        title = block_text(heading.group(1))
        start = heading.end()
        end = headings[index + 1].start() if index + 1 < len(headings) else len(section)
        result.setdefault(title, "")
        result[title] += section[start:end]
    return result


def parse_relations(body: str) -> tuple[list[str], list[str], list[str], dict[str, str]]:
    synonyms: list[str] = []
    antonyms: list[str] = []
    related: list[str] = []
    glosses: dict[str, str] = {}
    for section in all_sections(body, "SYN"):
        parts = split_h5(section)
        for word in parse_links(parts.get("近义词", "")):
            if word not in synonyms:
                synonyms.append(word)
        for word in parse_links(parts.get("反义词", "")):
            if word not in antonyms:
                antonyms.append(word)
        for word, gloss_html in RELATED_WORD_RE.findall(parts.get("联想词", "")):
            word = block_text(word)
            if not word:
                continue
            if word not in related:
                related.append(word)
            gloss = block_text(gloss_html).strip().strip(";；,， ")
            if gloss and word not in glosses:
                glosses[word] = gloss
        for word in parse_links(parts.get("解析", "")):
            if word not in related:
                related.append(word)
    return synonyms, antonyms, related, glosses


def parse_phrases(body: str) -> list[dict[str, str]]:
    phrases: list[dict[str, str]] = []
    for section in all_sections(body, "SPEC"):
        for block in PHRASE_SPLIT_RE.split(section)[1:]:
            link = re.search(r'<a\s+[^>]*href="dic://([^"]+)"[^>]*>(.*?)</a>', block, re.S | re.I)
            if not link:
                continue
            phrase = block_text(link.group(2)) or urllib.parse.unquote(link.group(1))
            if not phrase:
                continue
            gloss_match = re.search(
                r'<span class="exp">(.*?)</span>|<div class="exp">(.*?)</div>', block, re.S
            )
            gloss = ""
            if gloss_match:
                gloss = block_text(gloss_match.group(1) or gloss_match.group(2) or "")
            item = {"phrase": " ".join(phrase.split())}
            if gloss:
                item["gloss"] = gloss
            if item not in phrases:
                phrases.append(item)
            if len(phrases) >= 64:
                return phrases
    return phrases


def parse_forms(body: str) -> dict[str, str]:
    collected: dict[str, str] = {}
    trans_blocks = re.findall(r'<div id="trans">(.*?)</div>', body, re.S)
    for block in trans_blocks:
        for label_html, values_html in TRANS_LABEL_RE.findall(block):
            label = "".join(block_text(label_html).split())
            values = [block_text(v).strip() for v in TRANS_VALUE_RE.findall(values_html)]
            values = [v for v in values if v]
            if not label or not values:
                continue
            if label == "时态":
                forms = expand_tense(values)
                for key, words in forms.items():
                    if words:
                        existing = collected.get(key, "")
                        merged = merge_words(existing, "，".join(words))
                        collected[key] = merged
                continue
            mapped = FORM_LABELS.get(label)
            if mapped is None:
                continue
            joined = "，".join(values)
            collected[mapped] = merge_words(collected.get(mapped, ""), joined)
    return {k: v for k, v in collected.items() if v}


def split_words(value: str) -> list[str]:
    return [item.strip() for item in re.split(r"[，,;；]", value) if item.strip()]


def merge_words(left: str, right: str) -> str:
    seen: list[str] = []
    for item in split_words(left) + split_words(right):
        if item not in seen:
            seen.append(item)
    return "，".join(seen)


def expand_tense(spans: list[str]) -> dict[str, list[str]]:
    """Map the 时态 row (past, participle, present/third person) onto labels.

    Observed shapes: [past, participle, "present，third"] (run/take),
    [past, "present，third"] (make) and ["past，present，third"] (set).
    """
    groups = [split_words(value) for value in spans if split_words(value)]
    result: dict[str, list[str]] = {
        "过去式": [],
        "过去分词": [],
        "现在分词": [],
        "第三人称单数": [],
    }
    if not groups:
        return result
    if len(groups) >= 3:
        result["过去式"] = groups[0][:1]
        result["过去分词"] = groups[1][:1]
        rest = [word for group in groups[2:] for word in group]
        if rest:
            result["现在分词"] = rest[:1]
            result["第三人称单数"] = rest[1:]
    elif len(groups) == 2:
        result["过去式"] = groups[0][:1]
        second = groups[1]
        if len(second) >= 3:
            result["过去分词"] = second[:1]
            result["现在分词"] = second[1:2]
            result["第三人称单数"] = second[2:]
        else:
            result["过去分词"] = groups[0][:1]
            if second:
                result["现在分词"] = second[:1]
                result["第三人称单数"] = second[1:]
    else:
        values = groups[0]
        result["过去式"] = values[:1]
        result["过去分词"] = values[:1]
        if len(values) >= 2:
            result["现在分词"] = values[1:2]
        if len(values) >= 3:
            result["第三人称单数"] = values[2:]
    return result


def derive_pos(fc_html: str, pos_html: str) -> list[str]:
    ordered: list[str] = []
    for token in re.split(r"[|,，\s]+", block_text(pos_html)):
        token = token.strip().lower()
        if token:
            canonical = POS_TAG_MAP.get(token + ".", token)
            if canonical not in ordered:
                ordered.append(canonical)
    fragments = [block_text(item) for item in LI_RE.findall(fc_html)]
    if fc_html and not fragments:
        fragments = [block_text(fc_html)]
    for fragment in fragments:
        match = POS_LEAD_RE.match(fragment)
        if not match:
            continue
        canonical = POS_TAG_MAP.get(match.group(1).lower())
        if canonical and canonical not in ordered:
            ordered.append(canonical)
    return ordered


def parse_phonetic(body: str) -> tuple[str | None, str | None, str | None]:
    phon = one_section(body, "phon")
    if not phon:
        return None, None, None

    def tag(name: str) -> str | None:
        match = re.search(r"<" + name + r">(.*?)</" + name + r">", phon, re.S)
        if not match:
            return None
        value = block_text(match.group(1))
        return value or None

    return tag("uk"), tag("us"), tag("en")


def parse_pinyin(fc_html: str) -> str | None:
    match = PY_RE.search(fc_html or "")
    if not match:
        return None
    value = " ".join(block_text(match.group(1)).split())
    return value or None


def parse_syllables(short_gloss: str) -> str | None:
    match = SYL_RE.search(short_gloss or "")
    if not match:
        return None
    value = block_text(match.group(1)).strip()
    return value or None


def entry_record(row: int, section: str, word: str, body: str, short_gloss: str):
    fc = one_section(body, "FC")
    pinyin = parse_pinyin(fc)
    fc_for_senses = PY_RE.sub("", fc) if pinyin else fc
    senses = [fc_for_senses] if block_text(fc_for_senses) else []
    if not senses and short_gloss:
        visible = block_text(short_gloss)
        if visible:
            senses = [visible]
    english = one_section(body, "FF")
    uk, us, en = parse_phonetic(body)
    synonyms, antonyms, related, glosses = parse_relations(body)
    forms = parse_forms(body)
    examples = [item for item in all_sections(body, "LJ") if item.strip()]
    tags = []
    for raw in all_sections(body, "TAG"):
        for tag in split_words(block_text(raw)):
            if tag not in tags:
                tags.append(tag)
    record = {
        "index": row,
        "section": section,
        "headword": word,
        "pos": derive_pos(fc, one_section(body, "POS")),
        "phon_uk": uk,
        "phon_us": us,
        "phon_en": en,
        "pinyin": pinyin,
        "senses": senses,
        "english": english or None,
        "synonyms": synonyms,
        "antonyms": antonyms,
        "related": related,
        "forms": forms or None,
        "examples": examples,
        "phrases": parse_phrases(body),
        "tags": tags,
        "syllables": parse_syllables(short_gloss),
        "short_gloss": block_text(short_gloss) or None,
        "has_forvo": "<FORVO/>" in body,
        "offset": None,
    }
    return record, glosses


def write_shard(path: Path, records: list[dict]) -> dict:
    payload = "".join(
        json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n"
        for record in records
    ).encode("utf-8")
    path.write_bytes(payload)
    return {
        "path": path.name,
        "records": len(records),
        "bytes": len(payload),
        "sha256": sha256_bytes(payload),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--source", type=Path, required=True, help="path to dic/combined.bin")
    parser.add_argument("--out", type=Path, default=Path("data/eudic"), help="output directory")
    parser.add_argument(
        "--inflated",
        type=Path,
        default=None,
        help="reuse an already inflated page stream instead of decompressing",
    )
    parser.add_argument(
        "--shard-records", type=int, default=20_000, help="records per JSONL shard"
    )
    parser.add_argument("--merge", action="store_true", help="also write all.jsonl")
    parser.add_argument("--limit", type=int, default=None, help="debug: stop after N records")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    if args.shard_records <= 0:
        parser.error("--shard-records must be positive")
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)

    if not args.quiet:
        print(f"reading {args.source}", file=sys.stderr)
    inflated = load_inflated(args.source, args.inflated)
    records, buffer = load_container(args.source, inflated)
    source_sha = sha256_file(args.source)
    if not args.quiet:
        print(f"index {RECORD_COUNT} rows, headword buffer {len(buffer)} bytes", file=sys.stderr)

    shards: list[dict] = []
    shard: list[dict] = []
    gloss_table: dict[str, str] = {}
    total = args.limit if args.limit is not None else RECORD_COUNT
    en_count = 0
    zh_count = 0
    duplicates = 0
    seen_words: set[str] = set()
    empty_senses = 0
    with_phon = 0
    with_examples = 0
    with_relations = 0
    with_phrases = 0
    bad_headers = 0

    for row in range(total):
        section = "en" if row < EN_RECORDS else "zh"
        if section == "en":
            word, _, _, short_gloss, _ = english_headword(buffer, row, records[row])
            en_count += 1
        else:
            word, _, _, short_gloss, _ = chinese_headword(buffer, row, records)
            zh_count += 1
        _, offset, _, length = records[row]
        body = inflated[offset : offset + length].decode("utf-8", "strict")
        if not body.startswith("<FC>"):
            bad_headers += 1
        record, glosses = entry_record(row, section, word, body, short_gloss)
        record["offset"] = offset
        # RawEntry has no index/section/tags/short_gloss fields; they stay in
        # the JSONL as provenance and are ignored by the compiler.
        if not record["senses"]:
            empty_senses += 1
        if record["phon_uk"] or record["phon_us"] or record["phon_en"]:
            with_phon += 1
        if record["examples"]:
            with_examples += 1
        if record["synonyms"] or record["antonyms"] or record["related"]:
            with_relations += 1
        if record["phrases"]:
            with_phrases += 1
        if word in seen_words:
            duplicates += 1
        else:
            seen_words.add(word)
        for relation_word, gloss in glosses.items():
            gloss_table.setdefault(relation_word, gloss)
        shard.append(record)
        if len(shard) >= args.shard_records:
            shards.append(write_shard(out / f"part-{len(shards):05d}.jsonl", shard))
            shard = []
    if shard:
        shards.append(write_shard(out / f"part-{len(shards):05d}.jsonl", shard))

    related_path = out / "related.tsv"
    related_rows = 0
    with open(related_path, "w", encoding="utf-8", newline="\n") as handle:
        for word in sorted(gloss_table):
            gloss = gloss_table[word].replace("\t", " ").replace("\n", " ")
            handle.write(f"{word}\t{gloss}\n")
            related_rows += 1
    related_meta = {
        "path": related_path.name,
        "rows": related_rows,
        "bytes": related_path.stat().st_size,
        "sha256": sha256_file(related_path),
    }

    merged_path = None
    if args.merge:
        merged = out / "all.jsonl"
        with open(merged, "wb") as target:
            for shard_meta in shards:
                with open(out / shard_meta["path"], "rb") as source_handle:
                    while True:
                        chunk = source_handle.read(1 << 20)
                        if not chunk:
                            break
                        target.write(chunk)
        merged_path = {
            "path": merged.name,
            "bytes": merged.stat().st_size,
            "sha256": sha256_file(merged),
        }

    manifest = {
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "generator": "tools/eudic_extract.py",
        "created_utc": _dt.datetime.now(_dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z"),
        "source": {
            "name": args.source.name,
            "bytes": args.source.stat().st_size,
            "sha256": source_sha,
        },
        "container": {
            "page_table_offset": PAGE_TABLE_OFFSET,
            "page_count": PAGE_COUNT,
            "page_size": PAGE_SIZE,
            "inflated_bytes": len(inflated),
            "index_offset": INDEX_OFFSET,
            "record_count": RECORD_COUNT,
        },
        "records": sum(item["records"] for item in shards),
        "shard_records": args.shard_records,
        "shards": shards,
        "related_tsv": related_meta,
        "merged": merged_path,
        "sections": {"en": en_count, "zh": zh_count},
        "checks": {
            "duplicate_headwords": duplicates,
            "empty_senses": empty_senses,
            "bad_entry_headers": bad_headers,
            "with_phonetic": with_phon,
            "with_examples": with_examples,
            "with_relations": with_relations,
            "with_phrases": with_phrases,
        },
    }
    (out / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    if not args.quiet:
        print(
            f"wrote {manifest['records']} records in {len(shards)} shards, "
            f"{related_meta['rows']} related glosses",
            file=sys.stderr,
        )
        print(json.dumps(manifest["checks"], ensure_ascii=False), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
