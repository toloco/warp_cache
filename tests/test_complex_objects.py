"""Tests for caching large and complex Python objects — both as return values and as keys."""

import contextlib
import glob
import os
import sys
import tempfile
from dataclasses import dataclass

import pytest

from warp_cache import cache

_skip_on_windows = pytest.mark.skipif(sys.platform == "win32", reason="shared memory is Unix-only")

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Point:
    x: float
    y: float
    label: str


@dataclass(frozen=True)
class User:
    id: int
    name: str
    tags: tuple


def _cleanup_shm():
    tmpdir = tempfile.gettempdir()
    shm_dir = os.path.join(tmpdir, "warp_cache")
    if os.path.isdir(shm_dir):
        for f in glob.glob(os.path.join(shm_dir, "*")):
            with contextlib.suppress(OSError):
                os.unlink(f)


# ===========================================================================
# Memory backend — complex return values
# ===========================================================================


class TestMemoryComplexValues:
    def test_nested_dict(self):
        call_count = 0

        @cache(max_size=64)
        def build_config(name):
            nonlocal call_count
            call_count += 1
            return {
                "name": name,
                "nested": {"a": [1, 2, 3], "b": {"c": True, "d": None}},
                "tags": ["alpha", "beta"],
                "count": 42,
            }

        result = build_config("test")
        assert result["nested"]["b"]["c"] is True
        assert build_config("test") == result
        assert call_count == 1

    def test_list_of_dataclasses(self):
        call_count = 0

        @cache(max_size=64)
        def get_points(n):
            nonlocal call_count
            call_count += 1
            return [Point(x=float(i), y=float(i * 2), label=f"p{i}") for i in range(n)]

        points = get_points(100)
        assert len(points) == 100
        assert points[50] == Point(x=50.0, y=100.0, label="p50")
        assert get_points(100) is points  # same object from cache
        assert call_count == 1

    def test_large_dict(self):
        @cache(max_size=16)
        def build_large(n):
            return {f"key_{i}": list(range(i)) for i in range(n)}

        result = build_large(500)
        assert len(result) == 500
        assert result["key_499"] == list(range(499))
        assert build_large(500) is result

    def test_deeply_nested(self):
        @cache(max_size=16)
        def build_deep(depth):
            obj = {"value": 0}
            for i in range(1, depth + 1):
                obj = {"value": i, "child": obj, "items": list(range(i))}
            return obj

        result = build_deep(50)
        # Walk down to verify
        node = result
        for i in range(50, 0, -1):
            assert node["value"] == i
            node = node["child"]
        assert node["value"] == 0

        assert build_deep(50) is result

    def test_mixed_type_collection(self):
        @cache(max_size=16)
        def mixed():
            return {
                "int": 42,
                "float": 3.14,
                "str": "hello" * 1000,
                "bytes": b"\x00\xff" * 500,
                "none": None,
                "bool": True,
                "tuple": (1, (2, (3, (4,)))),
                "set": {1, 2, 3, 4, 5},
                "frozenset": frozenset(range(100)),
                "list_of_lists": [[i] * 10 for i in range(100)],
            }

        result = mixed()
        assert result["str"] == "hello" * 1000
        assert result["bytes"] == b"\x00\xff" * 500
        assert len(result["frozenset"]) == 100
        assert mixed() is result

    def test_large_bytes(self):
        @cache(max_size=16)
        def generate(n):
            return bytes(range(256)) * n

        blob = generate(1000)
        assert len(blob) == 256_000
        assert generate(1000) is blob

    def test_large_string(self):
        @cache(max_size=16)
        def build_text(n):
            return "abcdefghij" * n

        text = build_text(100_000)
        assert len(text) == 1_000_000
        assert build_text(100_000) is text


# ===========================================================================
# Memory backend — complex keys (arguments)
# ===========================================================================


class TestMemoryComplexKeys:
    def test_tuple_of_tuples(self):
        call_count = 0

        @cache(max_size=64)
        def process(data):
            nonlocal call_count
            call_count += 1
            return sum(sum(row) for row in data)

        matrix = tuple(tuple(range(i, i + 10)) for i in range(100))
        assert process(matrix) == process(matrix)
        assert call_count == 1

    def test_frozenset_key(self):
        call_count = 0

        @cache(max_size=64)
        def lookup(items):
            nonlocal call_count
            call_count += 1
            return len(items)

        fs = frozenset(range(1000))
        assert lookup(fs) == 1000
        assert lookup(fs) == 1000
        assert call_count == 1

    def test_dataclass_key(self):
        call_count = 0

        @cache(max_size=64)
        def describe(point):
            nonlocal call_count
            call_count += 1
            return f"{point.label}: ({point.x}, {point.y})"

        p = Point(x=1.5, y=2.5, label="origin")
        assert describe(p) == "origin: (1.5, 2.5)"
        assert describe(p) == "origin: (1.5, 2.5)"
        assert call_count == 1

        # Different dataclass instance with same values should also hit
        p2 = Point(x=1.5, y=2.5, label="origin")
        assert describe(p2) == "origin: (1.5, 2.5)"
        assert call_count == 1

    def test_many_kwargs(self):
        call_count = 0

        @cache(max_size=64)
        def f(**kwargs):
            nonlocal call_count
            call_count += 1
            return kwargs

        kw = {f"key_{i}": i for i in range(50)}
        f(**kw)
        f(**kw)
        assert call_count == 1

    def test_large_string_key(self):
        call_count = 0

        @cache(max_size=64)
        def echo(s):
            nonlocal call_count
            call_count += 1
            return s

        big = "x" * 100_000
        assert echo(big) == big
        assert echo(big) == big
        assert call_count == 1

    def test_mixed_arg_types(self):
        call_count = 0

        @cache(max_size=64)
        def f(a, b, c, d, e):
            nonlocal call_count
            call_count += 1
            return (a, b, c, d, e)

        result = f(42, "hello", (1, 2, 3), frozenset([4, 5]), True)
        assert f(42, "hello", (1, 2, 3), frozenset([4, 5]), True) == result
        assert call_count == 1


# ===========================================================================
# Shared backend — complex values and keys (pickle round-trip)
# ===========================================================================


@_skip_on_windows
class TestSharedComplexValues:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_nested_dict(self):
        call_count = 0

        @cache(max_size=64, backend="shared", max_value_size=8192)
        def build(name):
            nonlocal call_count
            call_count += 1
            return {
                "name": name,
                "nested": {"a": [1, 2, 3], "b": {"c": True}},
                "tags": ["alpha", "beta"],
            }

        result = build("test")
        assert result["nested"]["b"]["c"] is True
        cached = build("test")
        assert cached == result
        assert call_count == 1

    def test_list_of_dataclasses(self):
        call_count = 0

        @cache(max_size=64, backend="shared", max_value_size=65536)
        def get_points(n):
            nonlocal call_count
            call_count += 1
            return [Point(x=float(i), y=float(i * 2), label=f"p{i}") for i in range(n)]

        points = get_points(50)
        assert len(points) == 50
        assert points[25] == Point(x=25.0, y=50.0, label="p25")
        cached = get_points(50)
        assert cached == points
        assert call_count == 1

    def test_large_bytes(self):
        @cache(max_size=16, backend="shared", max_value_size=65536)
        def generate(n):
            return bytes(range(256)) * n

        blob = generate(200)
        assert len(blob) == 51_200
        assert generate(200) == blob

    def test_mixed_types(self):
        @cache(max_size=16, backend="shared", max_value_size=16384)
        def mixed():
            return {
                "int": 42,
                "float": 3.14,
                "str": "hello" * 200,
                "bytes": b"\xab" * 200,
                "none": None,
                "bool": False,
                "tuple": (1, 2, (3, 4)),
                "list": list(range(100)),
            }

        result = mixed()
        assert result["str"] == "hello" * 200
        assert result["list"] == list(range(100))
        assert mixed() == result


@_skip_on_windows
class TestSharedComplexKeys:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_tuple_key(self):
        call_count = 0

        @cache(max_size=64, backend="shared", max_key_size=8192)
        def process(data):
            nonlocal call_count
            call_count += 1
            return sum(data)

        key = tuple(range(200))
        assert process(key) == sum(range(200))
        assert process(key) == sum(range(200))
        assert call_count == 1

    def test_dataclass_key(self):
        call_count = 0

        @cache(max_size=64, backend="shared", max_key_size=4096)
        def describe(user):
            nonlocal call_count
            call_count += 1
            return f"{user.name} ({user.id})"

        u = User(id=1, name="alice", tags=("admin", "staff"))
        assert describe(u) == "alice (1)"
        assert describe(u) == "alice (1)"
        assert call_count == 1

    def test_many_kwargs(self):
        call_count = 0

        @cache(max_size=64, backend="shared", max_key_size=16384)
        def f(**kwargs):
            nonlocal call_count
            call_count += 1
            return len(kwargs)

        kw = {f"k{i}": i for i in range(30)}
        assert f(**kw) == 30
        assert f(**kw) == 30
        assert call_count == 1

    def test_frozenset_key(self):
        call_count = 0

        @cache(max_size=64, backend="shared", max_key_size=8192)
        def count(items):
            nonlocal call_count
            call_count += 1
            return len(items)

        fs = frozenset(range(200))
        assert count(fs) == 200
        assert count(fs) == 200
        assert call_count == 1
