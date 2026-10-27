#!/bin/bash
# Network comparison: clone and no-op fetch from GitHub over HTTPS, in both
# binary orders.
set -uo pipefail
B=$(cd "$(dirname "$0")" && pwd)
source "$B/env.sh"
mkdir -p "$B/net" "$B/results-net"
for spec in "S https://github.com/schacon/ticgit.git 6" "M https://github.com/BurntSushi/ripgrep.git 4"; do
  read -r n url runs <<<"$spec"
  for order in upstream,grit grit,upstream; do
    pass=$([ "$order" = upstream,grit ] && echo a || echo b)
    hyperfine -N --style basic -L bin "$order" -r "$runs" --prepare "rm -rf $B/net/{bin}-$n" \
      --export-json "$B/results-net/$n-net-clone.$pass.json" \
      "$B/bin/jj-{bin} --quiet git clone $url $B/net/{bin}-$n"
  done
  for order in upstream,grit grit,upstream; do
    pass=$([ "$order" = upstream,grit ] && echo a || echo b)
    hyperfine -N --style basic -L bin "$order" -r 8 -w 1 \
      --export-json "$B/results-net/$n-net-fetch-noop.$pass.json" \
      "$B/bin/jj-{bin} -R $B/net/{bin}-$n git fetch"
  done
done
