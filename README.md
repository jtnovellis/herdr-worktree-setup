# herdr worktree-setup

A [herdr](https://herdr.dev) plugin that makes a **new git worktree immediately
usable**. When herdr creates a worktree, a small TUI opens beside it and:

1. **copies your dev state** from the main checkout — `.env*`, `.envrc`,
   `.dev.vars`, `*.local.*`, `.vercel/`, `.claude/settings.local.json`, IDE
   settings… (only gitignored files, never tracked ones);
2. **clones dependency & build caches** — `node_modules`, `.venv`, `target`,
   `.turbo`, `.next/cache`, … — with APFS `clonefile` / Linux reflink, so a
   2 GB `node_modules` lands in ~3 s with zero extra disk and stays fully
   isolated per branch;
3. runs **`mise trust`** and **`direnv allow`** when the repo uses them;
4. runs the right **dependency install** (pnpm / bun / yarn / npm, uv / poetry /
   pipenv, bundle, go, mix, composer, cargo) — a near no-op on a cloned tree,
   and it reconciles the branch's lockfile;
5. runs any **repo-defined steps** (`[[steps]]`, e.g. a migration).

Zero configuration: install it and it works. Everything below "Configuration"
is optional.

```
 Worktree Setup ─ clyras · feature/x ─ ~/.herdr/worktrees/clyras/feature-x
┌ steps 5/5 ─────────────────────────────────────────────────────────────┐
│✓ 1  copy dev state     6 items via reflink+APFS clone+symlink     0.0s │
│✓ 2  clone caches       26 dirs via APFS clone (129470 files)      2.7s │
│– 3  mise trust         no mise config                                  │
│– 4  direnv allow       no .envrc                                       │
│✓ 5  bun install        done                                       0.5s │
└────────────────────────────────────────────────────────────────────────┘
┌ output: bun install ───────────────────────────────────────────────────┐
│$ bun install                                                           │
│Checked 1364 installs across 1513 packages (no changes) [513.00ms]      │
└────────────────────────────────────────────────────────────────────────┘
 j/k step  J/K scroll  g/G top/end  q close       closing in 5s (any key cancels)
```

## Install

```sh
herdr plugin install jtnovellis/herdr-worktree-setup
```

The build step downloads a prebuilt binary for macOS (arm64, x86_64) or Linux
(x86_64, aarch64) over https and verifies its SHA-256; a mismatch aborts the
install. If there is no prebuilt binary for your platform it builds from source
with `cargo` (Rust 1.88+). Release binaries carry Sigstore build provenance:

```sh
gh attestation verify --repo jtnovellis/herdr-worktree-setup \
  herdr-worktree-setup-aarch64-apple-darwin.tar.gz
```

Requires herdr **0.8.0+** (the `worktree.created` event is emitted for
UI-created worktrees from 0.8).

Then create a worktree as usual — `prefix+…` in the sidebar, or
`herdr worktree create --branch feature/x` — and watch the split appear.

### Keybindings (optional)

```toml
# ~/.config/herdr/config.toml
[[keys.command]]
key = "prefix+alt+w"
type = "plugin_action"
command = "worktree-setup.run"      # (re)run setup for the current worktree workspace

[[keys.command]]
key = "prefix+alt+p"
type = "plugin_action"
command = "worktree-setup.plan"     # dry run: show what would happen
```

### Local development

```sh
git clone https://github.com/jtnovellis/herdr-worktree-setup
cd herdr-worktree-setup
cargo build --release          # `herdr plugin link` does not run [[build]]
herdr plugin link "$PWD"
```

## How it decides what to bring over

Only paths that are **gitignored in the main checkout** are candidates
(`git ls-files --others --ignored --exclude-standard --directory`). Tracked
files come from git; untracked-but-not-ignored files are someone's in-progress
work and are never touched. Nothing that already exists in the worktree is ever
overwritten. Nested git repositories inside ignored directories are skipped.

Each candidate is classified in this order — **exclude → symlink → clone →
copy** — against the pattern lists below, and the first match wins.

| Kind | Default patterns | How |
|---|---|---|
| **copy** (small dev state) | `.env`, `.env.*`, `.envrc`, `.envrc.local`, `.dev.vars`, `.flaskenv`, `.mise.local.toml`, `mise.local.toml`, `.tool-versions`, `.npmrc`, `*.local.{json,toml,yaml,yml}`, `docker-compose.override.y(a)ml`, `.vercel/`, `.wrangler/`, `.claude/settings.local.json`, `.vscode/`, `.idea/`, `local.properties`, `.herdr-worktree.local.toml` | reflink when possible, else byte copy. Symlinks are recreated as symlinks. |
| **clone** (caches) | `node_modules/`, `.venv/`, `venv/`, `target/`, `.next/cache/`, `.turbo/`, `.cache/`, `.parcel-cache/`, `.yarn/cache/`, `.gradle/`, `vendor/bundle/`, `vendor/`, `_build/`, `deps/`, `.dart_tool/`, `Pods/`, `.build/`, `.zig-cache/`, `.mypy_cache/`, `.ruff_cache/` | macOS: one atomic APFS `clonefile` of the whole tree. Linux: per-file reflink (btrfs/xfs). Otherwise a byte copy, capped at 2 GB per dir / 8 GB total — over the cap it is skipped and the install step rebuilds it. |
| **exclude** | `.git/`, `.direnv/`, `.turbo/daemon/`, `dist/`, `build/`, `out/`, `coverage/`, `tmp/`, `.next/`, `.env.{example,sample,template,dist}`, `.env.*.{example,sample}`, `*.log`, `*.pid`, `*.sock`, `.DS_Store` | never copied |
| **symlink** | *(none by default)* | `worktree/x → main/x` |

Patterns use gitignore conventions: a trailing `/` means directories only, a
leading `/` anchors at the checkout root, otherwise the pattern matches at any
depth (so `node_modules/` catches every package in a monorepo). Absolute
symlinks that point into the main checkout are rewritten to point into the
worktree.

### Why copy, not symlink?

A symlinked `.env` means an edit in the worktree silently changes the main
checkout's secrets, a per-branch `PORT` or `DATABASE_URL` is impossible, Docker
`COPY` and some bundlers copy the link rather than the content, and deleting the
main checkout breaks every worktree. Copies are isolated and tool-agnostic; the
files are tiny, so drift is the only cost.

A shared `node_modules` is worse: two branches with different lockfiles corrupt
each other on the first `install`. Cloning gives isolation **and** speed —
copy-on-write means bytes are only materialised when a file is actually
modified. If you really want one shared directory (a large dataset, say), add
it to `symlink = [...]`.

## Configuration (optional)

Three layers, later ones win: your **user** config, the repo's committed
`.herdr-worktree.toml`, and the gitignored `.herdr-worktree.local.toml`. Lists
extend the defaults; `exclude` always wins.

Both repo layers are read from your **source checkout**, never from the
worktree being set up — the branch you are about to review does not get to
reconfigure the tool setting it up. A config file that exists only on the
branch is ignored, and the pane says so. See [SECURITY.md](SECURITY.md).

```sh
$EDITOR "$(herdr plugin config-dir worktree-setup)/config.toml"
```

```toml
auto_close_secs = 5        # close the pane this long after success; 0 = stay open
focus = true               # focus the setup pane when it opens
placement = "split"        # split | tab | overlay | zoomed
direction = "down"         # split direction: down | right
install = true             # run the dependency install step
mode = "reflink"           # reflink | copy
copy_size_cap_mb = 2048    # per-directory byte-copy cap when reflink is unavailable
total_size_cap_mb = 8192   # total byte-copy cap per run
color = true               # keep ANSI colour in step output
mise_trust = true
direnv_allow = true
use_mise = true            # wrap installs/steps in `mise exec --` when the repo has mise config
use_direnv = true          # wrap installs/steps in `direnv exec` when the repo has .envrc
trust_repo_steps = true    # run [[steps]] from repo config (same trust as a postinstall script)
step_timeout_secs = 1800   # kill a step (and everything it forked) after this long; 0 = no limit

copy    = [".secrets/"]        # extend the defaults
clone   = [".pio/"]
symlink = ["datasets/"]
exclude = [".env.prod"]

[env]                      # extra environment for install/steps
NODE_OPTIONS = "--max-old-space-size=4096"
```

A repo-level `.herdr-worktree.toml` may only ever **restrict** the run. It can
turn `install`, `mise_trust`, `direnv_allow`, `use_mise` and `use_direnv` off,
and lower `copy_size_cap_mb`, `total_size_cap_mb` and `step_timeout_secs`. It
cannot turn them on, raise a cap, set `focus`, `placement`, `direction`,
`mode`, `color`, `auto_close_secs` or `trust_repo_steps`, add to `symlink`, or
set an environment variable that decides which program runs (`PATH`, `LD_*`,
`DYLD_*`, `NODE_OPTIONS`, …). Refusals are reported in the pane. It may add
custom steps:

```toml
[[steps]]
name = "migrate"
run = "pnpm db:migrate"
if = "prisma/schema.prisma"      # only when this path exists in the worktree
continue_on_error = true         # a failure does not mark the setup as failed
```

Custom steps run in the worktree through `/bin/sh -c`, because shell syntax is
the point of them. Dependency installs do not: their command comes from a fixed
allowlist and is executed as argv, so nothing in the repository can be
reinterpreted as syntax. Both are wrapped in `direnv exec` and `mise exec --`
when applicable, and run with `HWS_SOURCE`, `HWS_TARGET`, `HWS_BRANCH` and
`HWS_WORKSPACE_ID` in the environment. A step that outlives
`step_timeout_secs` is terminated along with anything it forked.

### Environment resolution

herdr may launch plugins with a minimal `PATH`. The plugin resolves your real
environment once per run through your login shell (`$SHELL -lic`, falling back
to `-lc`, then to the current environment) and fills in well-known tool
directories (`~/.local/bin`, `~/.bun/bin`, `~/.cargo/bin`, mise shims, Homebrew,
…). The probe runs with `HWS_ENV_PROBE=1` set, so an rc file can skip anything
slow or interactive when it sees that variable; the probe times out after 5 s.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, `↑` / `↓`, `1`–`9` | select a step (shows its output) |
| `J` / `K`, `PgUp` / `PgDn`, mouse wheel | scroll the output |
| `g` / `G` | top / bottom of the output |
| `r` | retry failed steps |
| `q` / `Esc` / `Ctrl-C` | close (kills a running step) |

Any key cancels the auto-close countdown. The pane stays open when a step
fails, and the sidebar title tracks progress (`Setup 3/5`, `Setup ✓`).

## Commands

```
herdr-worktree-setup hook                 # the worktree.created hook (herdr calls this)
herdr-worktree-setup ui [--plain]         # the pane entrypoint
herdr-worktree-setup run [--dry-run]      # the workspace actions
herdr-worktree-setup plan --source <main> --target <worktree> [--apply] [--tui]
```

`plan` lets you preview or run the whole pipeline outside herdr.

## Trust

A plugin is ordinary code running as you, and this one fires automatically on
every worktree creation — so `git worktree add`, which executes nothing on its
own, now runs code. [SECURITY.md](SECURITY.md) is the full trust model; the
short version:

- **Configuration comes from your checkout, never from the branch**, and a repo
  config may only ever restrict what runs.
- **Reads** are confined to the source checkout, **writes** to the worktree:
  every destination is resolved component by component and refused if any
  component is a symlink, so a committed symlink cannot redirect a copy — your
  `.env` included — somewhere else. Nothing existing is overwritten.
- **What still runs branch code by design:** the dependency install (the
  branch's `postinstall` scripts) and repo `[[steps]]`. `mise trust` and
  `direnv allow` grant *persistent* execution rights to a `mise.toml` /
  `.envrc`. Each has its own switch.
- **Your secrets move into a tree the branch controls.** Whether the copied
  `.env` stays gitignored there is up to the branch's `.gitignore`; the plugin
  warns loudly about every copied file the branch does not ignore, so a routine
  `git add -A` cannot quietly commit your credentials.

To review an untrusted branch with nothing from it executed:

```toml
install = false
trust_repo_steps = false
mise_trust = false
direnv_allow = false
```

`worktree-setup.plan` shows every command and every file before anything runs.

## License

MIT
