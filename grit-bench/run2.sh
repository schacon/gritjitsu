#!/bin/bash
# Two passes from fresh fixtures, in opposite binary order.
B=$(cd "$(dirname "$0")" && pwd)
reset() {
  rm -rf "$B/work" && mkdir -p "$B/work/upstream" "$B/work/grit"
  for bin in upstream grit; do for n in S M L; do cp -cR "$B/saved/$bin/$n" "$B/work/$bin/$n"; done; done
}
mkdir -p "$B/results"
reset; PASS=a BINS=upstream,grit bash "$B/bench.sh" S M L
reset; PASS=b BINS=grit,upstream bash "$B/bench.sh" S M L
