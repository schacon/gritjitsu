# gritjitsu

**gritjitsu** is a fork of [Jujutsu (jj)](https://github.com/jj-vcs/jj) where every
Git operation goes through [grit-lib](https://github.com/gitbutlerapp/grit), a
pure-Rust Git implementation. It uses no [gitoxide](https://github.com/GitoxideLabs/gitoxide)
crates and never runs the `git` executable. jj's full test suite passes.

It is an experiment, not a supported distribution of jj. For jj itself, see
[jj-vcs/jj](https://github.com/jj-vcs/jj).

## Why

Upstream jj talks to Git in two ways:

- **gitoxide** (`gix`) for objects, refs, the index, config, `.gitignore`
  matching and URL parsing.
- **The `git` CLI** as a subprocess for fetch, push, finding a remote's default
  branch, worktree add/repair/prune and `gc`. jj parses the porcelain output,
  and users need Git 2.42 or newer on their `PATH`.

This project asks whether one library can do all of it:

- Can grit-lib replace both, with jj's test suite still passing unchanged
  except where tests checked subprocess details?
- What does it cost in speed and memory?
- Where does grit-lib's API fall short? Porting a real, heavily tested Git
  client is a good stress test, and every gap found went back to grit as a fix.

## Status

- Based on upstream jj `5f0451bdd` (0.46.0 development).
- `cargo nextest run --workspace`: **3480/3480 pass**. Upstream has 3491. Ten
  unit tests that only parsed `git` output were deleted with the parser, and
  two "git executable missing" clone tests became one test that clones with an
  empty `PATH`.
- `Cargo.lock` has no `gix*` crates.
- The `jj` binary works without `git` installed. Cloning and fetching from
  GitHub over HTTPS with an empty `PATH` works.
- Commit ids are unchanged. Commits are serialized byte-for-byte as gitoxide
  wrote them, and existing repos keep working.

## How it works

The port happened in two phases: first gitoxide was removed, then the `git`
subprocesses. Each step kept the suite green. [`GRIT_PORT.md`](GRIT_PORT.md)
has the step-by-step plan and progress log.

| Area | Upstream jj | gritjitsu |
| --- | --- | --- |
| Objects (blob/tree/commit, extra headers, signatures) | `gix` object read/write | grit `Odb` read/write, own commit serializer that matches gix byte-for-byte |
| Refs, reflogs, compare-and-swap ref transactions | `gix::refs::transaction` | grit `RefStore` / `RefTransaction` (`lib/src/git_grit.rs`) |
| HEAD, linked worktrees | `gix` | grit `RefStore`, `repo.worktrees()` |
| Git index (`reset_head`, conflicts, intent-to-add) | `gix::index` | grit `Index` / `IndexEntry` |
| Remote config, refspecs, URL rewrites | `gix::config`, `gix::Remote` | grit `remotes()`, `ConfigFile` |
| `.gitignore` | `gix-ignore` | grit `IgnorePatternList` (added to grit for this) |
| Copy detection | gix tree diff with rewrites | grit `diff_trees` + `detect_copies` |
| fetch | `git fetch` subprocess | grit `Remote::fetch` (`lib/src/git_transport.rs`) |
| push | `git push --force-with-lease` subprocess | grit `Remote::push` with expected-old-value checks |
| Remote default branch | `git remote show` | grit `ls_remote` HEAD symref |
| worktree add/repair/prune | `git worktree` | grit `worktree` APIs |
| `jj util gc` | `git gc` | `grit_lib::maintenance::gc` |
| Test fixtures | gix + `git` | grit-lib (a few helpers still use `git` as a reference implementation) |

Public jj-lib names such as `GitSubprocessOptions` and `GitSubprocessCallback`
were kept, so the CLI barely changed, even though nothing runs as a
subprocess any more. The `git.executable-path` setting was removed.

### Behavior differences

- **Environment:** `GitSubprocessOptions::environment` is layered over the
  process environment when grit loads configuration, so `GIT_CONFIG_*`,
  `GIT_ASKPASS` and similar variables still work. `GIT_TRACE` does nothing,
  because there is no `git` process to trace.
- **Repository opening:** jj infers the work tree of a non-bare `.git` dir and
  follows `.git` files itself, as gitoxide did. New repos get `hooks/` and
  `info/exclude` like Git's default template.
- **Config layout:** the internal repo's `.git/config` lists
  `repositoryformatversion` first and omits the default
  `logallrefupdates = false`. A few snapshots and library error messages
  changed because of this.
- **Default branch:** it comes from the remote's `HEAD` symref. A remote with
  a detached `HEAD` falls back to `init.defaultBranch`/`master`, then to the
  first branch at `HEAD`'s commit.
- **Progress:** remote "Counting/Compressing objects" progress is parsed from
  sideband lines. Local progress comes from grit's progress reporting.

### Repository formats

`grit-bench/formats.sh` runs the same steps against both builds:

| | upstream jj | gritjitsu |
| --- | --- | --- |
| Colocated repo with **reftable** refs (`git init --ref-format=reftable`) | `jj git init` succeeds, but jj sees none of Git's refs and doesn't write bookmarks or `HEAD` back | works: refs import and export both ways, and the repo stays reftable |
| Clone, fetch and push with a reftable remote | works (done by the `git` CLI) | works |
| SHA-256 repo created by jj (`git.object-hash = "sha256"`) | works | works |
| Clone, fetch and push between SHA-256 repos, with `git.object-hash = "sha256"` | works | works |
| `jj git clone` of a SHA-256 remote without that setting | fails: `mismatched algorithms: client sha1; server sha256` | fails: grit switches the new repo to SHA-256 during the fetch, but jj already opened it as SHA-1 |

Reftable support comes from grit-lib's ref store; jj only talks to it
through the generic ref API. Neither build detects the remote's object
format on clone yet.

## Changes to grit-lib

The port needed fixes and additions to grit-lib. They live on the fork
[schacon/grit](https://github.com/schacon/grit), and each one is sent
upstream:

| Change | Status |
| --- | --- |
| `IgnorePatternList`: match single paths against an exclude file | merged, [gitbutlerapp/grit#2137](https://github.com/gitbutlerapp/grit/pull/2137) |
| Strip at most one trailing CR from ignore lines | on grit `main` |
| `detect_copies` reports source modes when no source entries are given | on grit `main` (#2146) |
| `detect_copies` only treats created paths as copy destinations, like `git diff -C` | branch `detect-copies-modified-not-dest`, PR after #2146 |
| Local pushes run receive hooks and accept push options | [#2148](https://github.com/gitbutlerapp/grit/pull/2148) |
| `git worktree add --orphan`, and worktrees for bare repos | [#2149](https://github.com/gitbutlerapp/grit/pull/2149) |
| `worktree prune` checks the recorded `.git` file, like Git | [#2150](https://github.com/gitbutlerapp/grit/pull/2150) |
| Skip stat and readlink for regular loose ref files (found by the benchmarks below; the double `refs/` walk was fixed upstream independently) | [#2194](https://github.com/gitbutlerapp/grit/pull/2194) |

Known grit issue, not yet reported: `ls_remote` drops a local remote's
`HEAD` when asked to filter for `HEAD`. jj works around it by not filtering.

## Building

`Cargo.toml` points at grit-lib by path: `../grit-jj/grit-lib`. That
directory is a checkout of the schacon/grit fork with all the changes above
applied. Some of those changes are still unmerged or unpushed branches, so
this repo can't yet be built from a fresh clone. Once they land, the
dependency can switch to a git or crates.io version of grit-lib.

```sh
cargo build --release --bin jj
cargo nextest run --workspace
```

## Benchmarks

### Setup

- **Builds:** upstream jj at the same base commit (gitoxide plus Git 2.50.1,
  Apple Git-155) against gritjitsu. Both are release builds with the same
  Cargo profile.
- **Machine:** Apple M5 Max, 18 cores, macOS. Another workload (a VM using
  about 7 cores) was running throughout, so CPU time is reported alongside
  wall time.
- **Repos:** each is cloned from a local bare repository, so the network
  isn't measured:

  | | repo | commits | files | refs |
  | --- | --- | ---: | ---: | ---: |
  | S | [ticgit](https://github.com/schacon/ticgit) | 606 | 83 | 12 |
  | M | [jj](https://github.com/jj-vcs/jj) | 12,000 | 733 | 55 |
  | L | [git.git](https://github.com/git/git) | 82,348 | 4,854 | 1,023 |

- **Runs:** [hyperfine](https://github.com/sharkdp/hyperfine) with
  hermetic config. Every benchmark ran in two passes from fresh fixtures, one
  with upstream first and one with gritjitsu first. The medians below pool
  both passes, and the two passes agree to within about 0.1×.
- **Fixtures:** clones are colocated (`jj git clone` default). Commands that
  change state restore their fixture before each run.
- **Correctness:** before any timing, both binaries produced identical
  bookmarks, commit sets and Git refs on all three repos, and identical
  `file annotate` output.

The scripts are in [`grit-bench/`](grit-bench). The full table, with
standard deviations, CPU ratios and runs, is in
[`grit-bench/RESULTS.md`](grit-bench/RESULTS.md).

### Results

Wall-clock medians. A ratio below 1 means gritjitsu is faster.

| operation | S upstream → grit | S ratio | M upstream → grit | M ratio | L upstream → grit | L ratio |
| --- | --- | ---: | --- | ---: | --- | ---: |
| `git clone` | 302 → 205 ms | **0.68×** | 1783 → 1023 ms | **0.57×** | 10.7 → 12.4 s | 1.15× |
| `git fetch`, nothing new | 45 → 17 ms | **0.37×** | 51 → 20 ms | **0.39×** | 274 → 113 ms | **0.41×** |
| `git fetch`, catch up 50/500/2000 commits | 122 → 97 ms | **0.79×** | 527 → 421 ms | **0.80×** | 4.6 → 6.2 s | 1.37× |
| `git push`, new bookmark with 20 commits | 200 → 78 ms | **0.39×** | 251 → 106 ms | **0.42×** | 656 → 267 ms | **0.41×** |
| `git init --colocate` (existing repo) | 140 → 144 ms | 1.03× | 308 → 310 ms | 1.01× | 1584 → 1781 ms | 1.12× |
| `git import` (no-op) | 12 → 13 ms | 1.06× | 14 → 15 ms | 1.09× | 47 → 49 ms | 1.04× |
| `git export` (no-op) | 11 → 12 ms | 1.08× | 14 → 14 ms | 1.02× | 48 → 50 ms | 1.03× |
| `status` | 11 → 12 ms | 1.13× | 15 → 16 ms | 1.05× | 48 → 51 ms | 1.05× |
| `log` | 14 → 15 ms | 1.10× | 17 → 18 ms | 1.05× | 51 → 52 ms | 1.02× |
| `log` of every ancestor of `trunk()` | 17 → 18 ms | 1.08× | 94 → 91 ms | 0.97× | 597 → 597 ms | 1.00× |
| `diff --stat` across 100 commits | 86 → 21 ms | **0.25×** | 105 → 72 ms | **0.69×** | 1256 → 208 ms | **0.17×** |
| `file annotate` | 17 → 19 ms | 1.10× | 953 → 769 ms | **0.81×** | 5.5 → 4.8 s | **0.87×** |
| `status` after editing a file | 45 → 47 ms | 1.03× | 50 → 50 ms | 1.00× | 88 → 88 ms | 1.00× |
| `new` | 40 → 42 ms | 1.05× | 44 → 46 ms | 1.04× | 81 → 87 ms | 1.08× |
| `util gc` | 101 → 56 ms | **0.55×** | 637 → 398 ms | **0.62×** | 5.4 → 5.5 s | 1.03× |

**Geometric mean over all 45: 0.80× wall time, 0.82× CPU time.**

Network, cloning and fetching from GitHub over HTTPS (two passes each, so
expect noise):

| operation | upstream | gritjitsu | ratio |
| --- | ---: | ---: | ---: |
| clone ticgit | 4.22 s | 3.46 s | 0.82× |
| clone ripgrep | 4.93 s | 5.67 s | 1.15× |
| fetch ticgit, nothing new | 0.75 s | 0.71 s | 0.94× |
| fetch ripgrep, nothing new | 1.07 s | 1.04 s | 0.98× |

### What the numbers say

**Faster:**

- **Push and no-op fetch** against local remotes are 2.4–2.7× faster. jj no
  longer starts a `git` process (which starts `upload-pack` or
  `receive-pack`) and parses its output. Over the network, round trips
  dominate and the two are even.
- **`diff --stat`** is up to 6× faster. jj asks the Git backend for copy
  records when it diffs, and that now goes through grit's tree diff and
  `detect_copies` instead of gitoxide's rename tracking. This is the likely
  source of the speedup, but it hasn't been profiled.
- **`util gc`** is faster on small and medium repos, and `file annotate` is
  13–19% faster on large histories.

**About the same:** everyday commands (`status`, `log`, `new`, and the
import/export of Git refs that colocated repos do on every command) are
within a few percent. On tiny repos there is a fixed cost of about 1 ms.

**One regression the benchmarks found and fixed:**

- In the first run, every command on git.git was about 25 ms slower.
  `status` took 66 ms against upstream's 43 ms.
- The cause was in grit-lib: listing loose refs walked the same `refs/`
  directory twice, reading all ~1,000 tag files twice, and it made two extra
  syscalls per file.
- The fix cut listing from 38 ms to 15 ms, slightly faster than gitoxide, and
  the numbers above include it. grit-lib `main` has since fixed the double
  walk independently; the syscall part is
  [#2194](https://github.com/gitbutlerapp/grit/pull/2194).

**Slower:**

- **Local clone and fetch of large repositories:** cloning git.git from a
  local path is 1.15× slower and peaks at about 2 GB of memory against
  upstream's 0.6 GB. Catching up 2000 commits is 1.37× slower and uses 5× the
  memory. grit-lib's local transport works out the object set and writes a
  new pack in-process. A profile shows it hashing the full pack path on every object read
  (`pack_id_for_in_context`), and it spends about twice as much time in the
  kernel as `git fetch`. That's a grit-lib target for optimization; network fetches
  take a different path.
- **Peak memory** is generally higher for transfers (2× for network clones).
  Note that the peak RSS hyperfine reports for upstream is its largest single
  process, which may be jj or one of the `git` processes it starts.
- **`init --colocate`** on git.git is 12% slower.

## Reproducing

```sh
cd grit-bench
mkdir -p bin
cp /path/to/upstream/target/release/jj bin/jj-upstream
cp ../target/release/jj bin/jj-grit
BENCH_SMALL=~/src/ticgit BENCH_MEDIUM=~/src/jj BENCH_LARGE=~/src/git bash setup.sh
bash run2.sh       # local benchmarks, two passes
python3 summarize.py results
bash net.sh        # network benchmarks
bash formats.sh grit; bash formats.sh upstream   # reftable and SHA-256 checks
```

## License

Apache 2.0, like upstream jj. See [LICENSE](LICENSE).
