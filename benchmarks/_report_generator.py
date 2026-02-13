#!/usr/bin/env python3
"""Generate a Markdown benchmark report from JSON result files.

Usage:
    python benchmarks/_report_generator.py
    python benchmarks/_report_generator.py --tags py3.12,py3.13,py3.13t
"""

import argparse
import json
import platform
from datetime import datetime, timezone
from pathlib import Path

RESULTS_DIR = Path(__file__).resolve().parent / "results"
REPORT_PATH = Path(__file__).resolve().parent / "BENCHMARK_REPORT.md"


def _fmt_ops(ops: float) -> str:
    if ops >= 1_000_000:
        return f"{ops / 1_000_000:.2f}M"
    if ops >= 1_000:
        return f"{ops / 1_000:.0f}K"
    return f"{ops:.0f}"


def _load_results(tags: list[str] | None) -> dict[str, dict]:
    data: dict[str, dict] = {}
    if tags:
        for tag in tags:
            p = RESULTS_DIR / f"bench_{tag}.json"
            if p.exists():
                data[tag] = json.loads(p.read_text())
            else:
                print(f"Warning: {p} not found, skipping")
    else:
        for p in sorted(RESULTS_DIR.glob("bench_*.json")):
            tag = p.stem.removeprefix("bench_")
            data[tag] = json.loads(p.read_text())
    return data


def _md_table(headers: list[str], rows: list[list[str]]) -> str:
    lines = []
    lines.append("| " + " | ".join(headers) + " |")
    lines.append("| " + " | ".join("---" for _ in headers) + " |")
    for row in rows:
        lines.append("| " + " | ".join(row) + " |")
    return "\n".join(lines)


def generate_report(data: dict[str, dict]) -> str:
    sections: list[str] = []

    # Header
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    sections.append("# fast_cache Benchmark Report\n")
    sections.append(f"Generated: {now}  ")
    sections.append(f"Machine: {platform.machine()} / {platform.system()} {platform.release()}  ")

    # Python versions
    versions = []
    for tag, run in data.items():
        py = run["python"]
        ft = " (free-threaded)" if py["gil_disabled"] else ""
        versions.append(f"**{tag}**: Python {py['version']}{ft}")
    sections.append("Python versions: " + ", ".join(versions) + "\n")

    # Contestants
    first_run = next(iter(data.values()))
    if "contestants" in first_run:
        sections.append("## Contestants\n")
        headers = ["Library", "Version", "Available", "Thread-safe"]
        rows = []
        for name, info in first_run["contestants"].items():
            rows.append([
                name,
                info.get("version", ""),
                "Yes" if info.get("available") else "No",
                "Yes" if info.get("thread_safe") else "No (+ Lock)",
            ])
        sections.append(_md_table(headers, rows))
        sections.append("")

    # ── Single-thread throughput ──
    sections.append("## Single-Thread Throughput (ops/s)\n")
    for tag, run in data.items():
        py = run["python"]
        ft = " (free-threaded)" if py["gil_disabled"] else ""
        sections.append(f"### {tag} — Python {py['version']}{ft}\n")

        tp = run["throughput"]
        # Gather all contestant names across sizes (excluding zoocache_unbounded)
        all_names: list[str] = []
        for sz_key, sz_data in tp.items():
            if sz_key == "zoocache_unbounded":
                continue
            for name in sz_data:
                if name not in all_names:
                    all_names.append(name)

        headers = ["Cache Size"] + all_names
        rows = []
        for sz_key in sorted((k for k in tp if k != "zoocache_unbounded"), key=int):
            row = [sz_key]
            for name in all_names:
                val = tp[sz_key].get(name)
                row.append(_fmt_ops(val) if val is not None else "-")
            rows.append(row)
        sections.append(_md_table(headers, rows))

        if "zoocache_unbounded" in tp:
            zoo_ops = tp["zoocache_unbounded"].get("zoocache")
            if zoo_ops:
                sections.append(
                    f"\n*zoocache (unbounded, no maxsize): {_fmt_ops(zoo_ops)} ops/s*"
                )
        sections.append("")

    # ── Multi-thread scaling ──
    sections.append("## Multi-Thread Scaling (ops/s)\n")
    for tag, run in data.items():
        py = run["python"]
        ft = " (free-threaded)" if py["gil_disabled"] else ""
        sections.append(f"### {tag} — Python {py['version']}{ft}\n")

        th = run["threading"]
        all_names = []
        for tc_data in th.values():
            for name in tc_data:
                if name not in all_names:
                    all_names.append(name)

        headers = ["Threads"] + all_names
        rows = []
        for tc_key in sorted(th, key=int):
            row = [tc_key]
            for name in all_names:
                val = th[tc_key].get(name)
                row.append(_fmt_ops(val) if val is not None else "-")
            rows.append(row)
        sections.append(_md_table(headers, rows))
        sections.append("")

    # ── Sustained throughput ──
    tags_with_sustained = [t for t, r in data.items() if "sustained" in r]
    if tags_with_sustained:
        sections.append("## Sustained Throughput (10s, ops/s)\n")
        # Gather all contestant names
        all_names = []
        for tag in tags_with_sustained:
            for name in data[tag]["sustained"]:
                if name not in all_names:
                    all_names.append(name)

        headers = ["Version"] + all_names
        rows = []
        for tag in tags_with_sustained:
            row = [tag]
            for name in all_names:
                d = data[tag]["sustained"].get(name)
                row.append(_fmt_ops(d["ops_per_sec"]) if d else "-")
            rows.append(row)
        sections.append(_md_table(headers, rows))
        sections.append("")

    # ── TTL throughput ──
    tags_with_ttl = [t for t, r in data.items() if "ttl" in r]
    if tags_with_ttl:
        sections.append("## TTL Throughput (10s per TTL, ops/s)\n")
        for tag in tags_with_ttl:
            py = data[tag]["python"]
            ft = " (free-threaded)" if py["gil_disabled"] else ""
            sections.append(f"### {tag} — Python {py['version']}{ft}\n")

            ttl_data = data[tag]["ttl"]
            all_names = []
            for ttl_results in ttl_data.values():
                for name in ttl_results:
                    if name not in all_names:
                        all_names.append(name)

            headers = ["TTL"] + all_names
            rows = []
            for ttl_key, ttl_results in ttl_data.items():
                row = [ttl_key]
                for name in all_names:
                    d = ttl_results.get(name)
                    row.append(_fmt_ops(d["ops_per_sec"]) if d else "-")
                rows.append(row)
            sections.append(_md_table(headers, rows))
            sections.append("")

    # ── Shared backend ──
    tags_with_shared = [t for t, r in data.items() if "shared_throughput" in r]
    if tags_with_shared:
        sections.append("## Shared Backend — Memory vs Shared (fast_cache only)\n")
        headers = ["Version", "Memory (ops/s)", "Shared (ops/s)", "Ratio"]
        rows = []
        for tag in tags_with_shared:
            st = data[tag]["shared_throughput"]
            mem = st["memory"]["ops_per_sec"]
            shm = st["shared"]["ops_per_sec"]
            ratio = f"{mem / shm:.1f}x" if shm else "inf"
            rows.append([tag, _fmt_ops(mem), _fmt_ops(shm), ratio])
        sections.append(_md_table(headers, rows))
        sections.append("")

    # ── Multi-process ──
    tags_with_mp = [t for t, r in data.items() if "multiprocess" in r]
    if tags_with_mp:
        sections.append("## Multi-Process Scaling — Shared Backend (fast_cache only)\n")
        for tag in tags_with_mp:
            py = data[tag]["python"]
            ft = " (free-threaded)" if py["gil_disabled"] else ""
            sections.append(f"### {tag} — Python {py['version']}{ft}\n")

            mp = data[tag]["multiprocess"]
            headers = ["Processes", "Total ops/s", "Wall time (s)"]
            rows = []
            for np_key in sorted(mp, key=int):
                d = mp[np_key]
                rows.append([
                    np_key,
                    _fmt_ops(d["total_ops_per_sec"]),
                    f"{d['wall_time']:.2f}",
                ])
            sections.append(_md_table(headers, rows))
            sections.append("")

    # ── Feature matrix ──
    sections.append("## Feature Comparison\n")
    features = [
        ("Thread-safe (builtin)", "Yes (RwLock)", "No (manual Lock)", "No", "Yes", "Yes", "Yes"),
        ("Async support", "Yes (auto)", "No", "No", "No", "No", "No"),
        ("Cross-process (shared mem)", "Yes (mmap)", "No", "No", "No", "No", "No"),
        ("TTL support", "Yes", "No", "Yes", "FIFO only", "Yes", "No"),
        ("LRU strategy", "Yes", "Yes", "Yes", "Yes", "Yes", "No*"),
        ("LFU strategy", "Yes", "No", "Yes", "Yes", "No", "No"),
        ("FIFO strategy", "Yes", "No", "Yes", "Yes", "No", "No"),
        ("MRU strategy", "Yes", "No", "No", "No", "No", "No"),
        (
            "Implementation", "Rust (PyO3)", "C (CPython)",
            "Pure Python", "Rust (PyO3)", "Rust (PyO3)", "Rust (PyO3)",
        ),
    ]
    headers = [
        "Feature", "fast_cache", "lru_cache",
        "cachetools", "cachebox", "moka_py", "zoocache",
    ]
    rows = [list(f) for f in features]
    sections.append(_md_table(headers, rows))
    sections.append(
        "\n*zoocache uses semantic/dependency-based invalidation"
        " (unbounded cache, no LRU eviction)*\n"
    )

    return "\n".join(sections)


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate benchmark report")
    parser.add_argument(
        "--tags",
        type=str,
        default=None,
        help="Comma-separated list of tags to include (default: all bench_*.json files)",
    )
    args = parser.parse_args()

    tags = [t.strip() for t in args.tags.split(",")] if args.tags else None
    data = _load_results(tags)

    if not data:
        print("No benchmark results found.")
        return

    report = generate_report(data)
    REPORT_PATH.write_text(report)
    print(f"Report written to {REPORT_PATH}")
    print(f"  Datasets: {list(data.keys())}")


if __name__ == "__main__":
    main()
