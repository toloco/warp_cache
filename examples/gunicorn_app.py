# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache", "gunicorn"]
# ///
"""Shared cache with Gunicorn — cache persists across worker processes.

Each Gunicorn worker is a separate process. The shared memory backend
lets all workers read/write the same cache via mmap, avoiding redundant
computation and external cache services like Redis.

Run with:
    uv run examples/gunicorn_app.py

Then test:
    curl http://127.0.0.1:8000/compute/42
    curl http://127.0.0.1:8000/stats
"""

import hashlib
import json
import os

from warp_cache import Strategy, cache


@cache(strategy=Strategy.LRU, max_size=1024, ttl=60.0, backend="shared")
def expensive_compute(n):
    """Simulate a CPU-heavy computation shared across workers."""
    data = str(n).encode()
    for _ in range(500):
        data = hashlib.sha256(data).digest()
    return data.hex()


def app(environ, start_response):
    """Minimal WSGI application."""
    path = environ.get("PATH_INFO", "/")

    if path.startswith("/compute/"):
        key = path.split("/compute/", 1)[1]
        try:
            n = int(key)
        except ValueError:
            start_response("400 Bad Request", [("Content-Type", "text/plain")])
            return [b"key must be an integer"]

        result = expensive_compute(n)
        body = json.dumps({"key": n, "result": result, "pid": os.getpid()})
        start_response("200 OK", [("Content-Type", "application/json")])
        return [body.encode()]

    if path == "/stats":
        info = expensive_compute.cache_info()
        body = json.dumps({
            "pid": os.getpid(),
            "hits": info.hits,
            "misses": info.misses,
            "max_size": info.max_size,
            "current_size": info.current_size,
        })
        start_response("200 OK", [("Content-Type", "application/json")])
        return [body.encode()]

    start_response("404 Not Found", [("Content-Type", "text/plain")])
    return [b"Not found. Try /compute/<int> or /stats"]


if __name__ == "__main__":
    import sys

    from gunicorn.app.base import BaseApplication

    class StandaloneApp(BaseApplication):
        def __init__(self, wsgi_app, options=None):
            self.wsgi_app = wsgi_app
            self.options = options or {}
            super().__init__()

        def load_config(self):
            for key, value in self.options.items():
                self.cfg.set(key, value)

        def load(self):
            return self.wsgi_app

    workers = int(sys.argv[1]) if len(sys.argv) > 1 else 4
    print(f"Starting gunicorn with {workers} workers on http://127.0.0.1:8000")
    print("Try: curl http://127.0.0.1:8000/compute/42")
    print("     curl http://127.0.0.1:8000/stats")
    StandaloneApp(app, {"bind": "127.0.0.1:8000", "workers": workers}).run()
