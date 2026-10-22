"""Measure fresh-process latency with warmed OS file cache; never labels it cold-disk latency."""
import argparse
import json
from pathlib import Path
import platform
import statistics
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=Path)
parser.add_argument("database", type=Path)
parser.add_argument("--runs", type=int, default=30)
args = parser.parse_args()
if args.runs < 2:
    parser.error("--runs must be >=2")
base = [str(args.binary.resolve()), "--db", str(args.database.resolve())]
report = {"platform": platform.platform(), "processor": platform.processor(), "database_bytes": args.database.stat().st_size,
          "method": "fresh process; OS file cache warmed; captured complete stdout; no cold-disk claim", "queries": {}}
for query, mode in [("world", "exact"), ("计算机", "reverse"), ("wrold", "fuzzy")]:
    timings = []
    for i in range(args.runs + 1):
        start = time.perf_counter()
        result = subprocess.run(base + ["query", query, "--mode", mode, "--json"], capture_output=True)
        elapsed = (time.perf_counter() - start) * 1000
        if result.returncode != 0:
            raise SystemExit(result.stderr.decode("utf-8", errors="replace") or f"Query failed: {query}")
        if i:
            timings.append(elapsed)
    ordered = sorted(timings)
    report["queries"][query] = {"p50_ms": round(statistics.median(ordered), 3),
                              "p95_ms": round(ordered[int(0.95 * (len(ordered) - 1))], 3), "runs": args.runs}
print(json.dumps(report, ensure_ascii=False, indent=2))
