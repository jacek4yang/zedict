"""Word-level data: COCA frequency bands and exam/Collins syllabus lists.

The library itself carries no per-word difficulty, so `zedict hint` needs one
input it can honestly point at: ``level.bin`` from a locally installed Eudic
dictionary. It is *not* in this repository, hence ``--input`` — the file is read
only to be decoded, and its SHA-256 is recorded in the output so a later build
can say exactly which copy produced it.

Format (reverse-engineered, all offsets little-endian):

  * bytes ``0..7``   : magic and a small index;
  * then, packed with no gaps, one descriptor per word list: two u32
    ``begin``/``end`` followed by ``name\\n{json}\\n``;
  * ``begin``/``end`` are offsets into one shared payload that starts right after
    the last descriptor and runs to end of file;
  * each payload region is a plain ``\\n``-separated list of English items.

The ten ``level_*`` regions are COCA frequency bands, in COCA rank order and
contiguous; the twenty-four ``tag_*`` regions are syllabus word lists (初中/高中/
四级/六级/考研/专四/专八/PETS/雅思/托福/KET/PET/托业/BEC/GRE/GMAT/SAT/MBA/ACT and
Collins 1-5). Both are per-word source data, not an inference from anything.

Two facts survive into the output and are worth knowing before using it:

  * bands overlap -- a word can sit in more than one ``level_*`` region, so the
    lowest (most frequent) band wins and the duplicates are counted in ``--stats``;
  * the lists carry inflected forms and multi-word items (``in other words``,
    ``flyaway``), so they are matched against a query surface form as well as a
    lemma, and item counts never equal the number of distinct headwords.

Output is JSONL: one ``meta`` line (source + band/tag descs + counts), then one
``word`` line per distinct normalised item, sorted. Normalisation mirrors
``zedict::store::normalize`` on purpose -- keys that differ only by case,
curly quotes or runs of blanks would otherwise silently fail to join.

Usage: python tools/word_levels.py --input /path/to/eudic/dic/level.bin
                                   [--out data/word_levels.jsonl] [--stats]
"""

import argparse
import hashlib
import json
import re
import struct
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = ROOT / "data/word_levels.jsonl"
# Anchored on the JSON's own first key: a word list is free of '{', so this cannot
# match inside the payload, and walking descriptors forward makes `base` exact.
DESCRIPTOR_RE = re.compile(rb"(?<![A-Za-z0-9_])([A-Za-z_][A-Za-z_0-9]*)\n(\{\"name\": )")
BAND_PREFIX = "level_"
TAG_PREFIX = "tag_"


def normalize(text: str) -> str:
    """Mirror of ``store::normalize``: NFC, curly apostrophes, blank runs, case."""
    folded = unicodedata.normalize("NFC", text).replace("\u2019", "'").replace("\u2018", "'")
    return " ".join(folded.split()).lower()


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 16), b""):
            digest.update(chunk)
    return digest.hexdigest()


def descriptor_block(data: bytes):
    """Walk the packed descriptors; return ``([(name, begin, end, meta)], base)``.

    The first word list starts at offset 0 of the payload, and consecutive lists
    abut each other, so a gap or overlap between ``end`` and the next ``begin``
    means the offsets were read from the wrong place -- stop guessing and fail.
    """
    first = DESCRIPTOR_RE.search(data, 8)
    if first is None:
        raise SystemExit("没找到任何词表描述符，这个文件不是 level.bin")
    rows = []
    position = first.start() - 8
    while position + 8 <= len(data):
        begin, end = struct.unpack_from("<II", data, position)
        match = DESCRIPTOR_RE.match(data, position + 8)
        if match is None or end < begin:
            break
        if rows and begin != rows[-1][2]:
            raise SystemExit(f"词表 {match.group(1).decode()} 的偏移与上一段不连续")
        name = match.group(1).decode("ascii")
        brace = match.start(2)
        line_end = data.index(b"\n", brace)
        try:
            meta = json.loads(data[brace:line_end].decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise SystemExit(f"词表 {name} 的描述符解不开：{exc}") from None
        if meta.get("name") != name:
            raise SystemExit(f"词表 {name} 的自述名字对不上：{meta.get('name')!r}")
        rows.append((name, begin, end, meta))
        position = line_end + 1
    if not rows:
        raise SystemExit("描述符一处也没解开")
    return rows, position


def decode(path: Path, stats: bool):
    data = path.read_bytes()
    rows, base = descriptor_block(data)
    payload_bytes = len(data) - base
    if rows[0][1] != 0 or rows[-1][2] != payload_bytes:
        raise SystemExit(
            f"词表没有铺满 payload：first={rows[0][1]} last={rows[-1][2]} payload={payload_bytes}"
        )

    bands, tags, items = [], [], {}
    band_duplicates = items_total = 0
    for name, begin, end, meta in rows:
        body = data[base + begin : base + end].decode("utf-8")
        words = [item for item in body.split("\n") if item.strip()]
        items_total += len(words)
        if name.startswith(BAND_PREFIX):
            level = int(name[len(BAND_PREFIX) :]) + 1
            bands.append({"level": level, "name": name, "desc": meta.get("desc", ""),
                          "color": meta.get("color", ""), "items": len(words)})
            if level != len(bands):
                raise SystemExit(f"等级不连续：{name} 是第 {len(bands)} 条")
        elif name.startswith(TAG_PREFIX):
            tag = name[len(TAG_PREFIX) :]
            tags.append({"tag": tag, "name": name, "desc": meta.get("desc", ""),
                         "color": meta.get("color", ""), "items": len(words)})
        else:
            raise SystemExit(f"不认识的名字空间：{name}")
        is_band = name.startswith(BAND_PREFIX)
        for raw in words:
            key = normalize(raw)
            if not key:
                continue
            entry = items.setdefault(key, {"level": None, "tags": []})
            if is_band:
                if entry["level"] is None:
                    entry["level"] = level
                else:
                    # A word listed in two bands keeps the more frequent one.
                    band_duplicates += 1
                    entry["level"] = min(entry["level"], level)
            elif tag not in entry["tags"]:
                entry["tags"].append(tag)

    if stats:
        print(
            f"base={base} records={len(rows)} bands={len(bands)} tags={len(tags)} "
            f"items_total={items_total} "
            f"distinct={len(items)} band_duplicates={band_duplicates} "
            f"with_band={sum(1 for v in items.values() if v['level'] is not None)} "
            f"tag_only={sum(1 for v in items.values() if v['level'] is None)}",
            file=sys.stderr,
        )
    return bands, tags, items


def write(out: Path, source: Path, bands, tags, items) -> int:
    out.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        json.dumps(
            {
                "kind": "meta",
                "source": {
                    "path": str(source),
                    "sha256": sha256_of(source),
                    "bytes": source.stat().st_size,
                },
                "bands": bands,
                "tags": tags,
            },
            ensure_ascii=False,
            separators=(",", ":"),
        )
    ]
    lines += [
        json.dumps(
            {"kind": "word", "word": key, "level": value["level"], "tags": sorted(value["tags"])},
            ensure_ascii=False,
            separators=(",", ":"),
        )
        for key, value in sorted(items.items())
    ]
    # Atomic replace: a build may read this file while an earlier one is being kept.
    temp = out.with_name(out.name + ".tmp")
    temp.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    temp.replace(out)
    return len(items)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--stats", action="store_true")
    args = parser.parse_args()

    if not args.input.is_file():
        raise SystemExit(
            f"找不到 {args.input}；词级数据来自本机欧陆词典的 dic/level.bin，"
            "装上它再用 --input 指过来（本仓库不含该文件）"
        )
    bands, tags, items = decode(args.input, args.stats)
    count = write(args.out, args.input, bands, tags, items)
    print(f"wrote {args.out} ({count} items, {len(bands)} bands, {len(tags)} tags)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
