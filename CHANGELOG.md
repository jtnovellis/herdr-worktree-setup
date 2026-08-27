# Changelog

## 0.1.0 — 2026-08-27

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
