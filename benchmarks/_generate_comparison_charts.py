#!/usr/bin/env python3
"""Generate comparison charts (warp_cache vs lru_cache vs moka_py).

Produces 4 PNGs in benchmarks/results/:
  - comparison_st_throughput.png   Single-thread throughput (cache=256, 3 Python versions)
  - comparison_mt_scaling.png      Multi-thread scaling (py3.13 GIL vs py3.13t no-GIL)
  - comparison_scaling_ratio.png   Scaling efficiency normalized to 1-thread baseline
  - comparison_backends.png        Backend comparison (memory / shared / multi-process)

Usage:
    uv run python benchmarks/_generate_comparison_charts.py
"""

import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")

import matplotlib.pyplot as plt  # noqa: E402, I001

RESULTS_DIR = Path(__file__).resolve().parent / "results"

LIBS = ["warp_cache", "lru_cache", "moka_py"]
COLORS = {"warp_cache": "#2563eb", "lru_cache": "#ea580c", "moka_py": "#16a34a"}
LABELS = {"warp_cache": "warp_cache", "lru_cache": "lru_cache", "moka_py": "moka_py"}

DPI = 150


def _load(filename: str) -> dict:
    return json.loads((RESULTS_DIR / filename).read_text())


def _millions(val: float) -> float:
    return val / 1_000_000


def _py_label(data: dict) -> str:
    py = data["python"]
    ver = py["version"]
    suffix = "t (no GIL)" if py["gil_disabled"] else ""
    return f"Python {ver}{suffix}"


def chart_single_thread_throughput(py312: dict, py313: dict, py313t: dict) -> None:
    """Chart 1: Grouped bar — single-thread throughput at cache size 256."""
    datasets = [
        ("3.12", py312),
        ("3.13", py313),
        ("3.13t", py313t),
    ]

    fig, ax = plt.subplots(figsize=(8, 5))

    n_groups = len(datasets)
    bar_width = 0.22
    x_positions = range(n_groups)

    for i, lib in enumerate(LIBS):
        values = []
        for _, data in datasets:
            tp = data["throughput"]["256"]
            values.append(_millions(tp.get(lib, 0)))
        offsets = [x + i * bar_width for x in x_positions]
        ax.bar(offsets, values, bar_width, label=LABELS[lib], color=COLORS[lib])
        for x, v in zip(offsets, values, strict=True):
            ax.text(x, v + 0.3, f"{v:.1f}M", ha="center", va="bottom", fontsize=8)

    ax.set_xlabel("Python Version")
    ax.set_ylabel("Throughput (M ops/s)")
    ax.set_title("Single-Thread Throughput (cache size = 256)")
    ax.set_xticks([x + bar_width for x in x_positions])
    ax.set_xticklabels([label for label, _ in datasets])
    ax.legend()
    ax.set_ylim(bottom=0)
    fig.tight_layout()
    fig.savefig(RESULTS_DIR / "comparison_st_throughput.png", dpi=DPI)
    plt.close(fig)
    print("  comparison_st_throughput.png")


def chart_multithread_scaling(py313: dict, py313t: dict) -> None:
    """Chart 2: Dual-panel line — multi-thread scaling (GIL vs no-GIL)."""
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 5), sharey=True)

    panels = [
        (ax1, py313, "Python 3.13 (GIL)"),
        (ax2, py313t, "Python 3.13t (no GIL)"),
    ]

    for ax, data, title in panels:
        th = data["threading"]
        thread_counts = sorted((int(k) for k in th), key=int)
        for lib in LIBS:
            values = []
            for tc in thread_counts:
                val = th[str(tc)].get(lib, 0)
                values.append(_millions(val))
            if any(v > 0 for v in values):
                ax.plot(
                    thread_counts,
                    values,
                    marker="o",
                    label=LABELS[lib],
                    color=COLORS[lib],
                    linewidth=2,
                )
        ax.set_xlabel("Threads")
        ax.set_title(title)
        ax.legend()
        ax.set_xscale("log", base=2)
        ax.set_xticks(thread_counts)
        ax.set_xticklabels([str(t) for t in thread_counts])
        ax.grid(axis="y", alpha=0.3)

    ax1.set_ylabel("Throughput (M ops/s)")
    fig.suptitle("Multi-Thread Scaling", fontsize=14, y=1.02)
    fig.tight_layout()
    fig.savefig(RESULTS_DIR / "comparison_mt_scaling.png", dpi=DPI)
    plt.close(fig)
    print("  comparison_mt_scaling.png")


def chart_scaling_efficiency(py313: dict, py313t: dict) -> None:
    """Chart 3: Dual-panel line — scaling ratio normalized to 1-thread baseline."""
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 5), sharey=True)

    panels = [
        (ax1, py313, "Python 3.13 (GIL)"),
        (ax2, py313t, "Python 3.13t (no GIL)"),
    ]

    for ax, data, title in panels:
        th = data["threading"]
        thread_counts = sorted((int(k) for k in th), key=int)
        for lib in LIBS:
            baseline = th["1"].get(lib, 0)
            if baseline == 0:
                continue
            ratios = []
            for tc in thread_counts:
                val = th[str(tc)].get(lib, 0)
                ratios.append(val / baseline)
            ax.plot(
                thread_counts,
                ratios,
                marker="o",
                label=LABELS[lib],
                color=COLORS[lib],
                linewidth=2,
            )
        ax.axhline(y=1.0, color="gray", linestyle="--", alpha=0.5)
        ax.set_xlabel("Threads")
        ax.set_title(title)
        ax.legend()
        ax.set_xscale("log", base=2)
        ax.set_xticks(thread_counts)
        ax.set_xticklabels([str(t) for t in thread_counts])
        ax.grid(axis="y", alpha=0.3)

    ax1.set_ylabel("Scaling Ratio (vs 1 thread)")
    fig.suptitle("Multi-Thread Scaling Efficiency", fontsize=14, y=1.02)
    fig.tight_layout()
    fig.savefig(RESULTS_DIR / "comparison_scaling_ratio.png", dpi=DPI)
    plt.close(fig)
    print("  comparison_scaling_ratio.png")


def chart_backends(py313: dict) -> None:
    """Chart 4: Grouped bar (log y) — memory / shared / multi-process backends."""
    categories = ["Memory\n(in-process)", "Shared\n(mmap)", "Multi-process\n(8 workers)"]
    values = [
        _millions(py313["shared_throughput"]["memory"]["ops_per_sec"]),
        _millions(py313["shared_throughput"]["shared"]["ops_per_sec"]),
        _millions(py313["multiprocess"]["8"]["total_ops_per_sec"]),
    ]

    fig, ax = plt.subplots(figsize=(7, 5))
    bars = ax.bar(categories, values, color=[COLORS["warp_cache"], "#60a5fa", "#93c5fd"])

    for bar, val in zip(bars, values, strict=True):
        label = f"{val:.2f}M" if val >= 0.1 else f"{val * 1000:.0f}K"
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            bar.get_height(),
            label,
            ha="center",
            va="bottom",
            fontsize=10,
            fontweight="bold",
        )

    ax.set_ylabel("Throughput (M ops/s)")
    ax.set_title("warp_cache Backend Comparison (Python 3.13)")
    ax.set_yscale("log")
    ax.set_ylim(bottom=0.01)
    ax.grid(axis="y", alpha=0.3)
    fig.tight_layout()
    fig.savefig(RESULTS_DIR / "comparison_backends.png", dpi=DPI)
    plt.close(fig)
    print("  comparison_backends.png")


def main() -> None:
    print("Loading benchmark data...")
    py312 = _load("bench_py3.12.json")
    py313 = _load("bench_py3.13.json")
    py313t = _load("bench_default.json")

    print(f"  py3.12:  {_py_label(py312)}")
    print(f"  py3.13:  {_py_label(py313)}")
    print(f"  py3.13t: {_py_label(py313t)}")

    print("\nGenerating charts...")
    chart_single_thread_throughput(py312, py313, py313t)
    chart_multithread_scaling(py313, py313t)
    chart_scaling_efficiency(py313, py313t)
    chart_backends(py313)
    print("\nDone!")


if __name__ == "__main__":
    main()
