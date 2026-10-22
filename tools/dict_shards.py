"""Split a JSONL dictionary into verified shards and reassemble it byte-for-byte.

Sharding only works around transport limits; it does not grant redistribution
rights. Keep shards of a proprietary dictionary in storage you are allowed to
use, and never upload them to a public repository or public CI artifact.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path

FORMAT = "zedict-jsonl-shards-v1"
MANIFEST = "manifest.json"
BLOCK = 1024 * 1024


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(BLOCK), b""):
            digest.update(block)
    return digest.hexdigest()


def _check_part_name(name: str) -> None:
    if not name or Path(name).name != name or name in {".", ".."}:
        raise SystemExit(f"manifest 里的分片名不安全: {name!r}")


def split(source: Path, out: Path, part_mib: float) -> dict:
    if part_mib <= 0:
        raise SystemExit("--part-mib 必须大于 0")
    if not source.is_file():
        raise SystemExit(f"找不到输入文件: {source}")
    try:
        out.mkdir(parents=True)
    except FileExistsError:
        raise SystemExit(f"输出目录已存在，拒绝覆盖: {out}") from None
    limit = max(1, int(part_mib * BLOCK))

    source_digest = hashlib.sha256()
    source_bytes = 0
    source_lines = 0
    parts: list[dict] = []
    handle = None
    part_digest = None
    part_bytes = 0
    part_lines = 0
    part_path = None

    def close_part() -> None:
        nonlocal handle, part_digest, part_bytes, part_lines, part_path
        if handle is None:
            return
        handle.close()
        handle = None
        parts.append(
            {
                "name": part_path.name,
                "bytes": part_bytes,
                "sha256": part_digest.hexdigest(),
                "lines": part_lines,
            }
        )

    try:
        with source.open("rb") as src:
            for line in src:
                source_digest.update(line)
                source_bytes += len(line)
                source_lines += 1
                if handle is None or (part_bytes > 0 and part_bytes + len(line) > limit):
                    close_part()
                    part_path = out / f"part-{len(parts):05d}.jsonl"
                    handle = part_path.open("wb")
                    part_digest = hashlib.sha256()
                    part_bytes = 0
                    part_lines = 0
                handle.write(line)
                part_digest.update(line)
                part_bytes += len(line)
                part_lines += 1
        close_part()
    except BaseException:
        if handle is not None:
            handle.close()
        raise

    manifest = {
        "format": FORMAT,
        "source": source.name,
        "bytes": source_bytes,
        "sha256": source_digest.hexdigest(),
        "lines": source_lines,
        "part_mib": part_mib,
        "parts": parts,
    }
    (out / MANIFEST).write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return manifest


def join(source: Path, out: Path, force: bool) -> dict:
    manifest_path = source / MANIFEST
    if not manifest_path.is_file():
        raise SystemExit(f"缺少清单: {manifest_path}")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as err:
        raise SystemExit(f"无法读取清单 {manifest_path}: {err}") from None
    if manifest.get("format") != FORMAT:
        raise SystemExit(f"清单格式不是 {FORMAT}: {manifest.get('format')!r}")
    if out.exists() and not force:
        raise SystemExit(f"输出文件已存在，加 --force 才会覆盖: {out}")

    temporary = out.with_name(out.name + ".tmp")
    digest = hashlib.sha256()
    total_bytes = 0
    total_lines = 0
    try:
        with temporary.open("wb") as dst:
            for entry in manifest.get("parts", []):
                name = entry.get("name")
                _check_part_name(name)
                path = source / name
                if not path.is_file():
                    raise SystemExit(f"缺少分片: {path}")
                part_digest = hashlib.sha256()
                part_bytes = 0
                part_lines = 0
                with path.open("rb") as part:
                    for line in part:
                        dst.write(line)
                        digest.update(line)
                        part_digest.update(line)
                        part_bytes += len(line)
                        part_lines += 1
                if part_bytes != entry.get("bytes") or part_lines != entry.get("lines"):
                    raise SystemExit(f"分片大小或行数不符: {name}")
                if part_digest.hexdigest() != entry.get("sha256"):
                    raise SystemExit(f"分片校验失败: {name}")
                total_bytes += part_bytes
                total_lines += part_lines
        actual = digest.hexdigest()
        if (
            total_bytes != manifest.get("bytes")
            or total_lines != manifest.get("lines")
            or actual != manifest.get("sha256")
        ):
            raise SystemExit("合并结果与清单不符，已保留临时文件供排查")
        os.replace(temporary, out)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    return {
        "out": str(out),
        "bytes": total_bytes,
        "lines": total_lines,
        "sha256": manifest["sha256"],
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="把 JSONL 词库切成可校验分片，或按清单原样合并。"
    )
    sub = parser.add_subparsers(dest="action", required=True)
    cut = sub.add_parser("split", help="输入一个 JSONL，输出 part-*.jsonl 与 manifest.json")
    cut.add_argument("--input", type=Path, required=True)
    cut.add_argument("--out", type=Path, required=True, help="必须是不存在的目录")
    cut.add_argument("--part-mib", type=float, default=24.0, help="每片目标大小，默认 24 MiB")
    merge = sub.add_parser("join", help="按 manifest.json 校验并合并分片")
    merge.add_argument("--input", type=Path, required=True, help="包含 manifest.json 的目录")
    merge.add_argument("--out", type=Path, required=True)
    merge.add_argument("--force", action="store_true")
    args = parser.parse_args()

    if args.action == "split":
        manifest = split(args.input, args.out, args.part_mib)
        print(
            json.dumps(
                {
                    "parts": len(manifest["parts"]),
                    "bytes": manifest["bytes"],
                    "lines": manifest["lines"],
                    "sha256": manifest["sha256"],
                },
                ensure_ascii=False,
            )
        )
    else:
        print(json.dumps(join(args.input, args.out, args.force), ensure_ascii=False))


if __name__ == "__main__":
    main()
