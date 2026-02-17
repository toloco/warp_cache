# Contributing to warp_cache

Thank you for considering contributing to warp_cache! This guide will help you get
started.

## Getting Started

1. Fork the repository and clone your fork.
2. Set up the development environment:

```bash
make setup        # Create venv + install dev dependencies
make build-debug  # Build the Rust extension (debug mode)
make test-only    # Run the test suite
```

## Development Workflow

### Making Changes

1. Create a branch from `main` for your work.
2. Make your changes, keeping commits focused and well-described.
3. Add or update tests for any new or changed functionality.
4. Run the full check suite before submitting:

```bash
make fmt          # Format Python (ruff) + Rust (cargo fmt)
make lint         # Lint Python (ruff) + Rust (clippy)
make test         # Build + run all tests
```

### Project Structure

- **`src/`** — Rust core (PyO3 extension module)
- **`warp_cache/`** — Python package (decorator, strategies)
- **`tests/`** — pytest test suite
- **`benchmarks/`** — Performance benchmarks

### Code Style

- **Python**: Formatted and linted with [ruff](https://docs.astral.sh/ruff/) (line length 100)
- **Rust**: Formatted with `cargo fmt`, linted with `cargo clippy -- -D warnings`

Running `make fmt` handles both.

## Submitting Changes

### Pull Requests

1. Keep PRs focused on a single change.
2. Include a clear description of what the PR does and why.
3. Ensure all CI checks pass (formatting, linting, tests).
4. Link any related issues.

### Issues

- **Bug reports**: Include Python version, OS, warp_cache version, and a minimal
reproduction.
- **Feature requests**: Describe the use case and proposed behavior.

## Running Tests

```bash
make test              # Build + run all tests
make test-only         # Run tests without rebuilding
make test-matrix -j    # Test across Python 3.10-3.14
uv run pytest tests/test_basic.py::test_cache_hit -v  # Run a single test
```

## License

By contributing, you agree that your contributions will be licensed under the same
[MIT License](LICENSE) that covers this project.
