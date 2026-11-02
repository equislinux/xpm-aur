# xpm-aur

A Rust AUR helper for X Linux. It queries the AUR, resolves dependencies,
builds source packages with `makepkg`, and performs every package transaction
through **xpm** instead of pacman.

`xpm-aur` is a companion to xpm, not a replacement for pacman. `makepkg` still
comes from the pacman package, and builds still execute the PKGBUILD as bash.
What changes is the transaction backend: xpm owns installs, removals and the
package database, and AUR metadata lives in a small, explicit sidecar.

> Status: MVP. The common daily workflows are implemented and tested. Native
> clean-chroot builds, AUR signature/attestation policy and a public binary
> cache are future work.

## Features

- Combined official + AUR search and package information (`--aur`/`--repo` to restrict).
- Recursive dependency resolution across official repositories and AUR.
- `provides`, version constraints, conflicts, replacements and split packages.
- Build order for AUR package bases, with build dependencies installed first.
- Builds through `xpm build`; xpm installs the resulting packages.
- Installs, removes and upgrades through xpm.
- Tracks `origin=aur`, AUR version and git commit in a sidecar state file.
- `--devel` rebuilds VCS packages when HEAD changes.
- `adopt` imports AUR packages installed by yay or another helper.
- `getpkgbuild` clones/updates AUR PKGBUILDs for inspection.
- Foreign/installed queries (`-Qm`) and `--nocheck`.
- yay/pacman-style command forms: `-S`, `-Syu`, `-Ss`, `-Si`, `-Rns`, `-Qm`, `-Sc`.

## Install

From source:

```bash
cargo build --release
sudo install -Dm755 target/release/xpm-aur /usr/local/bin/xpm-aur
```

From the X repository:

```bash
xpm install xpm-aur
```

Runtime requirements: `xpm >= 0.1.0-5`, `pacman` (for `makepkg`), `git`, `base-devel`,
and a normal user for building. `xpm-aur` must not be run as root; it invokes
the configured sudo command for xpm transactions.

## Usage

```bash
xpm-aur search yay
xpm-aur info yay

xpm-aur install yay
xpm-aur install --print yay          # resolve and print the plan only
xpm-aur install --aur --rebuild foo-git
xpm-aur install --remove-conflicts foo-git

xpm-aur upgrade                      # xpm upgrade + AUR updates
xpm-aur upgrade --aur --devel        # AUR only, VCS HEAD checks

xpm-aur remove foo
xpm-aur remove -s foo                # recursive
xpm-aur remove -n foo                # purge config files

xpm-aur query
xpm-aur query --quiet
xpm-aur adopt                        # import yay-installed AUR packages
xpm-aur clean --all
```

Compatibility forms are accepted as well:

```bash
xpm-aur -S foo
xpm-aur -Syu --noconfirm
xpm-aur -Ss yay
xpm-aur -Si yay
xpm-aur -Rns foo
xpm-aur -Qm
xpm-aur -Sc
```

## How it works

1. `xpm sync` refreshes the sync databases.
2. The resolver reads installed packages from pacman's local database (where
   xpm mirrors its transactions) and official metadata from xpm's sync DB.
3. Official packages are installed by xpm as one batch.
4. AUR package bases are cloned/updated, built in dependency order with
   `xpm build`, and the resulting package files are installed with `xpm install`.
5. `xpm-aur` records the AUR base, commit, version and split outputs in
   `$XDG_STATE_HOME/xpm-aur/state.json`.

Build and package caches live in `$XDG_CACHE_HOME/xpm-aur/`:

```text
build/       AUR git checkouts and makepkg work trees
packages/    built .pkg.tar.zst files
sources/     makepkg source cache
```

## Configuration

Optional `~/.config/xpm-aur/config.toml`:

```toml
[aur]
rpc_url = "https://aur.archlinux.org/rpc/v5"
git_url = "https://aur.archlinux.org"

[commands]
xpm = "xpm"
sudo = "sudo"          # empty disables sudo

[build]
run_checks = true
extra_args = []

[paths]
build = "~/.cache/xpm-aur/build"
packages = "~/.cache/xpm-aur/packages"
sources = "~/.cache/xpm-aur/sources"
cache = "~/.cache/xpm-aur"
state = "~/.local/state/xpm-aur/state.json"
```

Command-line flags override the file; `XPM_AUR_CONFIG`, `XPM_AUR_STATE` and
`XPM_AUR_CACHE` are also honoured.

## Safety

- AUR PKGBUILDs are untrusted executable bash. Review them before building.
  `xpm-aur` does not sandbox builds yet; use a clean chroot when that lands.
- `xpm-aur` never runs `makepkg` directly, never builds as root, and never calls
  `pacman` to install.
- `xpm-aur` does not sign or publish built packages. A local signed binary
  repository is planned.
- Do not run `xpm-aur` and yay/pacman concurrently. xpm locks its own
  transaction and takes pacman's `db.lck` while mirroring, but the two tools do
  not share a lock for the whole operation.

## Differences from yay

- Transactions go through xpm; yay uses libalpm.
- `xpm-aur` requires `makepkg` but does not use pacman for install/remove.
- The state sidecar is explicit and readable; yay relies on the pacman DB.
- The compatibility layer covers common forms, not every yay flag. Native
  `xpm-aur` subcommands are the source of truth.

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The workspace is split into a pure core and adapters; see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Command compatibility is
documented in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md).
