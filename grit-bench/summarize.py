"""Summarizes hyperfine JSON results from bench.sh as a markdown table."""

import json
import math
import pathlib
import statistics
import sys

ORDER = [
    "clone", "fetch-noop", "fetch-catchup", "push-new-bookmark", "init-colocate",
    "git-import-noop", "git-export-noop", "status", "log", "log-all-descriptions",
    "diff-stat-100", "file-annotate", "snapshot-dirty", "new", "util-gc",
]


def fmt(seconds):
    return f"{seconds * 1000:.0f} ms" if seconds < 10 else f"{seconds:.1f} s"


def stats(results, key):
    values = [m[key]["value"] for r in results for m in r["measurements"]]
    return statistics.median(values), statistics.mean(values), statistics.pstdev(values), len(values)


def main(results_dir):
    rows = {}
    # Results of each benchmark, from every pass (`<repo>-<bench>[.<pass>].json`).
    by_bench = {}
    for path in sorted(pathlib.Path(results_dir).glob("*.json")):
        repo, bench = path.name.split(".")[0].split("-", 1)
        for r in json.loads(path.read_text())["results"]:
            bin_name = r["parameters"]["bin"]["value"]
            by_bench.setdefault((repo, bench), {}).setdefault(bin_name, []).append(r)
    for key, bins in by_bench.items():
        rows[key] = {
            bin_name: {
                "wall": stats(results, "time_wall_clock"),
                "cpu": stats(results, "time_cpu"),
                "rss": stats(results, "memory_peak_resident"),
            }
            for bin_name, results in bins.items()
        }

    print("| repo | operation | upstream wall | grit wall | wall ratio | CPU ratio | peak RSS up → grit | runs |")
    print("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    wall_ratios, cpu_ratios = [], []
    for repo in "SML":
        for bench in ORDER:
            if (repo, bench) not in rows:
                continue
            up, gr = rows[(repo, bench)]["upstream"], rows[(repo, bench)]["grit"]
            wall = gr["wall"][0] / up["wall"][0]
            cpu = gr["cpu"][0] / up["cpu"][0]
            wall_ratios.append(wall)
            cpu_ratios.append(cpu)
            print(f"| {repo} | {bench} | {fmt(up['wall'][0])} ± {fmt(up['wall'][2])} "
                  f"| {fmt(gr['wall'][0])} ± {fmt(gr['wall'][2])} | **{wall:.2f}×** | {cpu:.2f}× "
                  f"| {up['rss'][0] / 2**20:.0f} → {gr['rss'][0] / 2**20:.0f} MiB | {up['wall'][3]} |")
    for label, ratios in (("wall", wall_ratios), ("CPU", cpu_ratios)):
        geo = math.exp(sum(map(math.log, ratios)) / len(ratios))
        print(f"\nGeometric mean grit/upstream {label} time over {len(ratios)} benchmarks: {geo:.2f}×")


main(sys.argv[1] if len(sys.argv) > 1 else "results")
