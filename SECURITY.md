# Security Policy

## Supported versions

Only the latest release. Fixes ship as a new patch release; older tags are not
patched.

| Version | Supported |
|---------|-----------|
| latest  | ✅ |
| 0.1.2   | ❌ superseded — upgrade |
| 0.1.1   | ❌ superseded — upgrade |
| 0.1.0   | ❌ **withdrawn: contains a command injection.** Its binaries have been deleted from the release; the [tag](https://github.com/jtnovellis/herdr-worktree-setup/releases/tag/v0.1.0) is kept, with the removed assets' digests, so the withdrawal is auditable. See [CHANGELOG](CHANGELOG.md). |

## Reporting a vulnerability

Report privately via GitHub Security Advisories:
<https://github.com/jtnovellis/herdr-worktree-setup/security/advisories/new>

Please don't open a public issue. Expect an acknowledgement within 72 hours and
a fix or a plan within 14 days. Coordinated disclosure; credit on request.

## Trust model

Read this before installing. It is written to be honest about what is and is
not guaranteed, rather than reassuring.

### Installing

`herdr plugin install` clones this repository and runs
`scripts/fetch-or-build.sh` **as you**. That script downloads a prebuilt binary
and checks its SHA-256 against a `SHA256SUMS` file served from the same GitHub
release.

- That detects **corruption in transit**. It does **not** prove authorship:
  anyone who could publish to that release could supply a matching pair.
- Transport is pinned to `https`, including across redirects, so the download
  cannot be silently downgraded to plaintext.
- A checksum mismatch **aborts the install**. It never quietly falls back to a
  source build.
- To check provenance — which workflow, at which commit, produced a file:

  ```sh
  gh attestation verify --repo jtnovellis/herdr-worktree-setup \
    herdr-worktree-setup-<target>.tar.gz
  ```

  Releases from v0.1.1 onward carry Sigstore build provenance.
- If no prebuilt binary matches your platform, the script runs
  `cargo build --release --locked`, which executes the build scripts and proc
  macros of roughly 110 crates on your machine. That is inherent to building
  from source.

### Running

The plugin fires automatically on **every worktree creation**. That turns
`git worktree add` — an operation that executes nothing — into one that runs
code. The relevant question is always *whose* code.

**Configuration comes from your checkout, never from the branch.** Both
`.herdr-worktree.toml` and `.herdr-worktree.local.toml` are read from the
**source checkout** — the working copy you already have — and never from the
worktree being set up. A config file that exists only on the branch is ignored
and reported. This is deliberate: the branch you are reviewing does not get to
reconfigure the tool that is setting it up.

**A repository config may only ever restrict.** It can turn `install`,
`mise_trust`, `direnv_allow`, `use_mise` and `use_direnv` *off*, and lower the
size caps and the step timeout. It cannot turn any of them on, raise a cap,
change `mode`, `focus`, `placement`, `direction`, `color` or
`trust_repo_steps`, add to the `symlink` list, or set an environment variable
that decides which program runs (`PATH`, `LD_*`, `DYLD_*`, `NODE_OPTIONS`,
`BASH_ENV`, `GIT_SSH*`, `SHELL`, …). Every refusal is reported in the pane.

**What still runs branch-authored code, by design:**

- **The dependency install.** `pnpm install` and friends run the branch's
  `postinstall` scripts. Disable with `install = false`.
- **Repo-authored `[[steps]]`** from *your checkout's* `.herdr-worktree.toml`.
  Same trust level as an npm `postinstall`. Disable with
  `trust_repo_steps = false`.
- **`mise trust` / `direnv allow`** grant *persistent* execution rights: after
  them, a `mise.toml` or `.envrc` in that directory is evaluated by your shell
  on every `cd`. They only run when the corresponding config file exists, and
  each has its own switch (`mise_trust`, `direnv_allow`).

The install command itself is never assembled from repository data: the
`packageManager` field in `package.json` selects from a fixed allowlist, and
installs are executed as argv with no shell.

For reviewing untrusted branches, this is the paranoid configuration:

```toml
# $(herdr plugin config-dir worktree-setup)/config.toml
install = false
trust_repo_steps = false
mise_trust = false
direnv_allow = false
```

With all four off the plugin only copies files, and nothing from the branch is
executed.

### What crosses the boundary

Gitignored files from your main checkout are copied into the worktree. That
set includes `.env*` and `.npmrc`, which typically hold live credentials.

- Whether they stay ignored **in the worktree** is decided by the branch's
  `.gitignore`. A branch that drops `.env` from it turns a routine
  `git add -A` into a commit of your production secrets. The plugin checks this
  after copying and prints a loud warning naming every copied file the branch
  does not ignore — but the warning is only useful if you read it.
- Copies preserve their original permission bits, and directories created along
  the way are given the source directory's mode rather than the umask default.
- Writes are confined to the worktree. Every destination path is resolved
  component by component and refused if any component is a symlink, so a
  committed symlink cannot redirect a copy — secrets included — somewhere else.
  Nothing that already exists in the worktree is ever overwritten.

### Not in scope

Anything that requires an attacker who can already run code as you, or write to
your main checkout or your user config.
