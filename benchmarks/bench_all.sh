#!/usr/bin/env bash
#
# Run warp_cache benchmarks across multiple Python versions.
# Replaces the Jupyter notebook orchestration.
#
# Usage:
#   ./benchmarks/bench_all.sh                              # default versions
#   ./benchmarks/bench_all.sh --quick                      # skip sustained/TTL
#   ./benchmarks/bench_all.sh --versions "3.12 3.13 3.13t" # custom versions
#   ./benchmarks/bench_all.sh --report-only                # just regenerate report
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNNER="$SCRIPT_DIR/_bench_runner.py"
REPORT_GEN="$SCRIPT_DIR/_report_generator.py"

# Defaults
VERSIONS="3.12 3.13 3.13t"
QUICK=""
REPORT_ONLY=false
CLEANUP=true

# Bench dependency packages
BENCH_DEPS="cachetools cachebox moka-py zoocache"

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --versions "V1 V2 ..."   Python versions to test (default: "$VERSIONS")
  --quick                   Skip sustained & TTL benchmarks
  --report-only             Only regenerate report from existing JSON results
  --keep-venvs              Keep temporary venvs for debugging
  -h, --help                Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --versions) VERSIONS="$2"; shift 2 ;;
        --quick) QUICK="--quick"; shift ;;
        --report-only) REPORT_ONLY=true; shift ;;
        --keep-venvs) CLEANUP=false; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1"; usage; exit 1 ;;
    esac
done

if $REPORT_ONLY; then
    echo "==> Generating report from existing results..."
    tags=""
    for ver in $VERSIONS; do
        label="py${ver}"
        if [[ -n "$tags" ]]; then tags="$tags,$label"; else tags="$label"; fi
    done
    python "$REPORT_GEN" --tags "$tags"
    exit 0
fi

TMPDIR_BASE=$(mktemp -d "${TMPDIR:-/tmp}/warp_cache_bench_XXXXXX")
WHEELS_DIR="$TMPDIR_BASE/wheels"
mkdir -p "$WHEELS_DIR"

echo "Project root: $PROJECT_ROOT"
echo "Temp dir:     $TMPDIR_BASE"
echo "Versions:     $VERSIONS"
echo "Quick:        ${QUICK:-no}"
echo ""

completed_tags=""

for ver in $VERSIONS; do
    label="py${ver}"
    venv_dir="$TMPDIR_BASE/$label"

    echo ""
    echo "============================================================"
    echo "  $label (python=$ver)"
    echo "============================================================"

    # 1. Create venv
    echo ""
    echo "[1/5] Creating venv..."
    if ! uv venv --python "$ver" "$venv_dir" 2>&1; then
        echo "  SKIP $label: could not create venv (python $ver not available)"
        continue
    fi
    VENV_PYTHON="$venv_dir/bin/python"

    # 2. Install maturin
    echo ""
    echo "[2/5] Installing maturin..."
    uv pip install --python "$VENV_PYTHON" maturin

    # 3. Build wheel
    echo ""
    echo "[3/5] Building warp_cache wheel..."
    CARGO_BIN="$HOME/.cargo/bin"
    if ! PATH="$CARGO_BIN:$PATH" "$VENV_PYTHON" -m maturin build \
        --release -i "$VENV_PYTHON" -o "$WHEELS_DIR" \
        --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>&1; then
        echo "  SKIP $label: maturin build failed"
        continue
    fi

    # 4. Install wheel + bench deps
    echo ""
    echo "[4/5] Installing wheel + dependencies..."
    WHEEL=$(ls -t "$WHEELS_DIR"/warp_cache-*"$(echo "$ver" | tr -d '.')"-*.whl 2>/dev/null | head -1)
    if [[ -z "$WHEEL" ]]; then
        # Fallback: try any recent wheel
        WHEEL=$(ls -t "$WHEELS_DIR"/warp_cache-*.whl 2>/dev/null | head -1)
    fi
    if [[ -z "$WHEEL" ]]; then
        echo "  SKIP $label: no wheel found"
        continue
    fi
    uv pip install --python "$VENV_PYTHON" "$WHEEL" --force-reinstall
    # Install bench deps, ignoring failures for packages that may not support this Python
    for dep in $BENCH_DEPS; do
        uv pip install --python "$VENV_PYTHON" "$dep" 2>/dev/null || \
            echo "  Note: $dep not available for Python $ver"
    done

    # 5. Run benchmarks
    echo ""
    echo "[5/5] Running benchmarks..."
    "$VENV_PYTHON" "$RUNNER" --tag "$label" $QUICK

    if [[ -n "$completed_tags" ]]; then
        completed_tags="$completed_tags,$label"
    else
        completed_tags="$label"
    fi
done

# Generate report
if [[ -n "$completed_tags" ]]; then
    echo ""
    echo "============================================================"
    echo "  Generating report"
    echo "============================================================"
    python "$REPORT_GEN" --tags "$completed_tags"
fi

# Cleanup
if $CLEANUP; then
    rm -rf "$TMPDIR_BASE"
    echo ""
    echo "Cleaned up $TMPDIR_BASE"
else
    echo ""
    echo "Kept venvs at $TMPDIR_BASE"
fi

echo ""
echo "Completed: $completed_tags"
