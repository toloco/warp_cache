#!/usr/bin/env python3
"""Unified benchmark runner for warp_cache vs multiple competitors.

Executed inside each uv venv by bench_all.sh or directly via make bench.

Usage:
    python _bench_runner.py --tag py3.12
    python _bench_runner.py --tag py3.13t --quick
"""

import argparse
import functools
import json
import platform
import random
import sys
import sysconfig
import threading
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

RESULTS_DIR = Path(__file__).resolve().parent / "results"
RESULTS_DIR.mkdir(parents=True, exist_ok=True)


# ═══════════════════════════════════════════════════════════════════════════
# Contestant abstraction
# ═══════════════════════════════════════════════════════════════════════════


@dataclass
class Contestant:
    name: str
    make_lru: Callable[[int], Callable] | None = None
    make_ttl: Callable[[int, float], Callable] | None = None
    thread_safe: bool = False
    available: bool = False
    version: str = ""
    notes: list[str] = field(default_factory=list)


def _identity(x: int) -> int:
    return x


def _build_contestants() -> list[Contestant]:
    contestants: list[Contestant] = []

    # 1. warp_cache (always available — this is the project under test)
    from warp_cache import Strategy, cache

    contestants.append(
        Contestant(
            name="warp_cache",
            make_lru=lambda sz: cache(strategy=Strategy.LRU, max_size=sz)(_identity),
            make_ttl=lambda sz, ttl: cache(strategy=Strategy.LRU, max_size=sz, ttl=ttl)(_identity),
            thread_safe=True,
            available=True,
            version="0.1.0",
        )
    )

    # 2. functools.lru_cache (stdlib, always available)
    contestants.append(
        Contestant(
            name="lru_cache",
            make_lru=lambda sz: functools.lru_cache(maxsize=sz)(_identity),
            make_ttl=None,
            thread_safe=False,
            available=True,
            version=sys.version.split()[0],
        )
    )

    # 3. cachetools
    try:
        import cachetools
        from cachetools.func import lru_cache as ct_lru_cache
        from cachetools.func import ttl_cache as ct_ttl_cache

        contestants.append(
            Contestant(
                name="cachetools",
                make_lru=lambda sz: ct_lru_cache(maxsize=sz)(_identity),
                make_ttl=lambda sz, ttl: ct_ttl_cache(maxsize=sz, ttl=ttl)(_identity),
                thread_safe=False,
                available=True,
                version=cachetools.__version__,
            )
        )
    except ImportError:
        contestants.append(Contestant(name="cachetools"))

    # 4. cachebox
    try:
        import cachebox

        def _cachebox_lru(sz):
            @cachebox.cached(cachebox.LRUCache(maxsize=sz))
            def fn(x: int) -> int:
                return x

            return fn

        contestants.append(
            Contestant(
                name="cachebox",
                make_lru=_cachebox_lru,
                make_ttl=None,
                thread_safe=True,
                available=True,
                version=cachebox.__version__,
                notes=["TTL only via TTLCache (FIFO, not LRU)"],
            )
        )
    except ImportError:
        contestants.append(Contestant(name="cachebox"))

    # 5. moka-py
    try:
        import moka_py

        def _moka_lru(sz):
            @moka_py.cached(maxsize=sz)
            def fn(x: int) -> int:
                return x

            return fn

        def _moka_ttl(sz, ttl):
            @moka_py.cached(maxsize=sz, ttl=ttl)
            def fn(x: int) -> int:
                return x

            return fn

        contestants.append(
            Contestant(
                name="moka_py",
                make_lru=_moka_lru,
                make_ttl=_moka_ttl,
                thread_safe=True,
                available=True,
                version=getattr(moka_py, "VERSION", ""),
            )
        )
    except ImportError:
        contestants.append(Contestant(name="moka_py"))

    # 6. zoocache
    try:
        import zoocache

        def _zoo_lru(_sz):
            """ZooCache has no maxsize — caches everything (unbounded)."""

            @zoocache.cacheable
            def fn(x: int) -> int:
                return x

            return fn

        contestants.append(
            Contestant(
                name="zoocache",
                make_lru=_zoo_lru,
                make_ttl=None,
                thread_safe=True,
                available=True,
                version=getattr(zoocache, "__version__", ""),
                notes=["No maxsize param (unbounded cache)", "Semantic invalidation, not LRU"],
            )
        )
    except ImportError:
        contestants.append(Contestant(name="zoocache"))

    return contestants


# ═══════════════════════════════════════════════════════════════════════════
# Environment info
# ═══════════════════════════════════════════════════════════════════════════


def python_info() -> dict:
    """Collect Python build/runtime details."""
    gil_disabled = getattr(sys.flags, "nogil", False) or sysconfig.get_config_var("Py_GIL_DISABLED")
    return {
        "version": sys.version.split()[0],
        "implementation": platform.python_implementation(),
        "build": platform.python_build()[0],
        "compiler": platform.python_compiler(),
        "arch": platform.machine(),
        "gil_disabled": bool(gil_disabled),
    }


# ═══════════════════════════════════════════════════════════════════════════
# Helpers
# ═══════════════════════════════════════════════════════════════════════════


def zipf_keys(n: int, num_keys: int, *, seed: int = 42) -> list[int]:
    """Generate *n* keys following a Zipf-like distribution."""
    rng = random.Random(seed)
    weights = [1.0 / (i + 1) for i in range(num_keys)]
    return rng.choices(range(num_keys), weights=weights, k=n)


def fmt(ops: float) -> str:
    if ops >= 1_000_000:
        return f"{ops / 1_000_000:>7.2f}M"
    if ops >= 1_000:
        return f"{ops / 1_000:>7.0f}K"
    return f"{ops:>7.0f} "


def ratio_str(a: float, b: float) -> str:
    if b == 0:
        return "  inf"
    return f"{a / b:.2f}x"


def _time_loop(fn, keys: list[int]) -> float:
    """Time a cache function over a list of keys, return elapsed seconds."""
    t0 = time.perf_counter()
    for k in keys:
        fn(k)
    return time.perf_counter() - t0


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 1 — Correctness verification
# ═══════════════════════════════════════════════════════════════════════════


def verify_correctness(n_ops: int = 50_000) -> bool:
    from warp_cache import Strategy, cache

    max_size = 256
    num_keys = 500

    @cache(strategy=Strategy.LRU, max_size=max_size)
    def fc_fn(x: int) -> int:
        return x * 7 + 3

    @functools.lru_cache(maxsize=max_size)
    def lru_fn(x: int) -> int:
        return x * 7 + 3

    rng = random.Random(99)
    for _ in range(n_ops):
        k = rng.randint(0, num_keys - 1)
        fc_val = fc_fn(k)
        lru_val = lru_fn(k)
        if fc_val != lru_val:
            print(f"MISMATCH at key={k}: warp_cache={fc_val}, lru_cache={lru_val}")
            return False

    return True


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 2 — Single-thread throughput vs cache size
# ═══════════════════════════════════════════════════════════════════════════


def bench_throughput(
    contestants: list[Contestant],
    cache_sizes: list[int],
    n_ops: int = 100_000,
) -> dict:
    num_keys = 2000
    keys = zipf_keys(n_ops, num_keys)
    results: dict[str, dict[str, float]] = {}

    # zoocache has no maxsize — run it once and report separately
    zoo_contestant = next((c for c in contestants if c.name == "zoocache" and c.available), None)
    regular = [c for c in contestants if c.available and c.name != "zoocache"]

    for sz in cache_sizes:
        sz_results: dict[str, float] = {}
        for c in regular:
            fn = c.make_lru(sz)
            elapsed = _time_loop(fn, keys)
            sz_results[c.name] = n_ops / elapsed
        results[str(sz)] = sz_results

    if zoo_contestant:
        fn = zoo_contestant.make_lru(0)
        elapsed = _time_loop(fn, keys)
        results["zoocache_unbounded"] = {"zoocache": n_ops / elapsed}

    return results


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 3 — Multi-thread scaling
# ═══════════════════════════════════════════════════════════════════════════


def bench_threading(
    contestants: list[Contestant],
    thread_counts: list[int],
    n_ops: int = 100_000,
    max_size: int = 256,
) -> dict:
    num_keys = 2000
    results: dict[str, dict[str, float]] = {}

    active = [c for c in contestants if c.available and c.name != "zoocache"]

    for n_threads in thread_counts:
        ops_per_thread = n_ops // n_threads
        keys_per_thread = zipf_keys(ops_per_thread, num_keys)
        tc_results: dict[str, float] = {}

        for c in active:
            fn = c.make_lru(max_size)

            if c.thread_safe:

                def worker(f=fn):
                    for k in keys_per_thread:
                        f(k)
            else:
                lock = threading.Lock()

                def worker(f=fn, lk=lock):
                    for k in keys_per_thread:
                        with lk:
                            f(k)

            t0 = time.perf_counter()
            with ThreadPoolExecutor(max_workers=n_threads) as pool:
                futs = [pool.submit(worker) for _ in range(n_threads)]
                for f in futs:
                    f.result()
            elapsed = time.perf_counter() - t0

            total_ops = ops_per_thread * n_threads
            tc_results[c.name] = total_ops / elapsed

        results[str(n_threads)] = tc_results

    return results


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 4 — Sustained throughput (~10s time-based)
# ═══════════════════════════════════════════════════════════════════════════


def bench_sustained(
    contestants: list[Contestant],
    duration: float = 10.0,
    max_size: int = 256,
) -> dict[str, dict[str, float]]:
    num_keys = 2000
    keys = zipf_keys(1_000_000, num_keys)
    n_keys = len(keys)
    results: dict[str, dict[str, float]] = {}

    active = [c for c in contestants if c.available and c.name != "zoocache"]

    for c in active:
        fn = c.make_lru(max_size)
        deadline = time.perf_counter() + duration
        ops = 0
        idx = 0
        t0 = time.perf_counter()
        while time.perf_counter() < deadline:
            fn(keys[idx])
            ops += 1
            idx += 1
            if idx >= n_keys:
                idx = 0
        elapsed = time.perf_counter() - t0
        results[c.name] = {"ops": ops, "elapsed": elapsed, "ops_per_sec": ops / elapsed}

    return results


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 5 — TTL throughput
# ═══════════════════════════════════════════════════════════════════════════


def bench_ttl(
    contestants: list[Contestant],
    ttl_values: list[float | None] | None = None,
    duration: float = 10.0,
    max_size: int = 256,
) -> dict[str, dict[str, dict[str, float]]]:
    if ttl_values is None:
        ttl_values = [0.001, 0.01, 0.1, 1.0, None]

    num_keys = 2000
    keys = zipf_keys(1_000_000, num_keys)
    n_keys = len(keys)
    results: dict[str, dict[str, dict[str, float]]] = {}

    ttl_contestants = [c for c in contestants if c.available and c.make_ttl is not None]

    for ttl in ttl_values:
        ttl_label = "None" if ttl is None else str(ttl)
        ttl_results: dict[str, dict[str, float]] = {}

        for c in ttl_contestants:
            fn = c.make_ttl(max_size, ttl if ttl is not None else 3600.0)

            deadline = time.perf_counter() + duration
            ops = 0
            idx = 0
            t0 = time.perf_counter()
            while time.perf_counter() < deadline:
                fn(keys[idx])
                ops += 1
                idx += 1
                if idx >= n_keys:
                    idx = 0
            elapsed = time.perf_counter() - t0

            ttl_results[c.name] = {"ops_per_sec": ops / elapsed}

        results[ttl_label] = ttl_results

    return results


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 6 — Shared backend: single-process throughput
# ═══════════════════════════════════════════════════════════════════════════


def bench_shared_throughput(
    n_ops: int = 100_000, max_size: int = 256
) -> dict[str, dict[str, float]]:
    from warp_cache import Strategy, cache

    num_keys = 2000
    keys = zipf_keys(n_ops, num_keys)
    results: dict[str, dict[str, float]] = {}

    for backend in ("memory", "shared"):

        @cache(strategy=Strategy.LRU, max_size=max_size, backend=backend)
        def fn(x: int) -> int:
            return x

        t0 = time.perf_counter()
        for k in keys:
            fn(k)
        elapsed = time.perf_counter() - t0

        info = fn.cache_info()
        total = info.hits + info.misses
        hit_rate = info.hits / total if total else 0.0

        results[backend] = {"ops_per_sec": n_ops / elapsed, "hit_rate": hit_rate}

    return results


# ═══════════════════════════════════════════════════════════════════════════
# Benchmark 7 — Shared backend: multi-process scaling
# ═══════════════════════════════════════════════════════════════════════════


def _mp_worker(args):
    """Worker for multiprocess benchmark. Runs in a forked child."""
    shm_name, n_ops, num_keys, seed = args
    from warp_cache._warp_cache_rs import SharedCachedFunction

    fn = SharedCachedFunction(
        lambda x: x,
        0,
        512,
        None,
        512,
        4096,
        shm_name,
    )
    keys = zipf_keys(n_ops, num_keys, seed=seed)
    t0 = time.perf_counter()
    for k in keys:
        fn(k)
    elapsed = time.perf_counter() - t0
    return n_ops / elapsed


def bench_multiprocess(
    process_counts: list[int], n_ops: int = 500_000, max_size: int = 512
) -> dict[str, dict[str, float]]:
    import multiprocessing
    import os
    import tempfile

    num_keys = 2000
    results: dict[str, dict[str, float]] = {}

    for n_procs in process_counts:
        shm_name = f"bench_multiproc_{n_procs}"
        tmpdir = tempfile.gettempdir()
        shm_dir = os.path.join(tmpdir, "warp_cache")
        for suffix in (".data", ".lock"):
            p = os.path.join(shm_dir, f"{shm_name}{suffix}")
            if os.path.exists(p):
                os.unlink(p)

        from warp_cache._warp_cache_rs import SharedCachedFunction

        _init_fn = SharedCachedFunction(
            lambda x: x,
            0,
            max_size,
            None,
            512,
            4096,
            shm_name,
        )
        del _init_fn

        ops_per_proc = n_ops // n_procs
        worker_args = [(shm_name, ops_per_proc, num_keys, 42 + i) for i in range(n_procs)]

        ctx = multiprocessing.get_context("fork")
        t0 = time.perf_counter()
        with ctx.Pool(n_procs) as pool:
            per_proc_rates = pool.map(_mp_worker, worker_args)
        wall_elapsed = time.perf_counter() - t0

        total_ops = ops_per_proc * n_procs
        results[str(n_procs)] = {
            "total_ops_per_sec": total_ops / wall_elapsed,
            "per_process_avg_ops_per_sec": sum(per_proc_rates) / len(per_proc_rates),
            "wall_time": wall_elapsed,
        }

        for suffix in (".data", ".lock"):
            p = os.path.join(shm_dir, f"{shm_name}{suffix}")
            if os.path.exists(p):
                os.unlink(p)

    return results


# ═══════════════════════════════════════════════════════════════════════════
# Main
# ═══════════════════════════════════════════════════════════════════════════


def main() -> None:
    parser = argparse.ArgumentParser(description="warp_cache benchmark runner")
    parser.add_argument("--tag", required=True, help="Label for this run (e.g. py3.12)")
    parser.add_argument("--quick", action="store_true", help="Skip sustained & TTL benchmarks")
    args = parser.parse_args()

    info = python_info()
    contestants = _build_contestants()
    available = [c for c in contestants if c.available]
    unavailable = [c for c in contestants if not c.available]

    total_steps = 5 if args.quick else 7

    tag_suffix = " (free-threaded)" if info["gil_disabled"] else ""
    print(f"Python {info['version']}{tag_suffix}  [{info['implementation']}]")
    print(f"{info['compiler']}")
    print(f"{info['arch']}")
    if args.quick:
        print("(--quick mode: skipping sustained & TTL benchmarks)")

    print(f"\nContestants ({len(available)} available):")
    for c in available:
        notes = f"  ({', '.join(c.notes)})" if c.notes else ""
        print(f"  {c.name} v{c.version}{notes}")
    if unavailable:
        print(f"  Skipped (not installed): {', '.join(c.name for c in unavailable)}")

    # 1. Correctness
    print(f"\n[1/{total_steps}] Correctness verification ...")
    ok = verify_correctness()
    print(f"  Result: {'PASS' if ok else 'FAIL'}")
    if not ok:
        sys.exit(1)

    # 2. Single-thread throughput
    cache_sizes = [32, 64, 128, 256, 512, 1024]
    print(f"\n[2/{total_steps}] Single-thread throughput vs cache size ...")
    tp_results = bench_throughput(contestants, cache_sizes)
    for sz in cache_sizes:
        parts = []
        for name, ops in tp_results[str(sz)].items():
            parts.append(f"{name}={fmt(ops)}")
        print(f"  size={sz:>5}  {' '.join(parts)}")
    if "zoocache_unbounded" in tp_results:
        ops = tp_results["zoocache_unbounded"]["zoocache"]
        print(f"  zoocache (unbounded): {fmt(ops)}")

    # 3. Multi-thread scaling
    thread_counts = [1, 2, 4, 8, 16, 32]
    print(f"\n[3/{total_steps}] Multi-thread scaling ...")
    th_results = bench_threading(contestants, thread_counts)
    for nt in thread_counts:
        parts = []
        for name, ops in th_results[str(nt)].items():
            parts.append(f"{name}={fmt(ops)}")
        print(f"  threads={nt:>2}  {' '.join(parts)}")

    # 4. Sustained throughput
    sustained_results = None
    if not args.quick:
        print(f"\n[4/{total_steps}] Sustained throughput (~10s per impl) ...")
        sustained_results = bench_sustained(contestants)
        for label, data in sustained_results.items():
            print(f"  {label}: {data['ops_per_sec']:,.0f} ops/s ({data['elapsed']:.2f}s)")

    # 5. TTL throughput
    ttl_results = None
    if not args.quick:
        print(f"\n[5/{total_steps}] TTL throughput (~10s per TTL per impl) ...")
        ttl_results = bench_ttl(contestants)
        for ttl_label, ttl_data in ttl_results.items():
            parts = []
            for name, d in ttl_data.items():
                parts.append(f"{name}={fmt(d['ops_per_sec'])}")
            print(f"  TTL={ttl_label}: {' '.join(parts)}")

    # 6. Shared backend single-process
    step = 4 if args.quick else 6
    print(f"\n[{step}/{total_steps}] Shared backend: memory vs shared ...")
    shared_tp_results = bench_shared_throughput()
    for backend, data in shared_tp_results.items():
        print(f"  {backend}: {data['ops_per_sec']:,.0f} ops/s  hit_rate={data['hit_rate']:.1%}")

    # 7. Multi-process scaling
    step = 5 if args.quick else 7
    process_counts = [1, 2, 4, 8]
    print(f"\n[{step}/{total_steps}] Shared backend: multi-process scaling ...")
    mp_results = bench_multiprocess(process_counts)
    for np_str, d in mp_results.items():
        print(
            f"  procs={np_str}: total={d['total_ops_per_sec']:,.0f} ops/s"
            f"  wall={d['wall_time']:.2f}s"
        )

    # Save JSON
    contestant_info = {
        c.name: {"version": c.version, "available": c.available, "thread_safe": c.thread_safe}
        for c in contestants
    }
    payload: dict = {
        "python": info,
        "contestants": contestant_info,
        "throughput": tp_results,
        "threading": th_results,
        "shared_throughput": shared_tp_results,
        "multiprocess": mp_results,
    }
    if sustained_results is not None:
        payload["sustained"] = sustained_results
    if ttl_results is not None:
        payload["ttl"] = ttl_results

    json_path = RESULTS_DIR / f"bench_{args.tag}.json"
    json_path.write_text(json.dumps(payload, indent=2))
    print(f"\nResults saved to {json_path}")


if __name__ == "__main__":
    main()
