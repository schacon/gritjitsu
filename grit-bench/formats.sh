#!/bin/bash
# Exercises reftable and SHA-256 repositories with one jj binary.
# Usage: formats.sh <upstream|grit>
bin=$1
B=$(cd "$(dirname "$0")" && pwd)
source "$B/env.sh"
JJ=$B/bin/jj-$bin
W=$B/formats-$bin
rm -rf "$W" && mkdir -p "$W" && cd "$W" || exit 1

step() {
  local label=$1; shift
  if out=$("$@" 2>&1); then
    echo "ok    $label"
  else
    echo "FAIL  $label"
    echo "$out" | sed 's/^/        /' | head -6
  fi
}
check() {
  local label=$1 expected=$2 actual=$3
  if [ "$actual" = "$expected" ]; then echo "ok    $label"; else
    echo "FAIL  $label"; echo "        expected: $expected"; echo "        actual:   $actual"; fi
}

echo "== reftable: colocated repo"
git init -q -b main --ref-format=reftable rt && git -C rt commit -q --allow-empty -m one
step "jj git init --colocate" "$JJ" git init --colocate rt
step "jj log" "$JJ" -R rt log -r 'all()'
check "jj sees main" "main" "$("$JJ" -R rt log --no-graph -r main -T 'bookmarks' 2>&1)"
echo two >rt/file
step "jj commit" "$JJ" -R rt commit -m two
step "jj bookmark set feature" "$JJ" -R rt bookmark set feature -r @-
check "git sees feature at jj's commit" \
  "$("$JJ" -R rt log --no-graph -r feature -T commit_id 2>&1)" \
  "$(git -C rt rev-parse refs/heads/feature 2>&1)"
check "git HEAD follows jj" \
  "$("$JJ" -R rt log --no-graph -r @- -T commit_id 2>&1)" "$(git -C rt rev-parse HEAD 2>&1)"
git -C rt commit -q --allow-empty -m "from git" && git -C rt branch -q from-git
step "jj imports a branch made by git" "$JJ" -R rt log -r from-git
check "ref format is still reftable" "reftable" "$(git -C rt rev-parse --show-ref-format)"
step "git fsck" git -C rt fsck --no-progress

echo "== reftable: remote"
git init -q --bare -b main --ref-format=reftable rt-remote.git
git -C rt push -q "$W/rt-remote.git" main
step "jj git clone from reftable remote" "$JJ" git clone rt-remote.git rt-clone
check "clone has main@origin" "$(git -C rt-remote.git rev-parse main)" \
  "$("$JJ" -R rt-clone log --no-graph -r main@origin -T commit_id 2>&1)"
git -C rt push -q "$W/rt-remote.git" feature
step "jj git fetch from reftable remote" "$JJ" -R rt-clone git fetch
check "fetched feature@origin" "$(git -C rt-remote.git rev-parse feature)" \
  "$("$JJ" -R rt-clone log --no-graph -r feature@origin -T commit_id 2>&1)"
echo three >rt-clone/file
"$JJ" -R rt-clone --quiet commit -m three 2>/dev/null
"$JJ" -R rt-clone --quiet bookmark set pushed -r @- 2>/dev/null
step "jj git push to reftable remote" "$JJ" -R rt-clone git push -b pushed
check "remote has pushed" "$("$JJ" -R rt-clone log --no-graph -r pushed -T commit_id 2>&1)" \
  "$(git -C rt-remote.git rev-parse refs/heads/pushed 2>&1)"
step "remote git fsck" git -C rt-remote.git fsck --no-progress

echo "== sha256: jj-created repo"
step "jj git init (sha256)" "$JJ" --config git.object-hash=sha256 git init s256
check "object format" "sha256" "$(git -C s256 rev-parse --show-object-format 2>&1)"
echo one >s256/file
step "jj commit" "$JJ" -R s256 commit -m one
step "jj bookmark set main" "$JJ" -R s256 bookmark set main -r @-
check "git sees main" "$("$JJ" -R s256 log --no-graph -r main -T commit_id 2>&1)" \
  "$(git -C s256 rev-parse refs/heads/main 2>&1)"
step "git fsck" git -C s256 fsck --no-progress

echo "== sha256: remote"
git init -q --bare -b main --object-format=sha256 s256-remote.git
step "jj git remote add" "$JJ" -R s256 git remote add origin "$W/s256-remote.git"
step "jj git push to sha256 remote" "$JJ" -R s256 git push -b main
check "remote has main" "$("$JJ" -R s256 log --no-graph -r main -T commit_id 2>&1)" \
  "$(git -C s256-remote.git rev-parse refs/heads/main 2>&1)"
step "jj git clone from sha256 remote (no config)" "$JJ" git clone s256-remote.git s256-clone-auto
step "jj git clone from sha256 remote (git.object-hash=sha256)" \
  "$JJ" --config git.object-hash=sha256 git clone s256-remote.git s256-clone
check "clone object format" "sha256" "$(git -C s256-clone rev-parse --show-object-format 2>&1)"
check "clone has main@origin" "$(git -C s256-remote.git rev-parse main)" \
  "$("$JJ" -R s256-clone log --no-graph -r main@origin -T commit_id 2>&1)"
git clone -q s256-remote.git s256-git && git -C s256-git commit -q --allow-empty -m "from git" \
  && git -C s256-git push -q origin main
step "jj git fetch from sha256 remote" "$JJ" -R s256-clone git fetch
check "fetched new main@origin" "$(git -C s256-remote.git rev-parse main)" \
  "$("$JJ" -R s256-clone log --no-graph -r main@origin -T commit_id 2>&1)"
step "remote git fsck" git -C s256-remote.git fsck --no-progress
