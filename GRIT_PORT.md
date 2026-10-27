# Porting jj from gitoxide + `git` to grit-lib

Goal: a jj whose only Git implementation is
[grit-lib](https://github.com/gitbutlerapp/grit). No `gix*` crates and no
`git` subprocesses, with the full test suite passing.

Baseline (upstream `5f0451bdd`): `cargo nextest run --workspace` gives
3491/3491 passing.

grit-lib changes live on the fork <https://github.com/schacon/grit>. Fixes and
API additions that make sense on their own go upstream as PRs.

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
