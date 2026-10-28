# gritjitsu

**gritjitsu** is [Jujutsu (jj)](https://github.com/jj-vcs/jj) with every Git
operation going through [grit-lib](https://github.com/gitbutlerapp/grit), a
pure-Rust Git implementation. It doesn't use
[gitoxide](https://github.com/GitoxideLabs/gitoxide) and never runs the `git`
executable, and jj's full test suite passes.

This is an experiment, not a supported build of jj. For jj itself, see
[jj-vcs/jj](https://github.com/jj-vcs/jj).

## Why

Upstream jj uses gitoxide for objects, refs, the index and config. It runs
the `git` CLI for fetch, push, worktrees and `gc`, which is why users need
Git 2.42 or newer installed. The question here is whether one library can do
all of it, how fast it is, and what grit-lib is missing. Every gap found went
back to grit as a fix.

## Status

- All 3480 jj tests pass. Upstream has 3491; the difference is unit tests
  for the `git` output parser, which is gone.
- There are no `gix*` crates in `Cargo.lock`, and `jj` works with no `git`
  installed, including clones and fetches from GitHub.
- Commits are written byte-for-byte as before, so commit ids don't change and
  existing repos keep working.
- Colocated repos with **reftable** refs work, which upstream jj doesn't
  support yet. SHA-256 repos work in both.
- Based on upstream jj `5f0451bdd` (0.46.0 development).

[`GRIT_PORT.md`](GRIT_PORT.md) has the step-by-step port plan and log.

## Benchmarks

Release builds of upstream jj (gitoxide + Git 2.50.1) and gritjitsu on an
Apple M5 Max, timed with [hyperfine](https://github.com/sharkdp/hyperfine)
against local bare repos. A ratio below 1 means gritjitsu is faster.

**git.git** (82k commits, 1,000 refs):

| operation | upstream | gritjitsu | ratio |
| --- | ---: | ---: | ---: |
| `git clone` | 9.76 s | 3.09 s | **0.32×** |
| `git fetch`, 2000 commits behind | 4.31 s | 3.28 s | **0.76×** |
| `git fetch`, nothing new | 274 ms | 113 ms | **0.41×** |
| `git push`, 20 new commits | 656 ms | 267 ms | **0.41×** |
| `diff --stat` across 100 commits | 1256 ms | 208 ms | **0.17×** |
| `file annotate` | 5.5 s | 4.8 s | **0.87×** |
| `status` | 48 ms | 51 ms | 1.05× |
| `log` | 51 ms | 52 ms | 1.02× |
| `util gc` | 5.4 s | 5.5 s | 1.03× |
| `git init --colocate` | 1.58 s | 1.78 s | 1.12× |

Peak memory for the clone is 259 MiB, against 580 MiB for upstream. Catching
up 2000 commits uses more: 645 MiB against 216 MiB (upstream's figure is its
largest single process, and `git` splits the work across several).

- **Faster:** clone, fetch and push skip starting `git` processes and
  parsing their output. A local clone hard-links the source pack, as
  `git clone --local` does. `diff --stat` uses grit's copy detection
  instead of gitoxide's rename tracking.
- **About the same:** everyday commands like `status`, `log` and `new`.
- **Still to do:** a catch-up fetch computes new deltas where Git reuses
  stored ones, so it uses about 1.7× the CPU. Over the network to GitHub,
  clone and fetch are roughly even.

The clone and catch-up rows were measured on Oct 28, after the grit-lib fixes
in [#2217](https://github.com/gitbutlerapp/grit/pull/2217) and
[#2218](https://github.com/gitbutlerapp/grit/pull/2218). The other rows are
from the full Oct 27 run, which also covers smaller repos and the network.
Its geometric mean over 45 benchmarks was 0.80×. See
[`grit-bench/RESULTS.md`](grit-bench/RESULTS.md).

## Changes to grit-lib

The port needed these fixes and additions, developed on the
[schacon/grit](https://github.com/schacon/grit) fork:

| Change | Status |
| --- | --- |
| `IgnorePatternList` for matching single paths against an exclude file | merged ([#2137](https://github.com/gitbutlerapp/grit/pull/2137)) |
| `detect_copies` reports source modes without source tree entries | merged ([#2146](https://github.com/gitbutlerapp/grit/pull/2146)) |
| `git worktree add --orphan`, and worktrees for bare repos | merged ([#2149](https://github.com/gitbutlerapp/grit/pull/2149)) |
| `worktree prune` checks the recorded `.git` file | merged ([#2150](https://github.com/gitbutlerapp/grit/pull/2150)) |
| Read loose ref files without an extra stat and readlink | merged ([#2194](https://github.com/gitbutlerapp/grit/pull/2194)) |
| `detect_copies` only treats newly created files as copies | open ([#2195](https://github.com/gitbutlerapp/grit/pull/2195)) |
| Tell the pusher when the `update` hook declines a ref | open ([#2148](https://github.com/gitbutlerapp/grit/pull/2148)) |
| Fix "pack not found" after a repack in another repository handle | open ([#2217](https://github.com/gitbutlerapp/grit/pull/2217)) |
| Faster local clone and fetch | open ([#2218](https://github.com/gitbutlerapp/grit/pull/2218)) |

## Building

`Cargo.toml` uses grit-lib from a local checkout at `../grit-jj/grit-lib`
with the open changes above applied, so this repo can't be built from a
fresh clone until they're merged.

```sh
cargo build --release --bin jj
cargo nextest run --workspace
```

To rerun the benchmarks, see [`grit-bench/`](grit-bench) (`setup.sh`,
`run2.sh`, `summarize.py`, `net.sh`, and `formats.sh` for the reftable and
SHA-256 checks).

## License

Apache 2.0, like upstream jj. See [LICENSE](LICENSE).
