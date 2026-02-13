# Development

Requires [uv](https://docs.astral.sh/uv/) and [Rust](https://rustup.rs/).

```bash
uv sync --dev                          # create venv + install dependencies
uv run maturin develop --release       # build the Rust extension
uv run pytest tests/ -v                # run tests
```

Or using Make:

```bash
make setup    # uv sync --dev
make test     # build + test
```
