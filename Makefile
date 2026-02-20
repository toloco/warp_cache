.PHONY: help fmt lint typecheck build build-debug test test-rust test-only bench bench-quick bench-all bench-report clean publish publish-test setup all

# Optional: specify Python version, e.g. make build PYTHON=3.14
PYTHON ?=
UV_PYTHON := $(if $(PYTHON),--python $(PYTHON),)
SUPPORTED_PYTHONS ?= 3.9 3.10 3.11 3.12 3.13 3.14

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"} /^[a-zA-Z_-]+:.*##/ {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

# ── Setup ────────────────────────────────────────────────────────────────────
setup: ## Create venv and install dev dependencies
	uv sync --dev $(UV_PYTHON)

# ── Format ───────────────────────────────────────────────────────────────────
fmt: ## Format Python (ruff) and Rust (cargo fmt)
	uv run $(UV_PYTHON) ruff format warp_cache/ tests/ benchmarks/
	uv run $(UV_PYTHON) ruff check --fix warp_cache/ tests/ benchmarks/
	cargo fmt

# ── Lint ─────────────────────────────────────────────────────────────────────
lint: ## Lint Python (ruff), type-check (ty), and Rust (clippy)
	uv run $(UV_PYTHON) ruff check warp_cache/ tests/ benchmarks/
	uv run $(UV_PYTHON) ty check
	cargo clippy -- -D warnings

typecheck: ## Type-check Python (ty)
	uv run $(UV_PYTHON) ty check

# ── Build ────────────────────────────────────────────────────────────────────
build: ## Build the Rust extension (release)
	uv run $(UV_PYTHON) maturin develop --release

build-debug: ## Build the Rust extension (debug, faster compile)
	uv run $(UV_PYTHON) maturin develop

# ── Test ─────────────────────────────────────────────────────────────────────
test-rust: ## Run Rust unit tests
	cargo test

test: build test-rust ## Build and run tests
	uv run $(UV_PYTHON) pytest tests/ -v


TEST_TARGETS := $(addprefix test-py-,$(SUPPORTED_PYTHONS))
test-matrix: $(TEST_TARGETS) ## Run tests for multiple Python versions (parallel with -j)

test-py-%:
	@echo "==> Testing with Python $*"
	@$(MAKE) test PYTHON=$*

test-only: ## Run tests without rebuilding
	uv run $(UV_PYTHON) pytest tests/ -v

# ── Benchmark ────────────────────────────────────────────────────────────────
bench: build ## Run benchmarks for current Python
	uv run $(UV_PYTHON) python benchmarks/_bench_runner.py --tag default

bench-quick: build ## Quick benchmarks (skip sustained/TTL)
	uv run $(UV_PYTHON) python benchmarks/_bench_runner.py --tag default --quick

bench-all: ## Run benchmarks across Python versions + generate report
	bash benchmarks/bench_all.sh

bench-report: ## Generate report from existing results
	uv run python benchmarks/_report_generator.py

# ── Publish ──────────────────────────────────────────────────────────────────
publish-test: ## Build and upload to TestPyPI
	uv run $(UV_PYTHON) maturin publish --repository testpypi

publish: ## Build and upload to PyPI
	uv run $(UV_PYTHON) maturin publish

# ── Clean ────────────────────────────────────────────────────────────────────
clean: ## Remove build artifacts
	cargo clean
	rm -rf target/ dist/ *.egg-info build/
	find . -type d -name __pycache__ -exec rm -rf {} + 2>/dev/null || true

# ── All ──────────────────────────────────────────────────────────────────────
all: fmt lint test ## Format, lint, and test
