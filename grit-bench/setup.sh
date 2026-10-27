#!/bin/bash
# Builds the fixtures for bench.sh: bare remotes, an "old" remote for fetch
# catch-up, and jj clones per binary. Fixtures are built at the paths the
# benchmarks use and saved under saved/, so restoring one is a copy back to
# the same path (jj warns when a repo moves).
set -euo pipefail
B=$(cd "$(dirname "$0")" && pwd)
source "$B/env.sh"

# name source branch behind-count
# Local clones of the benchmark repositories, e.g. https://github.com/schacon/ticgit,
# https://github.com/jj-vcs/jj and https://github.com/git/git.
REPOS=(
  "S ${BENCH_SMALL:?path to a ticgit clone} master 50"
  "M ${BENCH_MEDIUM:?path to a jj clone} main 500"
  "L ${BENCH_LARGE:?path to a git.git clone} master 2000"
)

rm -rf "$B/remotes" "$B/pristine" "$B/work" "$B/saved"
mkdir -p "$B/remotes" "$B/work" "$B/saved"

for spec in "${REPOS[@]}"; do
  read -r name src branch behind <<<"$spec"
  R=$B/remotes/$name.git
  git clone -q --bare "$src" "$R"
  git -C "$R" symbolic-ref HEAD "refs/heads/$branch"
  git -C "$R" repack -adq
  old=$(git -C "$R" rev-parse "$branch~$behind")
  # Remote with only the default branch, $behind first-parent commits back.
  git init -q --bare -b "$branch" "$B/remotes/$name-old.git"
  git -C "$R" push -q "$B/remotes/$name-old.git" "$old:refs/heads/$branch"
  echo "$name: $(git -C "$R" for-each-ref | wc -l | tr -d ' ') refs, $(git -C "$R" rev-list --all | wc -l | tr -d ' ') commits, $(git -C "$R" ls-tree -r HEAD | wc -l | tr -d ' ') files"

  for bin in upstream grit; do
    W=$B/work/$bin/$name
    mkdir -p "$W"
    jjb=$B/bin/jj-$bin
    # Clone used for read-only commands.
    "$jjb" --quiet git clone "$R" "$W/clone"
    # Clone for commands that write (snapshot, new, gc).
    "$jjb" --quiet git clone "$R" "$W/rw"
    # Clone of the old remote, pointed at the full one, for fetch catch-up.
    "$jjb" --quiet git clone "$B/remotes/$name-old.git" "$W/behind"
    "$jjb" -R "$W/behind" git remote set-url origin "$R"
    # 20 new commits on a new bookmark, to push to a copy of the remote.
    "$jjb" --quiet git clone "$R" "$W/push"
    cp -cR "$R" "$W/push-remote.git"
    "$jjb" -R "$W/push" git remote set-url origin "$W/push-remote.git"
    for i in $(seq 1 20); do
      printf 'line %s\n' "$i" >>"$W/push/bench-push-file.txt"
      head -c 20000 /dev/urandom | base64 >"$W/push/bench-blob-$i.txt"
      "$jjb" -R "$W/push" --quiet commit -m "bench commit $i"
    done
    "$jjb" -R "$W/push" --quiet bookmark create bench-push -r @-
    # Plain git checkout for `jj git init --colocate`.
    git clone -q "$R" "$W/gitonly"
    mkdir -p "$B/saved/$bin"
    cp -cR "$W" "$B/saved/$bin/$name"
  done
done
