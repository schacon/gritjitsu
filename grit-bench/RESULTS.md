# Local benchmark results (pooled from two passes)

Produced by `run2.sh` and `summarize.py`. Medians; `±` is the standard deviation.

| repo | operation | upstream wall | grit wall | wall ratio | CPU ratio | peak RSS up → grit | runs |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| S | clone | 302 ms ± 34 ms | 205 ms ± 18 ms | **0.68×** | 0.38× | 23 → 27 MiB | 20 |
| S | fetch-noop | 45 ms ± 4 ms | 17 ms ± 1 ms | **0.37×** | 0.46× | 24 → 26 MiB | 30 |
| S | fetch-catchup | 122 ms ± 19 ms | 97 ms ± 10 ms | **0.79×** | 0.86× | 24 → 58 MiB | 30 |
| S | push-new-bookmark | 200 ms ± 22 ms | 78 ms ± 8 ms | **0.39×** | 0.30× | 24 → 26 MiB | 30 |
| S | init-colocate | 140 ms ± 9 ms | 144 ms ± 6 ms | **1.03×** | 1.09× | 28 → 30 MiB | 20 |
| S | git-import-noop | 12 ms ± 0 ms | 13 ms ± 0 ms | **1.06×** | 1.08× | 24 → 25 MiB | 30 |
| S | git-export-noop | 11 ms ± 0 ms | 12 ms ± 1 ms | **1.08×** | 1.09× | 24 → 25 MiB | 30 |
| S | status | 11 ms ± 0 ms | 12 ms ± 1 ms | **1.13×** | 1.10× | 24 → 26 MiB | 30 |
| S | log | 14 ms ± 1 ms | 15 ms ± 1 ms | **1.10×** | 1.12× | 24 → 26 MiB | 30 |
| S | log-all-descriptions | 17 ms ± 1 ms | 18 ms ± 1 ms | **1.08×** | 1.08× | 24 → 26 MiB | 30 |
| S | diff-stat-100 | 86 ms ± 2 ms | 21 ms ± 1 ms | **0.25×** | 0.38× | 29 → 35 MiB | 30 |
| S | file-annotate | 17 ms ± 2 ms | 19 ms ± 1 ms | **1.10×** | 1.04× | 26 → 28 MiB | 20 |
| S | snapshot-dirty | 45 ms ± 1 ms | 47 ms ± 1 ms | **1.03×** | 1.10× | 25 → 26 MiB | 30 |
| S | new | 40 ms ± 1 ms | 42 ms ± 2 ms | **1.05×** | 1.10× | 24 → 26 MiB | 30 |
| S | util-gc | 101 ms ± 4 ms | 56 ms ± 4 ms | **0.55×** | 0.54× | 24 → 36 MiB | 20 |
| M | clone | 1783 ms ± 26 ms | 1023 ms ± 51 ms | **0.57×** | 0.18× | 57 → 85 MiB | 10 |
| M | fetch-noop | 51 ms ± 3 ms | 20 ms ± 1 ms | **0.39×** | 0.57× | 29 → 31 MiB | 20 |
| M | fetch-catchup | 527 ms ± 10 ms | 421 ms ± 15 ms | **0.80×** | 1.45× | 34 → 238 MiB | 20 |
| M | push-new-bookmark | 251 ms ± 15 ms | 106 ms ± 4 ms | **0.42×** | 0.36× | 32 → 34 MiB | 20 |
| M | init-colocate | 308 ms ± 20 ms | 310 ms ± 15 ms | **1.01×** | 1.02× | 58 → 61 MiB | 10 |
| M | git-import-noop | 14 ms ± 3 ms | 15 ms ± 3 ms | **1.09×** | 1.02× | 29 → 30 MiB | 20 |
| M | git-export-noop | 14 ms ± 1 ms | 14 ms ± 2 ms | **1.02×** | 1.00× | 29 → 30 MiB | 20 |
| M | status | 15 ms ± 2 ms | 16 ms ± 1 ms | **1.05×** | 1.02× | 29 → 30 MiB | 20 |
| M | log | 17 ms ± 3 ms | 18 ms ± 1 ms | **1.05×** | 1.03× | 29 → 30 MiB | 20 |
| M | log-all-descriptions | 94 ms ± 4 ms | 91 ms ± 7 ms | **0.97×** | 0.95× | 34 → 36 MiB | 20 |
| M | diff-stat-100 | 105 ms ± 6 ms | 72 ms ± 5 ms | **0.69×** | 0.86× | 56 → 81 MiB | 20 |
| M | file-annotate | 953 ms ± 53 ms | 769 ms ± 23 ms | **0.81×** | 0.81× | 64 → 174 MiB | 10 |
| M | snapshot-dirty | 50 ms ± 2 ms | 50 ms ± 3 ms | **1.00×** | 1.02× | 31 → 32 MiB | 20 |
| M | new | 44 ms ± 2 ms | 46 ms ± 5 ms | **1.04×** | 1.07× | 32 → 33 MiB | 20 |
| M | util-gc | 637 ms ± 12 ms | 398 ms ± 12 ms | **0.62×** | 0.85× | 101 → 152 MiB | 10 |
| L | clone | 10.7 s ± 512 ms | 12.4 s ± 398 ms | **1.15×** | 1.04× | 581 → 1970 MiB | 6 |
| L | fetch-noop | 274 ms ± 6 ms | 113 ms ± 2 ms | **0.41×** | 0.55× | 59 → 72 MiB | 10 |
| L | fetch-catchup | 4558 ms ± 144 ms | 6244 ms ± 233 ms | **1.37×** | 2.11× | 217 → 1039 MiB | 10 |
| L | push-new-bookmark | 656 ms ± 12 ms | 267 ms ± 24 ms | **0.41×** | 0.42× | 65 → 71 MiB | 10 |
| L | init-colocate | 1584 ms ± 50 ms | 1781 ms ± 127 ms | **1.12×** | 1.15× | 222 → 241 MiB | 6 |
| L | git-import-noop | 47 ms ± 4 ms | 49 ms ± 3 ms | **1.04×** | 1.02× | 59 → 60 MiB | 10 |
| L | git-export-noop | 48 ms ± 3 ms | 50 ms ± 1 ms | **1.03×** | 1.01× | 59 → 60 MiB | 10 |
| L | status | 48 ms ± 2 ms | 51 ms ± 0 ms | **1.05×** | 1.01× | 59 → 61 MiB | 10 |
| L | log | 51 ms ± 3 ms | 52 ms ± 2 ms | **1.02×** | 1.02× | 60 → 62 MiB | 10 |
| L | log-all-descriptions | 597 ms ± 47 ms | 597 ms ± 9 ms | **1.00×** | 1.00× | 92 → 95 MiB | 10 |
| L | diff-stat-100 | 1256 ms ± 11 ms | 208 ms ± 5 ms | **0.17×** | 0.24× | 142 → 164 MiB | 10 |
| L | file-annotate | 5460 ms ± 175 ms | 4751 ms ± 52 ms | **0.87×** | 0.87× | 241 → 384 MiB | 6 |
| L | snapshot-dirty | 88 ms ± 1 ms | 88 ms ± 1 ms | **1.00×** | 1.03× | 63 → 65 MiB | 10 |
| L | new | 81 ms ± 3 ms | 87 ms ± 2 ms | **1.08×** | 1.05× | 64 → 70 MiB | 10 |
| L | util-gc | 5380 ms ± 190 ms | 5539 ms ± 148 ms | **1.03×** | 1.14× | 809 → 788 MiB | 6 |

Geometric mean grit/upstream wall time over 45 benchmarks: 0.80×

Geometric mean grit/upstream CPU time over 45 benchmarks: 0.82×
