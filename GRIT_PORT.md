# Porting jj from gitoxide + `git` to grit-lib

Goal: a jj whose only Git implementation is
[grit-lib](https://github.com/gitbutlerapp/grit). No `gix*` crates and no
`git` subprocesses, with the full test suite passing.

Baseline (upstream `5f0451bdd`): `cargo nextest run --workspace` gives
3491/3491 passing.

grit-lib changes were developed on the fork <https://github.com/schacon/grit>
and sent upstream as PRs. All of them are now on grit `main`, and `Cargo.toml`
pins grit-lib to an upstream `main` commit.

## Research: what jj uses Git for

| Area | Where | Today | grit-lib replacement |
| --- | --- | --- | --- |
| Init/open repo (bare, colocated, external, sha1/sha256) | `git_backend.rs` | `gix::ThreadSafeRepository::{init_opts,open_opts}` | `Repository::{init,open}` with `InitOptions` / `RepositoryOptions` |
| Object read/write (blob, tree, commit + extra headers, gpgsig) | `git_backend.rs` | `gix::objs::*`, `write_object`, `write_buf_with_known_id` | `Objects::{read,write,hash}`, `parse_commit` / `serialize_commit`, `parse_tree` / `serialize_tree`, `extra_headers` |
| Shallow roots | `git_backend.rs` | `repo.shallow_commits()` | read `$GIT_DIR/shallow` directly |
| Copy/rename detection | `git_backend.rs::get_copy_records` | `gix` tree diff with rewrites | `diff::diff_trees` + `detect_renames` / `detect_copies` |
| Refs: list, read, peel, CAS transactions, reflog | `git.rs`, `git_backend.rs` | `gix::refs::transaction::*` | `RefStore::prepare(RefTransaction)` with `RefUpdate` / `Expected` / `ReflogUpdate` |
| HEAD / detached HEAD / linked worktrees | `git.rs` | `repo.head()`, `repo.worktrees()` | `RefStore` on `HEAD`, `repo.worktrees().list()` |
| Index write (reset_head, conflicts, intent-to-add) | `git.rs` | `gix::index::File` | `index::Index` / `IndexEntry` |
| Operation state (merge/rebase in progress) | `git.rs` | `repo.state()` | `state::detect_in_progress` |
| Remote config (add/remove/rename/set-url, refspecs) | `git.rs`, CLI | `gix::config::File`, `gix::Remote` | `repo.remotes()`, `ConfigFile`, grit refspec parsing |
| Git URL parsing | CLI `git_util.rs` | `gix::url` | grit url helpers, or a small local parser |
| `.gitignore` matching | `gitignore.rs` | `gix-ignore` | needs a standalone pattern-list matcher in grit (PR) |
| Excludes file / global config lookup | CLI `cli_util.rs` | `gix::config::File::from_globals` | `ConfigSet::load` |
| Tempfile cleanup on signal | CLI `cleanup_guard.rs` | `gix::tempfile` registry | drop (grit uses `tempfile`/lockfiles) |
| `git fetch` | `git_subprocess.rs` | subprocess | `repo.remotes().fetch` |
| `git push` | `git_subprocess.rs` | subprocess | `repo.remotes().push` |
| `git remote show` (default branch) | `git_subprocess.rs` | subprocess | `ls_remote` HEAD symref |
| `git worktree add/repair/prune` | `git_subprocess.rs` | subprocess | `repo.worktrees()` |
| `git gc` | `git_backend.rs` | subprocess | `maintenance::gc` |
| Test fixtures | `testutils`, tests | gix + `git` CLI | grit-lib |

## Plan

Phase A replaces gitoxide. During the transition `GitBackend` holds a gix
handle and a grit handle side by side, so each step keeps the suite green.

1. Add the grit-lib dependency and open a grit `Repository` next to gix in
   `GitBackend`.
2. Port `gitignore.rs` (grit PR: standalone ignore pattern matcher).
3. Port object read/write, shallow roots and conflict trees in `git_backend.rs`.
4. Port no-gc refs and copy detection in `git_backend.rs`.
5. Port ref import/export, HEAD handling and worktree enumeration in `git.rs`.
6. Port Git index writing (`reset_head`, conflict stages, intent-to-add).
7. Port remote management (add/remove/rename/set-url, refspecs).
8. Port the CLI (`git_util`, `init`, `clone`, `colocation`, `remote`,
   `gerrit`, `cli_util`).
9. Port `testutils` and the tests off gix.
10. Remove `gix` and `gix-ignore` from every `Cargo.toml`.

Phase B removes the `git` subprocesses.

11. fetch through grit-lib transport (progress, sideband, prune, depth, refspecs).
12. push through grit-lib (CAS leases, porcelain-equivalent results).
13. Default-branch discovery (replaces `git remote show`).
14. worktree add/repair/prune.
15. gc.
16. Remove `git.executable-path` and `git_subprocess.rs`; port the test
    helpers that shell out to `git`.

## Progress log

Each step is one commit on this fork, plus PRs to grit for library changes.

- **Steps 1–3**: grit-lib is a dependency. `GitBackend` reads and writes blobs,
  trees and commits through grit-lib. Commits are serialized byte-for-byte as
  before, so commit ids don't change. `.gitignore` matching uses grit's new
  `IgnorePatternList`. grit PRs: standalone ignore pattern list; strip only one
  trailing CR from ignore lines. Tests: 3491/3491.
- **Step 4**: `refs/jj/keep/*` no-gc refs go through grit's `RefStore`
  transactions. Copy detection uses `diff_trees` + `detect_copies`. jj keeps
  its old gix-era semantics: empty files are never copy sources, and modified
  files are copy sources with their *new* contents. grit PRs: report source
  modes on copies; only created paths become copy destinations (matches
  `git diff -C`). Tests: 3491/3491.
- **Step 5**: ref import/export, HEAD import/detach/reset, linked-worktree
  HEADs, remote ref rename/removal, fetch pruning and post-push ref updates go
  through grit-lib (`lib/src/git_grit.rs` has the shared helpers). Reflogs are
  written the way Git decides (existing reflog or `core.logAllRefUpdates`).
  Deleting a ref also deletes its reflog, which grit leaves to callers. Three
  snapshots changed because they print the Git library's error message.
  Tests: 3491/3491.
- **Step 6**: the Git index (`reset_head`, conflict stages, intent-to-add
  entries) is built and written with grit-lib's `Index`. Resolved parent trees
  are read straight from the Git tree, and stat data is carried over from the
  old index for unchanged entries. Tests: 3491/3491.
- **Steps 7–10**: gitoxide is gone (`cargo tree` and `Cargo.lock` have no
  `gix*`). Remote config (add/remove/rename/set-url, refspecs, URL rewrites),
  repository init, the CLI (URL parsing, `remote list`, gerrit, excludes file,
  colocation) and the test helpers all use grit-lib. `testutils::git` now
  wraps a grit `Repository` with small helpers similar to the old gix calls.
  `GitBackend::git_repo()` returns `&grit_lib::repo::Repository`.
  Behavior notes:
  - jj infers the work tree of a non-bare `.git` dir and follows `.git`
    files, as gix did. grit's `open` doesn't infer either.
  - New repos get `hooks/` and `info/exclude` like Git's default template.
    grit's init omits them.
  - The internal repo's config lists `repositoryformatversion` first and omits
    the default `logallrefupdates = false` (grit's init layout). Snapshots of
    `.git/config` and some library error messages changed accordingly.
  Tests: 3491/3491.
- **Steps 11–16**: no `git` subprocesses remain. `lib/src/git_transport.rs`
  runs fetch, push, default-branch lookup and worktree add/repair/prune
  through grit-lib, and `jj util gc` uses `grit_lib::maintenance::gc`.
  `git.executable-path` is gone, and so is the minimum Git version.
  `git_subprocess.rs` keeps only the error, callback and progress types, so
  the public `GitSubprocess*` names stay the same for the CLI.
  Behavior notes:
  - `GitSubprocessOptions::environment` is layered over the process
    environment when grit loads configuration, so `GIT_CONFIG_*`,
    `GIT_ASKPASS` and similar variables still apply. `GIT_TRACE` does
    nothing, so the env-option tests now redirect the remote with a
    `GIT_CONFIG_*` `insteadOf` rule.
  - Remote "Counting/Compressing objects" progress comes from sideband
    lines; local progress comes from grit's progress sink.
  - The default branch comes from `ls-remote` HEAD symrefs. A remote with a
    detached HEAD falls back to `init.defaultBranch`/`master` and then the
    first branch at HEAD's commit.
  - Pushes don't update remote-tracking refs themselves; jj records them as
    before.
  Ten unit tests that parsed `git` output were removed, and the two "missing
  git executable" clone tests became one test that clones with an empty
  `PATH`. Test helpers and a few tests still run `git` as a reference
  implementation; jj itself never does. Tests: 3480/3480.

- **Benchmarks**: `grit-bench/` compares this build against upstream jj on
  three repos. Results and analysis are in `README.md`; the full table is in
  `grit-bench/RESULTS.md`. The first run found grit listing loose refs twice,
  which made every colocated command on git.git about 25 ms slower; the grit
  fix above removed that.

### grit-lib changes

| Change | Status |
| --- | --- |
| `IgnorePatternList` (standalone ignore matching) | merged: gitbutlerapp/grit#2137 |
| Strip at most one trailing CR from ignore lines | on upstream `main` (d4988e6ca, pushed directly by mistake; see below) |
| `detect_copies` reports source modes | merged: gitbutlerapp/grit#2146 |
| `detect_copies` only treats created paths as copy destinations | integrated: gitbutlerapp/grit#2195 (4992b10ab) |
| Tell the pusher when the update hook declines a ref (main had since added receive hooks on local pushes) | integrated: gitbutlerapp/grit#2148 (4992b10ab) |
| `WorktreeBranch::Orphan` (`git worktree add --orphan`); worktrees for bare repos | merged: gitbutlerapp/grit#2149 |
| `worktree prune` checks the recorded `.git` file, like Git | merged: gitbutlerapp/grit#2150 |
| Skip stat and readlink for regular loose ref files (double `refs/` walk fixed upstream as `store_bases`) | merged: gitbutlerapp/grit#2194 |
| Don't trust a pack listing left by another store on the thread | integrated: gitbutlerapp/grit#2217 (4992b10ab) |
| Faster local clone and fetch | integrated: gitbutlerapp/grit#2218 (4992b10ab) |

Incident: creating a PR for the CR fix force-pushed upstream `main`, because
that branch had been created from `upstream/main` and still tracked it. The
merge it dropped (ec05183a6, `factory/revs-count-paint`) was restored by a
regular merge push (0c5982a64). The branch's tracking config has been removed.
