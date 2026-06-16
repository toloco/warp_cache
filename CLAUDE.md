# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

**warp_cache** is a thread-safe Python caching decorator backed by a Rust extension
(PyO3 + maturin): SIEVE eviction, TTL support, async awareness, and a cross-process
shared-memory backend. For internals see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md);
for contributor process see [`CONTRIBUTING.md`](CONTRIBUTING.md).

---

## Golden rules (non-negotiable)

Every change flows through the same pipeline:

> **issue → branch → implement + tests → full check gate → PR → human review**

- **Never start coding without an approved GitHub issue** (see Step 0).
- **Never commit or push to `master`.** Work on a branch, always.
- **Never merge.** Open the PR and hand it to a human. You stop at "review requested".
- **Never claim a gate passed without running it** and showing the output.

The default branch is **`master`**. `gh` CLI is available and authenticated.

---

## The workflow

### 0 · Start from an issue

Work begins from a GitHub issue. If the user references one, read it
(`gh issue view <N>`). If none exists, **do not start coding** — instead either:

- **Draft one** with `gh issue create`, then show it to the user and get explicit
  approval before writing any code, **or**
- **Ask** the user to point you to / create the issue.

The issue defines scope. If the work drifts from it, stop and re-confirm.

### 1 · Branch from `master`

```bash
git switch master && git pull
git switch -c <type>/<issue#>-<slug>   # e.g. fix/123-async-none-caching
```

Branch prefix matches the change type: `feat/`, `fix/`, `perf/`, `docs/`, `refactor/`.

### 2 · Implement

- **Tests are required** for any new behavior or bug fix (writing them first is
  encouraged but not mandatory). A bug fix should include a test that fails before the
  fix and passes after.
- Follow existing patterns. Keep the change focused on the issue's scope.
- If the **public API** changes, update `README.md`, `docs/`, `llms.txt`, and
  `llms-full.txt` to match.
- If **architecture/invariants** change, update [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

### 3 · Quality gate

Run the gate (next section) and **block the PR on any failure**. Report the actual
command output — don't assert success without evidence.

### 4 · Commit

Use **Conventional Commits** and end every commit message with the trailer:

```
<type>: <imperative summary>      # feat / fix / perf / docs / refactor / test / chore

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
```

### 5 · Open the PR and request review

```bash
git push -u origin HEAD
gh pr create --base master --fill   # then edit the body
```

The PR body must:
- Link the issue with **`Closes #<N>`**.
- Summarize **what** changed and **why** (and call out any perf/behavioral impact).
- Note which gates were run (and matrix/bench results for risky changes).

End the PR body with:

```
🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

Then **stop** — the human reviews and merges.

---

## Quality gate reference

Run in order before opening a PR. Any failure blocks the PR.

| Step | Command | Checks |
|------|---------|--------|
| Format | `make fmt` | ruff format + `--fix`, `cargo fmt` |
| Lint | `make lint` | ruff check, `ty` type-check, `cargo clippy -- -D warnings` |
| Test | `make test` | builds the extension, runs `cargo test` + full pytest suite |

**`make all`** runs fmt + lint + test in one shot.

### Risky changes — also run matrix + benchmarks

A change is **risky** if it touches any of:

- Rust core: `src/store.rs`, `src/shared_store.rs`, `src/key.rs`, `src/serde.rs`, `src/shm/`
- Hashing, key equality, or the passthrough hasher
- Anything `unsafe`, FFI / PyO3 boundary, or locking
- `#[repr(C)]` layout / `size_of` assertions in `layout.rs`

For risky changes, additionally run and report:

```bash
make test-matrix -j     # Python 3.9–3.14
make bench              # or: make bench-sieve  for eviction-quality changes
```

CI also runs lint, clippy (incl. Windows), and the full OS/Python test matrix on every
PR — but the gate above must pass locally first.

---

## Build & test commands

```bash
make setup            # venv + dev deps (uv sync --dev)
make build            # build Rust extension (release)
make build-debug      # build Rust extension (debug, faster compile)
make test             # build + cargo test + pytest
make test-only        # pytest without rebuilding
make test PYTHON=3.13 # specific Python version
make fmt / make lint  # format / lint (Python + Rust)
make bench            # benchmarks for current Python

# Run a single test:
uv run pytest tests/test_basic.py::test_cache_hit -v
```

---

## Code map

Where things live (full detail in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)):

| Path | Responsibility |
|------|----------------|
| `src/lib.rs` | PyO3 module entry; exports `CachedFunction`, `SharedCachedFunction` |
| `src/store.rs` | In-process backend (sharded hashbrown, GIL-conditional locking, hot `__call__`) |
| `src/shared_store.rs` | Cross-process backend over mmap'd shared memory |
| `src/serde.rs` | Fast-path binary serialization (pickle fallback) |
| `src/key.rs` | `CacheKey` + zero-alloc `BorrowedArgs` hit-path lookup |
| `src/entry.rs` | `SieveEntry` |
| `src/shm/` | Shared-memory infra: layout, mmap region, seqlock, hashtable, SIEVE ordering |
| `warp_cache/_decorator.py` | `cache()` factory; async detection + `AsyncCachedFunction` wrapper |
| `warp_cache/_strategies.py` | `Backend(IntEnum)`: MEMORY=0, SHARED=1 |
| `tests/` | pytest suite |
| `benchmarks/` | performance + SIEVE-quality benchmarks |

## Critical invariants (full list in ARCHITECTURE.md)

These two are cheap to violate and easy to miss in review:

- **Hash table capacity must be power-of-2** — probing uses `hash & (capacity - 1)`.
  Always `.next_power_of_two()`.
- **`#[repr(C)]` field ordering** — u64 fields before u32 to avoid alignment padding;
  affects `size_of` assertions in `layout.rs`.

## Linting

- **Python**: ruff (rules `E, F, W, I, UP, B, SIM`; line-length 100; target py39) + `ty`.
- **Rust**: `cargo fmt`, `cargo clippy -- -D warnings`.

`make fmt` and `make lint` handle both languages.
