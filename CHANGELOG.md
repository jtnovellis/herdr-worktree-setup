# Changelog

## 0.1.2

Correctness and honesty pass over the 0.1.1 security work, with render-level
tests so the UI parts cannot regress silently.

### Fixed

- **Configuration refusals were effectively invisible.** Warnings — including
  "this branch shipped a config, which was ignored" and "`install` can only be
  disabled by a repo config" — rendered only while the first step had produced
  no output, so in practice nobody ever saw them. They now have their own strip
  under the header, and there are render tests asserting they survive a full
  log pane.
- **Two panes could set up the same worktree at once.** A pane that lost the
  lock race ran the pipeline anyway, so two dependency installs could land in
  one tree. It now focuses the pane that holds the lock and exits.
- **The housekeeping sweep could delete a live lock.** Lock files aged out after
  a week like any other file, so a pane left open that long lost its lock and a
  second setup could start. Locks are now decided by whether anyone holds them;
  only job records age out.
- **Any herdr event was treated as a worktree creation.** The event name is now
  checked — accepting both spellings herdr uses, the dotted `worktree.created`
  of manifests and the `worktree_created` of the JSON envelope — and anything
  else is reported and ignored. The test fixture is now a capture of the real
  envelope rather than a hand-written approximation of it.
- **The declared MSRV was wrong** — `rust-version` said 1.85 while the
  dependency graph requires 1.88, so a build from source on 1.85 failed with a
  dependency error instead of a clear message. Corrected, and CI now enforces
  it on the 1.88 toolchain.
- CI passed `toolchain` to the Rust setup action implicitly, which a SHA pin
  makes ambiguous; every job now names its toolchain.
- README showed the pre-0.1.1 shell-wrapped install line and understated the
  distinction between installs (argv, no shell) and `[[steps]]` (a shell, by
  design).

## 0.1.1

Security release. **0.1.0 contains a command injection; upgrade.** Found by an
adversarial audit of 0.1.0; every finding below was reproduced before being
fixed and has a regression test.

### Fixed — critical

- **Command injection via `package.json`.** The `packageManager` field — which
  any branch controls — was interpolated into a `/bin/sh -c` command line, so
  opening a worktree for a hostile branch ran its code. It fired even with
  `install` and `trust_repo_steps` disabled. Install commands are now built
  only from a fixed allowlist and executed as argv with no shell.
- **Writes outside the worktree.** A committed symlink at any intermediate path
  component (`packages -> /elsewhere`) redirected the copy — plaintext `.env`
  files included — to a path the branch chose. Destinations are now resolved
  component by component and refused if any component is a symlink.

### Fixed — high

- A repo config could re-enable `install`, `mise_trust`, `direnv_allow`,
  `use_mise` and `use_direnv` after the user disabled them. A repo layer may
  now only ever restrict.
- A repo config's `[env]` could set `PATH` (and `LD_PRELOAD`, `NODE_OPTIONS`,
  `BASH_ENV`, …), choosing which `npm`/`mise`/`direnv` binary ran. Those names
  are refused from repo layers, and `mise`/`direnv` are invoked by absolute
  path.
- Repo config is now read from the **source checkout**, never from the worktree
  being set up. A config that exists only on the branch is ignored and
  reported, which also stops a tracked `.herdr-worktree.local.toml` from
  shadowing your own.
- Release assets carry Sigstore build provenance; a checksum mismatch now
  aborts the install instead of silently falling back to a source build;
  downloads are pinned to https across redirects.

### Fixed — medium and lower

- `exclude` is now honoured at any depth under the macOS whole-tree clone
  (it silently stopped applying below depth 3).
- A FIFO reached the copier and blocked the pane forever; non-regular files are
  refused.
- Size caps are enforced on single-file copies and on the walk's fallback path;
  a repo may lower them but never raise them.
- A repo config can no longer add to the `symlink` list, which would have aimed
  the worktree's `node_modules` back at the main checkout.
- Steps run in their own process group with a timeout (`step_timeout_secs`,
  default 30 min), so a hung or forking step is stopped as a unit instead of
  wedging the pane and leaking children.
- The pane warns when a copied file is not gitignored on the branch, so
  `git add -A` cannot quietly commit your `.env`.
- Directories created for a copy keep the source directory's mode instead of
  the umask default.
- `git` is invoked with `--literal-pathspecs`; a dangling `.git` symlink no
  longer defeats the nested-repo check; the environment probe uses NUL-
  delimited `env -0`; job keys are collision-free and the "already running"
  lock uses `flock` instead of a recorded pid.
- Hardened CI/release: SHA-pinned actions, least-privilege `permissions`, no
  persisted credentials, `--locked` everywhere, `cargo-deny`, Dependabot.

### Added

- `step_timeout_secs` configuration key.
- `SECURITY.md` with the full trust model and a reporting channel.

## 0.1.0 — 2026-08-27 (WITHDRAWN)

Contains a command injection reachable with no user interaction, and writes
outside the worktree via a committed symlink. The release binaries were deleted
on 2026-08-28; the tag and the removed assets' digests are kept on the [release
page](https://github.com/jtnovellis/herdr-worktree-setup/releases/tag/v0.1.0)
so the withdrawal is auditable. Do not use. Original notes follow.

Initial release.

- `worktree.created` hook opens a **Worktree Setup** split in the new workspace.
- Copies gitignored dev state from the main checkout (`.env*`, `.envrc`,
  `.dev.vars`, `*.local.*`, `.vercel/`, `.claude/settings.local.json`, IDE dirs…)
  — copy-on-write where the filesystem allows it, never symlinks by default.
- Clones dependency and build caches (`node_modules`, `.venv`, `target`,
  `.turbo`, `.next/cache`, …) with APFS `clonefile` / Linux reflink: instant,
  zero extra disk, fully isolated per branch. Size-capped byte copy otherwise.
- `mise trust`, `direnv allow`, and the right dependency install
  (pnpm/bun/yarn/npm, uv/poetry/pipenv, bundle, go, mix, composer, cargo),
  run through `direnv exec` / `mise exec` when the repo uses them.
- Resolves the user's real shell environment (`$SHELL -lic`) so tools that only
  live on the rc-file PATH are found even when herdr's own PATH is minimal.
- Optional config: user `config.toml`, repo `.herdr-worktree.toml`, and an
  ignored `.herdr-worktree.local.toml` with custom `[[steps]]`.
- Actions: `worktree-setup.run` and `worktree-setup.plan` (dry run).
- Prebuilt binaries for macOS (arm64, x86_64) and Linux (x86_64, aarch64 musl).
