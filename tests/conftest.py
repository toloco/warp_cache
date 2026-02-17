import sys

collect_ignore_glob = []
if sys.platform == "win32":
    collect_ignore_glob = ["test_shared_*.py"]
