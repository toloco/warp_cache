#!/usr/bin/env python3
"""SIEVE eviction quality benchmark — measures hit ratio advantages over LRU.

Synthesizes workload patterns from the NSDI'24 SIEVE paper:
scan resistance, one-hit-wonder filtering, working set adaptivity,
and hit ratio across varying cache sizes and skewness.

Usage:
    python benchmarks/bench_sieve.py                     # full suite, 1M requests
    python benchmarks/bench_sieve.py --quick              # 100K requests
    python benchmarks/bench_sieve.py --bench scan,ohw     # specific benchmarks
    python benchmarks/bench_sieve.py --seed 99            # custom seed
"""

import argparse
import functools
import json
import random
import time
from dataclasses import dataclass
from pathlib import Path

from warp_cache import cache

RESULTS_DIR = Path(__file__).resolve().parent / "results"
RESULTS_DIR.mkdir(parents=True, exist_ok=True)

ALL_BENCHMARKS = ["hitratio", "zipf", "scan", "ohw", "shift", "throughput"]


# ═══════════════════════════════════════════════════════════════════════════
# Workload generators
# ═══════════════════════════════════════════════════════════════════════════


def zipf_keys(n: int, num_keys: int, alpha: float = 1.0, *, seed: int = 42) -> list[int]:
    """Generate *n* keys following a Zipf distribution with configurable skewness."""
    rng = random.Random(seed)
    weights = [1.0 / ((i + 1) ** alpha) for i in range(num_keys)]
    return rng.choices(range(num_keys), weights=weights, k=n)


def scan_resistant_keys(
    n: int,
    hot_size: int,
    scan_size: int,
    hot_fraction: float,
    alpha: float = 1.0,
    *,
    seed: int = 42,
) -> list[int]:
    """Interleave Zipf-distributed hot keys with sequential scan keys.

    hot_fraction controls the mix: 1.0 = all hot, 0.0 = all scan.
    Scan keys are offset by hot_size so they don't overlap.
    """
    rng = random.Random(seed)
    hot_weights = [1.0 / ((i + 1) ** alpha) for i in range(hot_size)]
    scan_seq = list(range(hot_size, hot_size + scan_size))
    scan_idx = 0

    keys = []
    for _ in range(n):
        if rng.random() < hot_fraction:
            keys.append(rng.choices(range(hot_size), weights=hot_weights, k=1)[0])
        else:
            keys.append(scan_seq[scan_idx % len(scan_seq)])
            scan_idx += 1
    return keys


def one_hit_wonder_keys(
    n: int,
    num_reused_keys: int,
    ohw_ratio: float,
    alpha: float = 1.0,
    *,
    seed: int = 42,
) -> list[int]:
    """Mix Zipf-distributed reused keys with unique one-time keys.

    ohw_ratio: fraction of accesses that are unique (one-hit wonders).
    OHW keys start at num_reused_keys and increment, never repeating.
    """
    rng = random.Random(seed)
    reused_weights = [1.0 / ((i + 1) ** alpha) for i in range(num_reused_keys)]
    ohw_counter = num_reused_keys

    keys = []
    for _ in range(n):
        if rng.random() < ohw_ratio:
            keys.append(ohw_counter)
            ohw_counter += 1
        else:
            keys.append(rng.choices(range(num_reused_keys), weights=reused_weights, k=1)[0])
    return keys


def working_set_shift_keys(
    n_per_phase: int,
    set_size: int,
    alpha: float = 1.0,
    *,
    seed: int = 42,
) -> tuple[list[int], list[int], list[int]]:
    """Three phases: keys 0..set_size-1, then set_size..2*set_size-1, then back.

    Returns (phase1, phase2, phase3) key lists.
    """
    rng = random.Random(seed)
    weights = [1.0 / ((i + 1) ** alpha) for i in range(set_size)]
    phase1 = rng.choices(range(set_size), weights=weights, k=n_per_phase)
    phase2 = rng.choices(range(set_size, 2 * set_size), weights=weights, k=n_per_phase)
    phase3 = rng.choices(range(set_size), weights=weights, k=n_per_phase)
    return phase1, phase2, phase3


# ═══════════════════════════════════════════════════════════════════════════
# Cache factories
# ═══════════════════════════════════════════════════════════════════════════


def make_sieve_fn(max_size: int):
    """Create a warp_cache (SIEVE) cached identity function."""

    @cache(max_size=max_size)
    def fn(x):
        return x

    return fn


def make_lru_fn(max_size: int):
    """Create a functools.lru_cache (LRU) cached identity function."""

    @functools.lru_cache(maxsize=max_size)
    def fn(x):
        return x

    return fn


# ═══════════════════════════════════════════════════════════════════════════
# Measurement helpers
# ═══════════════════════════════════════════════════════════════════════════


@dataclass
class HitRatioResult:
    name: str
    hit_ratio: float
    hits: int
    misses: int
    ops_per_sec: float


def _get_info(fn):
    """Get (hits, misses) from either warp_cache or functools cache_info."""
    info = fn.cache_info()
    return info.hits, info.misses


def measure_hit_ratio(fn, keys: list[int], name: str) -> HitRatioResult:
    """Run keys through fn, return hit ratio stats."""
    t0 = time.perf_counter()
    for k in keys:
        fn(k)
    elapsed = time.perf_counter() - t0

    hits, misses = _get_info(fn)
    total = hits + misses
    return HitRatioResult(
        name=name,
        hit_ratio=hits / total if total else 0.0,
        hits=hits,
        misses=misses,
        ops_per_sec=len(keys) / elapsed,
    )


def measure_phase_hit_ratio(fn, keys: list[int]) -> tuple[int, int]:
    """Run keys through fn and return (hits_delta, misses_delta) for this phase."""
    h0, m0 = _get_info(fn)
    for k in keys:
        fn(k)
    h1, m1 = _get_info(fn)
    return h1 - h0, m1 - m0


def measure_windowed_hit_ratio(fn, keys: list[int], window_size: int = 10_000) -> list[float]:
    """Run keys through fn, return per-window hit ratios."""
    ratios = []
    for start in range(0, len(keys), window_size):
        chunk = keys[start : start + window_size]
        h0, m0 = _get_info(fn)
        for k in chunk:
            fn(k)
        h1, m1 = _get_info(fn)
        dh, dm = h1 - h0, m1 - m0
        total = dh + dm
        ratios.append(dh / total if total else 0.0)
    return ratios


# ═══════════════════════════════════════════════════════════════════════════
# Formatting
# ═══════════════════════════════════════════════════════════════════════════


def fmt_pct(v: float) -> str:
    return f"{v * 100:6.2f}%"


def fmt_ops(ops: float) -> str:
    if ops >= 1_000_000:
        return f"{ops / 1_000_000:.2f}M"
    if ops >= 1_000:
        return f"{ops / 1_000:.0f}K"
    return f"{ops:.0f}"


def fmt_delta(sieve_ratio: float, lru_ratio: float) -> str:
    """Format the miss ratio reduction of SIEVE vs LRU."""
    sieve_miss = 1 - sieve_ratio
    lru_miss = 1 - lru_ratio
    if lru_miss == 0:
        return "  n/a"
    reduction = (lru_miss - sieve_miss) / lru_miss * 100
    return f"{reduction:+.1f}%"


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 1 — Hit ratio vs cache size ratio
# ═══════════════════════════════════════════════════════════════════════════


def bench_hitratio(n_ops: int, seed: int) -> dict:
    num_keys = 10_000
    ratios = [0.001, 0.005, 0.01, 0.05, 0.10, 0.25, 0.50]
    keys = zipf_keys(n_ops, num_keys, alpha=1.0, seed=seed)

    results = []
    print("\n  Cache%   SIEVE     LRU    MissReduction")
    print("  " + "─" * 42)

    for r in ratios:
        sz = max(1, int(num_keys * r))
        sieve_fn = make_sieve_fn(sz)
        lru_fn = make_lru_fn(sz)

        s = measure_hit_ratio(sieve_fn, keys, "sieve")
        lr = measure_hit_ratio(lru_fn, keys, "lru")

        delta = fmt_delta(s.hit_ratio, lr.hit_ratio)
        print(f"  {r * 100:5.1f}%  {fmt_pct(s.hit_ratio)}  {fmt_pct(lr.hit_ratio)}    {delta}")

        results.append(
            {
                "cache_ratio": r,
                "cache_size": sz,
                "sieve_hit_ratio": s.hit_ratio,
                "lru_hit_ratio": lr.hit_ratio,
            }
        )

    return {"num_keys": num_keys, "n_ops": n_ops, "results": results}


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 2 — Zipf skewness sweep
# ═══════════════════════════════════════════════════════════════════════════


def bench_zipf(n_ops: int, seed: int) -> dict:
    num_keys = 10_000
    cache_size = num_keys // 10  # 10% of unique keys
    alphas = [0.5, 0.7, 0.8, 1.0, 1.2, 1.5]

    results = []
    print("\n  Alpha   SIEVE     LRU    MissReduction")
    print("  " + "─" * 42)

    for alpha in alphas:
        keys = zipf_keys(n_ops, num_keys, alpha=alpha, seed=seed)
        sieve_fn = make_sieve_fn(cache_size)
        lru_fn = make_lru_fn(cache_size)

        s = measure_hit_ratio(sieve_fn, keys, "sieve")
        lr = measure_hit_ratio(lru_fn, keys, "lru")

        delta = fmt_delta(s.hit_ratio, lr.hit_ratio)
        print(f"  {alpha:5.2f}  {fmt_pct(s.hit_ratio)}  {fmt_pct(lr.hit_ratio)}    {delta}")

        results.append(
            {
                "alpha": alpha,
                "sieve_hit_ratio": s.hit_ratio,
                "lru_hit_ratio": lr.hit_ratio,
            }
        )

    return {"num_keys": num_keys, "cache_size": cache_size, "n_ops": n_ops, "results": results}


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 3 — Scan resistance
# ═══════════════════════════════════════════════════════════════════════════


def bench_scan(n_ops: int, seed: int) -> dict:
    hot_size = 100
    scan_size = 10_000
    cache_size = 200  # can hold entire hot set
    hot_fractions = [1.0, 0.9, 0.8, 0.7, 0.5, 0.3]

    results = []
    print("\n  HotFrac  SIEVE     LRU    MissReduction")
    print("  " + "─" * 44)

    for hf in hot_fractions:
        keys = scan_resistant_keys(n_ops, hot_size, scan_size, hf, seed=seed)
        sieve_fn = make_sieve_fn(cache_size)
        lru_fn = make_lru_fn(cache_size)

        s = measure_hit_ratio(sieve_fn, keys, "sieve")
        lr = measure_hit_ratio(lru_fn, keys, "lru")

        delta = fmt_delta(s.hit_ratio, lr.hit_ratio)
        print(f"  {hf * 100:5.1f}%  {fmt_pct(s.hit_ratio)}  {fmt_pct(lr.hit_ratio)}    {delta}")

        results.append(
            {
                "hot_fraction": hf,
                "sieve_hit_ratio": s.hit_ratio,
                "lru_hit_ratio": lr.hit_ratio,
            }
        )

    return {
        "hot_size": hot_size,
        "scan_size": scan_size,
        "cache_size": cache_size,
        "n_ops": n_ops,
        "results": results,
    }


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 4 — One-hit-wonder filtering
# ═══════════════════════════════════════════════════════════════════════════


def bench_ohw(n_ops: int, seed: int) -> dict:
    num_reused_keys = 5_000
    cache_size = 500
    ohw_ratios = [0.0, 0.25, 0.50, 0.75]

    results = []
    print("\n  OHW%    SIEVE     LRU    MissReduction")
    print("  " + "─" * 42)

    for ohw in ohw_ratios:
        keys = one_hit_wonder_keys(n_ops, num_reused_keys, ohw, seed=seed)
        sieve_fn = make_sieve_fn(cache_size)
        lru_fn = make_lru_fn(cache_size)

        s = measure_hit_ratio(sieve_fn, keys, "sieve")
        lr = measure_hit_ratio(lru_fn, keys, "lru")

        delta = fmt_delta(s.hit_ratio, lr.hit_ratio)
        print(f"  {ohw * 100:5.1f}%  {fmt_pct(s.hit_ratio)}  {fmt_pct(lr.hit_ratio)}    {delta}")

        results.append(
            {
                "ohw_ratio": ohw,
                "sieve_hit_ratio": s.hit_ratio,
                "lru_hit_ratio": lr.hit_ratio,
            }
        )

    return {
        "num_reused_keys": num_reused_keys,
        "cache_size": cache_size,
        "n_ops": n_ops,
        "results": results,
    }


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 5 — Working set shift
# ═══════════════════════════════════════════════════════════════════════════


def bench_shift(n_ops: int, seed: int) -> dict:
    set_size = 1_000
    cache_size = 200
    n_per_phase = n_ops // 2  # split across 3 phases (slightly uneven is fine)
    window_size = max(1_000, n_per_phase // 50)

    p1, p2, p3 = working_set_shift_keys(n_per_phase, set_size, seed=seed)

    results = {}
    print()

    for label, impl_factory in [("sieve", make_sieve_fn), ("lru", make_lru_fn)]:
        fn = impl_factory(cache_size)

        phase_results = {}
        all_windowed = []

        for phase_name, phase_keys in [("phase1", p1), ("phase2", p2), ("phase3", p3)]:
            dh, dm = measure_phase_hit_ratio(fn, phase_keys)
            total = dh + dm
            hr = dh / total if total else 0.0
            phase_results[phase_name] = hr

            w = measure_windowed_hit_ratio(fn, phase_keys, window_size)
            all_windowed.extend(w)

        results[label] = {
            "phases": phase_results,
            "windowed": all_windowed,
        }

        print(
            f"  {label.upper():>5}  "
            f"P1={fmt_pct(phase_results['phase1'])}  "
            f"P2={fmt_pct(phase_results['phase2'])}  "
            f"P3={fmt_pct(phase_results['phase3'])}"
        )

    return {
        "set_size": set_size,
        "cache_size": cache_size,
        "n_per_phase": n_per_phase,
        "window_size": window_size,
        "results": results,
    }


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 6 — Throughput under eviction pressure
# ═══════════════════════════════════════════════════════════════════════════


def bench_throughput(n_ops: int, seed: int) -> dict:
    cache_size = 64
    num_keys = 10_000
    # Use 2x ops for throughput to get stable numbers
    actual_ops = n_ops * 2
    keys = zipf_keys(actual_ops, num_keys, seed=seed)

    results = {}
    print()

    for label, factory in [("sieve", make_sieve_fn), ("lru", make_lru_fn)]:
        fn = factory(cache_size)

        t0 = time.perf_counter()
        for k in keys:
            fn(k)
        elapsed = time.perf_counter() - t0

        ops_sec = actual_ops / elapsed
        hits, misses = _get_info(fn)
        total = hits + misses
        hr = hits / total if total else 0.0

        results[label] = {
            "ops_per_sec": ops_sec,
            "hit_ratio": hr,
            "elapsed": elapsed,
        }

        print(f"  {label.upper():>5}  {fmt_ops(ops_sec)} ops/s  hit_ratio={fmt_pct(hr)}")

    return {
        "cache_size": cache_size,
        "num_keys": num_keys,
        "n_ops": actual_ops,
        "results": results,
    }


# ═══════════════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════════════


BENCH_DISPATCH = {
    "hitratio": ("Hit ratio vs cache size", bench_hitratio),
    "zipf": ("Zipf skewness sweep", bench_zipf),
    "scan": ("Scan resistance", bench_scan),
    "ohw": ("One-hit-wonder filtering", bench_ohw),
    "shift": ("Working set shift", bench_shift),
    "throughput": ("Throughput under eviction pressure", bench_throughput),
}


def main() -> None:
    parser = argparse.ArgumentParser(description="SIEVE eviction quality benchmark")
    parser.add_argument("--quick", action="store_true", help="Use 100K requests instead of 1M")
    parser.add_argument(
        "--bench",
        type=str,
        default=None,
        help="Comma-separated benchmarks to run (default: all). "
        f"Options: {','.join(ALL_BENCHMARKS)}",
    )
    parser.add_argument("--seed", type=int, default=42, help="Random seed (default: 42)")
    args = parser.parse_args()

    n_ops = 100_000 if args.quick else 1_000_000
    seed = args.seed
    selected = args.bench.split(",") if args.bench else ALL_BENCHMARKS

    for name in selected:
        if name not in BENCH_DISPATCH:
            parser.error(f"Unknown benchmark: {name!r}. Options: {','.join(ALL_BENCHMARKS)}")

    mode = "quick" if args.quick else "full"
    print(f"SIEVE eviction quality benchmark ({mode}, {n_ops:,} ops, seed={seed})")
    print("=" * 60)

    all_results = {"n_ops": n_ops, "seed": seed, "mode": mode}

    for i, name in enumerate(selected, 1):
        title, bench_fn = BENCH_DISPATCH[name]
        print(f"\n[{i}/{len(selected)}] {title}")
        all_results[name] = bench_fn(n_ops, seed)

    json_path = RESULTS_DIR / "bench_sieve.json"
    json_path.write_text(json.dumps(all_results, indent=2))
    print(f"\nResults saved to {json_path}")


if __name__ == "__main__":
    main()
