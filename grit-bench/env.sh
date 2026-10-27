# Hermetic environment shared by setup.sh and bench.sh. Expects $B (this
# directory) and the binaries bin/jj-upstream and bin/jj-grit.
mkdir -p "$B/home"
[ -f "$B/jjconfig.toml" ] || printf '%s\n' 'user.name = "Bench"' 'user.email = "bench@example.com"' \
  'ui.paginate = "never"' 'ui.color = "never"' >"$B/jjconfig.toml"
: >"$B/gitconfig"
export HOME=$B/home
export JJ_CONFIG=$B/jjconfig.toml
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=$B/gitconfig
export GIT_AUTHOR_NAME=Bench GIT_AUTHOR_EMAIL=bench@example.com
export GIT_COMMITTER_NAME=Bench GIT_COMMITTER_EMAIL=bench@example.com
# hyperfine must be on PATH. Upstream jj also needs `git`.
export PATH=$PATH:/usr/bin:/bin:/usr/sbin:/sbin
