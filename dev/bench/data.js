window.BENCHMARK_DATA = {
  "lastUpdate": 1781693736242,
  "repoUrl": "https://github.com/toloco/warp_cache",
  "entries": {
    "warp_cache benchmarks": [
      {
        "commit": {
          "author": {
            "email": "toloco@gmail.com",
            "name": "Tolo Palmer",
            "username": "toloco"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "7e903accf98986416caee5bdde759aefef81d601",
          "message": "fix: run CI benchmark trend job warp_cache-only (cachebox hangs on 3.10/3.11) (#72)",
          "timestamp": "2026-06-17T11:08:04+01:00",
          "tree_id": "db6e6255db0a037d918ddc1fa8f9c901fbc0235a",
          "url": "https://github.com/toloco/warp_cache/commit/7e903accf98986416caee5bdde759aefef81d601"
        },
        "date": 1781691852285,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "warp_cache single-thread throughput (size=1024) [macos-latest-py3.10]",
            "value": 15261642,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [macos-latest-py3.10]",
            "value": 11559246,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [macos-latest-py3.14]",
            "value": 12420496,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [macos-latest-py3.14]",
            "value": 10412689,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-24.04-arm-py3.10]",
            "value": 10076973,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-24.04-arm-py3.10]",
            "value": 7985950,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-24.04-arm-py3.14]",
            "value": 10476422,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-24.04-arm-py3.14]",
            "value": 8435195,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.10]",
            "value": 11030380,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.10]",
            "value": 8863210,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.11]",
            "value": 10594257,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.11]",
            "value": 8269236,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.12]",
            "value": 8229617,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.12]",
            "value": 6876019,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.13]",
            "value": 11079694,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.13]",
            "value": 8943326,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.14]",
            "value": 10692907,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.14]",
            "value": 8687069,
            "unit": "ops/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "toloco@gmail.com",
            "name": "Tolo Palmer",
            "username": "toloco"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "3c28952be7a0fea3066eb9b3518685f97d31be61",
          "message": "fix: fall back to pickle for non-UTF8 strings instead of raising (#34) (#68)\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-17T11:28:55+01:00",
          "tree_id": "dad10984a35cf3a311ef376a13cbc16bcfeffc28",
          "url": "https://github.com/toloco/warp_cache/commit/3c28952be7a0fea3066eb9b3518685f97d31be61"
        },
        "date": 1781692207823,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "warp_cache single-thread throughput (size=1024) [macos-latest-py3.10]",
            "value": 13719851,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [macos-latest-py3.10]",
            "value": 11420687,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [macos-latest-py3.14]",
            "value": 11593531,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [macos-latest-py3.14]",
            "value": 9921291,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-24.04-arm-py3.10]",
            "value": 10096149,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-24.04-arm-py3.10]",
            "value": 8012380,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-24.04-arm-py3.14]",
            "value": 10508072,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-24.04-arm-py3.14]",
            "value": 8411592,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.10]",
            "value": 9680882,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.10]",
            "value": 5798374,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.11]",
            "value": 10498003,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.11]",
            "value": 8492786,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.12]",
            "value": 9478598,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.12]",
            "value": 7402533,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.13]",
            "value": 11244387,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.13]",
            "value": 8476608,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.14]",
            "value": 10710485,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.14]",
            "value": 8773754,
            "unit": "ops/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "toloco@gmail.com",
            "name": "Tolo Palmer",
            "username": "toloco"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "3864af063a2fd4376aae845ddd965bf92dad4166",
          "message": "fix: serialize shared-cache creation to fix create/truncate race (#33) (#73)\n\nCo-authored-by: Claude Opus 4.8 (1M context) <noreply@anthropic.com>",
          "timestamp": "2026-06-17T11:53:57+01:00",
          "tree_id": "a4bb7d6b2aabc8cffe506fc38b24d73e536e76cf",
          "url": "https://github.com/toloco/warp_cache/commit/3864af063a2fd4376aae845ddd965bf92dad4166"
        },
        "date": 1781693735442,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "warp_cache single-thread throughput (size=1024) [macos-latest-py3.10]",
            "value": 15378306,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [macos-latest-py3.10]",
            "value": 12883555,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [macos-latest-py3.14]",
            "value": 8306654,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [macos-latest-py3.14]",
            "value": 8684765,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-24.04-arm-py3.10]",
            "value": 10095878,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-24.04-arm-py3.10]",
            "value": 8040167,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-24.04-arm-py3.14]",
            "value": 10518535,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-24.04-arm-py3.14]",
            "value": 8375135,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.10]",
            "value": 10285267,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.10]",
            "value": 8192081,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.11]",
            "value": 10468356,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.11]",
            "value": 8622455,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.12]",
            "value": 9513891,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.12]",
            "value": 7769918,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.13]",
            "value": 10994160,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.13]",
            "value": 9018722,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache single-thread throughput (size=1024) [ubuntu-latest-py3.14]",
            "value": 10813994,
            "unit": "ops/s"
          },
          {
            "name": "warp_cache throughput (8 threads) [ubuntu-latest-py3.14]",
            "value": 8854905,
            "unit": "ops/s"
          }
        ]
      }
    ]
  }
}