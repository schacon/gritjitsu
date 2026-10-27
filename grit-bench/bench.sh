#!/bin/bash
# Times upstream jj (gitoxide + git subprocesses) against jj on grit-lib.
# Usage: bench.sh [check] [repo...]   (check: run each command once, untimed)
set -uo pipefail
B=$(cd "$(dirname "$0")" && pwd)
source "$B/env.sh"

MODE=time
if [ "${1:-}" = check ]; then MODE=check; shift; fi
NAMES=("$@")
[ $# -eq 0 ] && NAMES=(S M L)
mkdir -p "$B/results"

J="$B/bin/jj-{bin}"

# runs slow-runs annotate-file branch
repo_params() {
  case $1 in
    S) echo "15 10 README.md master" ;;
    M) echo "10 5 CHANGELOG.md main" ;;
    L) echo "5 3 README.md master" ;;
  esac
}

# bench <repo> <id> <runs> <warmup> <prepare|-> <command>
bench() {
  local name=$1 id=$2 runs=$3 warmup=$4 prepare=$5 cmd=$6
  [[ -n "${ONLY:-}" && ! "$id" =~ $ONLY ]] && return
  if [ "$MODE" = check ]; then
    for bin in upstream grit; do
      [ "$prepare" != - ] && bash -c "${prepare//\{bin\}/$bin}" >/dev/null 2>&1
      if ! out=$(bash -c "${cmd//\{bin\}/$bin}" 2>&1); then
        echo "FAIL $name/$id/$bin: $out" | head -5
      else
        echo "ok   $name/$id/$bin"
      fi
    done
    return
  fi
  local args=(-N --style basic -L bin "${BINS:-upstream,grit}" -r "$runs" -w "$warmup"
    --export-json "$B/results/$name-$id${PASS:+.$PASS}.json" --command-name "$id ({bin})")
  [ "$prepare" != - ] && args+=(--prepare "$prepare")
  echo "=== $name $id"
  hyperfine "${args[@]}" "$cmd" 2>&1 | grep -E "Time|±|faster|Warning" | head -8
}

for name in "${NAMES[@]}"; do
  R=$B/remotes/$name.git
  W="$B/work/{bin}/$name"
  S="$B/saved/{bin}/$name"
  restore() { echo "rm -rf $W/$1 && cp -cR $S/$1 $W/$1"; }
  read -r runs slow annotate branch <<<"$(repo_params "$name")"
  base=$(git -C "$R" rev-parse "$branch~100")

  # Remote operations against local bare repositories.
  bench "$name" clone "$slow" 0 "rm -rf $W/newclone" \
    "$J --quiet git clone $R $W/newclone"
  bench "$name" fetch-noop "$runs" 2 - "$J -R $W/clone git fetch"
  bench "$name" fetch-catchup "$runs" 0 "$(restore behind)" "$J -R $W/behind git fetch"
  bench "$name" push-new-bookmark "$runs" 0 \
    "$(restore push) && $(restore push-remote.git)" \
    "$J -R $W/push git push -b bench-push"
  bench "$name" init-colocate "$slow" 0 "$(restore gitonly)" \
    "$J git init --colocate $W/gitonly"
  bench "$name" git-import-noop "$runs" 2 - "$J -R $W/clone git import"
  bench "$name" git-export-noop "$runs" 2 - "$J -R $W/clone git export"

  # Local commands that read through the Git backend.
  bench "$name" status "$runs" 3 - "$J -R $W/clone status"
  bench "$name" log "$runs" 3 - "$J -R $W/clone log"
  bench "$name" log-all-descriptions "$runs" 1 - \
    "$J -R $W/clone log --no-graph -r '::trunk()' -T 'commit_id.short() ++ description.first_line() ++ \"\\n\"'"
  bench "$name" diff-stat-100 "$runs" 1 - \
    "$J -R $W/clone diff --stat --from $base --to 'trunk()'"
  bench "$name" file-annotate "$slow" 1 - \
    "$J -R $W/clone file annotate -r 'trunk()' $W/clone/$annotate"

  # Local commands that write through the Git backend.
  bench "$name" snapshot-dirty "$runs" 1 "echo \$RANDOM >>$W/rw/bench-dirty.txt" \
    "$J -R $W/rw status"
  bench "$name" new "$runs" 1 - "$J -R $W/rw new"
  bench "$name" util-gc "$slow" 0 "$(restore rw)" "$J -R $W/rw util gc --expire=now"
done
