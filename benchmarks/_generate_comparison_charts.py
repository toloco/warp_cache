#!/usr/bin/env python3
"""Generate comparison charts (warp_cache vs lru_cache vs moka_py).

Produces SVGs in benchmarks/results/ with light + dark variants:
  - comparison_st_throughput_{light,dark}.svg
  - comparison_mt_scaling_{light,dark}.svg
  - comparison_scaling_ratio_{light,dark}.svg
  - comparison_backends_{light,dark}.svg

Usage:
    uv run python benchmarks/_generate_comparison_charts.py
"""

import json
from dataclasses import dataclass
from pathlib import Path

import matplotlib
import matplotlib.patheffects as pe

matplotlib.use("Agg")

import matplotlib.pyplot as plt  # noqa: E402, I001

RESULTS_DIR = Path(__file__).resolve().parent / "results"

LIBS = ["warp_cache", "lru_cache", "moka_py"]
LABELS = {"warp_cache": "warp_cache", "lru_cache": "lru_cache", "moka_py": "moka_py"}


@dataclass
class Theme:
    name: str
    text: str
    text_dim: str
    grid: str
    warp_cache: str
    lru_cache: str
    moka_py: str
    backend_colors: list[str]


LIGHT = Theme(
    name="light",
    text="#24292f",
    text_dim="#656d76",
    grid=(0.5, 0.5, 0.5, 0.25),
    warp_cache="#4f46e5",
    lru_cache="#d97706",
    moka_py="#059669",
    backend_colors=["#4f46e5", "#6366f1", "#818cf8"],
)

DARK = Theme(
    name="dark",
    text="#e6edf3",
    text_dim="#8b949e",
    grid=(0.5, 0.5, 0.5, 0.25),
    warp_cache="#818cf8",
    lru_cache="#fbbf24",
    moka_py="#34d399",
    backend_colors=["#818cf8", "#a5b4fc", "#c7d2fe"],
)


def _apply_theme(theme: Theme) -> None:
    plt.rcdefaults()
    plt.xkcd(scale=0.3, length=200, randomness=1)
    # xkcd() adds white stroke outlines — use dark stroke for dark mode
    stroke_color = "white" if theme.name == "light" else "#0d1117"
    plt.rcParams.update(
        {
            "figure.facecolor": "none",
            "axes.facecolor": "none",
            "savefig.facecolor": "none",
            "axes.edgecolor": theme.text_dim,
            "axes.labelcolor": theme.text,
            "axes.titlecolor": theme.text,
            "text.color": theme.text,
            "xtick.color": theme.text_dim,
            "ytick.color": theme.text_dim,
            "legend.facecolor": "none",
            "legend.edgecolor": "none",
            "legend.labelcolor": theme.text,
            "grid.color": theme.grid,
            "grid.alpha": 1.0,
            "font.size": 11,
            "axes.titlesize": 13,
            "axes.labelsize": 11,
            "figure.titlesize": 15,
            "axes.linewidth": 1.0,
            "lines.linewidth": 1.5,
            "path.effects": [pe.withStroke(linewidth=4, foreground=stroke_color)],
            "svg.fonttype": "none",
        }
    )


def _colors(theme: Theme) -> dict[str, str]:
    return {"warp_cache": theme.warp_cache, "lru_cache": theme.lru_cache, "moka_py": theme.moka_py}


def _style_ax(ax: plt.Axes) -> None:
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.grid(axis="y", linewidth=0.5)
    ax.tick_params(length=0)


def _load(filename: str) -> dict:
    return json.loads((RESULTS_DIR / filename).read_text())


def _millions(val: float) -> float:
    return val / 1_000_000


def _py_label(data: dict) -> str:
    py = data["python"]
    ver = py["version"]
    suffix = "t (no GIL)" if py["gil_disabled"] else ""
    return f"Python {ver}{suffix}"


def _save(fig: plt.Figure, name: str) -> None:
    fig.savefig(RESULTS_DIR / name, format="svg", bbox_inches="tight", transparent=True)
    plt.close(fig)
    print(f"  {name}")


def chart_single_thread_throughput(
    py312: dict,
    py313: dict,
    py313t: dict,
    theme: Theme,
) -> None:
    datasets = [("3.12", py312), ("3.13", py313), ("3.13t", py313t)]
    colors = _colors(theme)

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
        ax.bar(offsets, values, bar_width, label=LABELS[lib], color=colors[lib])
        for x, v in zip(offsets, values, strict=True):
            ax.text(
                x,
                v + 0.4,
                f"{v:.1f}M",
                ha="center",
                va="bottom",
                fontsize=9,
                color=colors[lib],
            )

    ax.set_xlabel("Python Version")
    ax.set_ylabel("Throughput (M ops/s)")
    ax.set_title("Single-Thread Throughput  (cache size = 256)", pad=10)
    ax.set_xticks([x + bar_width for x in x_positions])
    ax.set_xticklabels([label for label, _ in datasets])
    ax.legend(ncol=3, loc="upper center", bbox_to_anchor=(0.5, -0.12), frameon=False)
    ax.set_ylim(bottom=0, top=38)
    _style_ax(ax)
    fig.tight_layout()
    _save(fig, f"comparison_st_throughput_{theme.name}.svg")


def chart_multithread_scaling(py313: dict, py313t: dict, theme: Theme) -> None:
    colors = _colors(theme)
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 5), sharey=True)

    panels = [
        (ax1, py313, "Python 3.13 (GIL)"),
        (ax2, py313t, "Python 3.13t (no GIL)"),
    ]

    for ax, data, title in panels:
        th = data["threading"]
        thread_counts = sorted((int(k) for k in th), key=int)
        for lib in LIBS:
            values = [_millions(th[str(tc)].get(lib, 0)) for tc in thread_counts]
            if any(v > 0 for v in values):
                ax.plot(
                    thread_counts,
                    values,
                    marker="o",
                    markersize=4,
                    label=LABELS[lib],
                    color=colors[lib],
                    linewidth=2,
                )
        ax.set_xlabel("Threads")
        ax.set_title(title)
        ax.set_xscale("log", base=2)
        ax.set_xticks(thread_counts)
        ax.set_xticklabels([str(t) for t in thread_counts])
        _style_ax(ax)

    ax1.set_ylabel("Throughput (M ops/s)")
    handles, labels = ax1.get_legend_handles_labels()
    fig.legend(
        handles, labels, ncol=len(LIBS),
        loc="lower center", bbox_to_anchor=(0.5, -0.05), frameon=False,
    )
    fig.suptitle("Multi-Thread Scaling", y=1.02)
    fig.tight_layout()
    _save(fig, f"comparison_mt_scaling_{theme.name}.svg")


def chart_scaling_efficiency(py313: dict, py313t: dict, theme: Theme) -> None:
    colors = _colors(theme)
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
            ratios = [th[str(tc)].get(lib, 0) / baseline for tc in thread_counts]
            ax.plot(
                thread_counts,
                ratios,
                marker="o",
                markersize=5,
                label=LABELS[lib],
                color=colors[lib],
                linewidth=2,
            )
        ax.axhline(y=1.0, color=theme.text_dim, linestyle="--", alpha=0.4, linewidth=1)
        ax.set_xlabel("Threads")
        ax.set_title(title)
        ax.set_xscale("log", base=2)
        ax.set_xticks(thread_counts)
        ax.set_xticklabels([str(t) for t in thread_counts])
        _style_ax(ax)

    ax1.set_ylabel("Scaling Ratio (vs 1 thread)")
    handles, labels = ax1.get_legend_handles_labels()
    fig.legend(
        handles, labels, ncol=len(LIBS),
        loc="lower center", bbox_to_anchor=(0.5, -0.05), frameon=False,
    )
    fig.suptitle("Multi-Thread Scaling Efficiency", y=1.02)
    fig.tight_layout()
    _save(fig, f"comparison_scaling_ratio_{theme.name}.svg")


def chart_backends(py313: dict, theme: Theme) -> None:
    categories = ["Memory\n(in-process)", "Shared\n(mmap)", "Multi-process\n(8 workers)"]
    values = [
        _millions(py313["shared_throughput"]["memory"]["ops_per_sec"]),
        _millions(py313["shared_throughput"]["shared"]["ops_per_sec"]),
        _millions(py313["multiprocess"]["8"]["total_ops_per_sec"]),
    ]

    fig, ax = plt.subplots(figsize=(7, 5))
    bars = ax.bar(categories, values, color=theme.backend_colors)

    for bar, val in zip(bars, values, strict=True):
        label = f"{val:.1f}M" if val >= 1.0 else f"{val:.2f}M"
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            bar.get_height() * 0.75,
            label,
            ha="center",
            va="top",
            fontsize=13,
            color="white",
        )

    ax.set_ylabel("Throughput (M ops/s)")
    ax.set_title("warp_cache Backend Comparison  (Python 3.13)", pad=12)
    ax.set_ylim(bottom=0)
    _style_ax(ax)
    fig.tight_layout()
    _save(fig, f"comparison_backends_{theme.name}.svg")


def chart_async_throughput(py313: dict, py313t: dict, theme: Theme) -> None:
    """Bar chart comparing sync vs async throughput for warp_cache and moka_py."""
    colors = _colors(theme)
    async_libs = ["warp_cache", "moka_py"]
    async_labels = {"warp_cache": "warp_cache", "moka_py": "moka_py"}

    # Data: sync 256 and async 256 for each lib, on both 3.13 and 3.13t
    datasets = [
        ("3.13\nsync", py313, "sync_256"),
        ("3.13\nasync", py313, "256"),
        ("3.13t\nsync", py313t, "sync_256"),
        ("3.13t\nasync", py313t, "256"),
    ]

    fig, ax = plt.subplots(figsize=(8, 5))
    n_groups = len(datasets)
    bar_width = 0.3
    x_positions = range(n_groups)

    for i, lib in enumerate(async_libs):
        values = []
        for _, data, key in datasets:
            at = data.get("async_throughput", {})
            values.append(_millions(at.get(key, {}).get(lib, 0)))
        offsets = [x + i * bar_width for x in x_positions]
        ax.bar(offsets, values, bar_width, label=async_labels[lib], color=colors[lib])
        for x, v in zip(offsets, values, strict=True):
            if v > 0:
                ax.text(
                    x,
                    v + 0.3,
                    f"{v:.1f}M",
                    ha="center",
                    va="bottom",
                    fontsize=9,
                    color=colors[lib],
                )

    ax.set_xlabel("Python Version / Mode")
    ax.set_ylabel("Throughput (M ops/s)")
    ax.set_title("Sync vs Async Throughput  (cache size = 256)", pad=10)
    center = bar_width * (len(async_libs) - 1) / 2
    ax.set_xticks([x + center for x in x_positions])
    ax.set_xticklabels([label for label, _, _ in datasets])
    ax.legend(ncol=2, loc="upper center", bbox_to_anchor=(0.5, -0.15), frameon=False)
    ax.set_ylim(bottom=0)
    _style_ax(ax)
    fig.tight_layout()
    _save(fig, f"comparison_async_{theme.name}.svg")


def main() -> None:
    print("Loading benchmark data...")
    py312 = _load("bench_py3.12.json")
    py313 = _load("bench_py3.13.json")
    py313t = _load("bench_default.json")

    print(f"  py3.12:  {_py_label(py312)}")
    print(f"  py3.13:  {_py_label(py313)}")
    print(f"  py3.13t: {_py_label(py313t)}")

    for theme in (LIGHT, DARK):
        print(f"\nGenerating {theme.name} charts...")
        _apply_theme(theme)
        chart_single_thread_throughput(py312, py313, py313t, theme)
        chart_multithread_scaling(py313, py313t, theme)
        chart_scaling_efficiency(py313, py313t, theme)
        chart_backends(py313, theme)
        chart_async_throughput(py313, py313t, theme)

    print("\nDone!")


if __name__ == "__main__":
    main()
