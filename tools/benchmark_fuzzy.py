"""Compare fuzzy recall and latency between two builds that differ only by --spell.

The candidate pool in `query --mode fuzzy` is produced by an exhaustive length-window
scan plus the inflection table, so a delete-1 index can only ever add candidates the
scan was not allowed to reach (`max+1`). This script measures exactly that difference:
same source, two databases, 30 classic misspellings, plus fresh-process latency on
random real words with the OS file cache warmed (never labelled cold-disk latency).

Recall is reported against the word the typo was meant to be. Words whose intended
target has no card at all are broken out separately -- no candidate generator can
suggest a headword the dictionary does not contain, and mixing those two failures
would make the recall number meaningless.
"""
import argparse
import json
import platform
import random
import sqlite3
import statistics
import subprocess
import time
from pathlib import Path

# 30 classic misspellings paired with the word each one was meant to be.
CASES = [
    ("runing", "run"),
    ("recieve", "receive"),
    ("seperate", "separate"),
    ("wich", "which"),
    ("thier", "their"),
    ("becuase", "because"),
    ("defualt", "default"),
    ("acheive", "achieve"),
    ("goverment", "government"),
    ("enviroment", "environment"),
    ("langauge", "language"),
    ("wrold", "world"),
    ("axs", "axe"),
    ("teh", "the"),
    ("addres", "address"),
    ("alrm", "alarm"),
    ("calender", "calendar"),
    ("cemetary", "cemetery"),
    ("definately", "definitely"),
    ("embarass", "embarrass"),
    ("existant", "existent"),
    ("familar", "familiar"),
    ("foriegn", "foreign"),
    ("gaurd", "guard"),
    ("harrass", "harass"),
    ("independant", "independent"),
    ("occured", "occurred"),
    ("recomend", "recommend"),
    ("adress", "address"),
    ("buisness", "business"),
]
RANKS = (1, 3, 5, 10)


def suggestions(binary: Path, database: Path, word: str, limit: int) -> list:
    """Top-k suggestions for one misspelling; exit code 3 just means "nothing found"."""
    result = subprocess.run(
        [str(binary.resolve()), "--db", str(database.resolve()),
         "query", word, "--mode", "fuzzy", "--limit", str(limit), "--json"],
        capture_output=True,
    )
    if result.returncode not in (0, 3):
        raise SystemExit(
            result.stderr.decode("utf-8", errors="replace") or f"fuzzy query failed: {word}"
        )
    if not result.stdout:
        return []
    return json.loads(result.stdout)["suggestions"]


def headwords(database: Path) -> set:
    connection = sqlite3.connect(f"file:{database.as_posix()}?mode=ro", uri=True)
    try:
        return {row[0] for row in connection.execute("SELECT key FROM entries")}
    finally:
        connection.close()


def recall(binary: Path, database: Path) -> dict:
    """Per-rank recall plus the misses, so a regression can be read off the output."""
    hits = {rank: 0 for rank in RANKS}
    missed = {rank: [] for rank in RANKS}
    for word, target in CASES:
        ranked = [item["word"] for item in suggestions(binary, database, word, max(RANKS))]
        position = ranked.index(target) + 1 if target in ranked else None
        for rank in RANKS:
            if position is not None and position <= rank:
                hits[rank] += 1
            else:
                missed[rank].append(word if position is None else f"{word}@{position}")
    return {
        "cases": len(CASES),
        **{f"recall_at_{rank}": f"{hits[rank]}/{len(CASES)}" for rank in RANKS},
        **{f"missed_at_{rank}": missed[rank] for rank in RANKS},
    }


def latency(binary: Path, database: Path, samples: int, max_length: int) -> dict:
    """Fresh process per query on random real words; first run only warms the cache.

    `--length 4` matters on its own: the delete-1 backfill can only ever add a
    candidate to a query of four characters or fewer (longer queries already allow
    distance 2 in the exhaustive scan), so sampling the whole dictionary measures
    mostly the code path both builds share.
    """
    words = sorted(key for key in headwords(database) if len(key) <= max_length) if max_length else sorted(headwords(database))
    random.seed(20261004)
    picks = random.sample(words, min(samples, len(words)))
    timings = []
    for index, word in enumerate(picks):
        start = time.perf_counter()
        subprocess.run(
            [str(binary.resolve()), "--db", str(database.resolve()),
             "query", word, "--mode", "fuzzy", "--json"],
            capture_output=True,
        )
        if index:
            timings.append((time.perf_counter() - start) * 1000)
    ordered = sorted(timings)
    return {
        "runs": len(ordered),
        "p50_ms": round(statistics.median(ordered), 3),
        "p95_ms": round(ordered[int(0.95 * (len(ordered) - 1))], 3),
    }


parser = argparse.ArgumentParser()
parser.add_argument("binary", type=Path)
parser.add_argument("--plain", type=Path, required=True, help="build without --spell")
parser.add_argument("--spell", type=Path, required=True, help="build with --spell")
parser.add_argument("--timing", action="store_true", help="also measure fresh-process latency")
parser.add_argument("--samples", type=int, default=500)
parser.add_argument(
    "--length",
    type=int,
    default=0,
    help="only sample headwords this short or shorter (0 = no filter)",
)
args = parser.parse_args()
if args.plain == args.spell:
    parser.error("--plain and --spell must be two different databases")
for database in (args.plain, args.spell):
    if not database.exists():
        raise SystemExit(f"词库不存在: {database}")

plain = recall(args.binary, args.plain)
with_spell = recall(args.binary, args.spell)
available = headwords(args.plain)
unfixable = sorted({target for _, target in CASES if target not in available})


def hits(value: str) -> int:
    """`recall_at_5` is a string like "25/30"; take the numerator before comparing."""
    return int(value.split("/")[0])


report = {
    "platform": platform.platform(),
    "processor": platform.processor(),
    "method": "fresh process; OS file cache warmed; captured complete stdout; no cold-disk claim",
    "databases": {
        "plain": {"path": str(args.plain), "bytes": args.plain.stat().st_size},
        "spell": {"path": str(args.spell), "bytes": args.spell.stat().st_size},
    },
    "size_delta_bytes": args.spell.stat().st_size - args.plain.stat().st_size,
    "plain": plain,
    "spell": with_spell,
    # A typo whose intended target has no card cannot be recovered by any candidate
    # generator; that is a coverage hole, and it has to be read next to recall.
    "targets_absent_from_dictionary": unfixable,
    "delta_recall_at": {
        str(rank): hits(with_spell[f"recall_at_{rank}"]) - hits(plain[f"recall_at_{rank}"])
        for rank in RANKS
    },
}
if args.timing:
    report["latency"] = {
        "sample_max_length": args.length or None,
        "plain": latency(args.binary, args.plain, args.samples, args.length),
        "spell": latency(args.binary, args.spell, args.samples, args.length),
    }
print(json.dumps(report, ensure_ascii=False, indent=2))
