#!/bin/sh
# run.sh — single entrypoint used by every command in herdr-plugin.toml.
# Execs the built binary with the given mode (hook | ui | run | plan). When the
# plugin was `herdr plugin link`ed (which skips [[build]]) and never built, it
# fails loudly: one herdr notification + one stderr line in `herdr plugin log`.
set -u
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
for bin in "$root/target/release/herdr-worktree-setup" "$root/target/debug/herdr-worktree-setup"; do
  if [ -x "$bin" ]; then
    exec "$bin" "$@"
  fi
done
herdr=${HERDR_BIN_PATH:-herdr}
"$herdr" notification show "Worktree Setup: not built" \
  --body "Run: sh $root/scripts/fetch-or-build.sh" --sound request >/dev/null 2>&1 || true
echo "worktree-setup: binary not found under $root/target; run: sh $root/scripts/fetch-or-build.sh" >&2
exit 1
