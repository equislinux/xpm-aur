# Architecture

`xpm-aur` is a layered Cargo workspace. The dependency direction is always
inward: adapters know about the core, the core knows about nothing.

```text
xpm-aur-cli
    │  parsing, config, formatting
    ▼
xpm-aur-app
    │  use cases: install, upgrade, remove, adopt, clean
    ├──────────────┬──────────────┬──────────────┐
    ▼              ▼              ▼              ▼
xpm-aur-core   xpm-aur-aur   xpm-aur-build  xpm-aur-system
(domain)       (AUR)         (makepkg)      (host adapters)
```

## Crates

| Crate | Responsibility | Must not know about |
| --- | --- | --- |
| `xpm-aur-core` | versions, dependencies, candidates, resolver, plan, process port | files, network, commands |
| `xpm-aur-aur` | AUR RPC v5, AUR git, `.SRCINFO` | pacman, xpm, local state |
| `xpm-aur-build` | `makepkg` invocation, artifact discovery | resolution, AUR RPC, xpm |
| `xpm-aur-system` | pacman/xpm DBs, xpm CLI, state sidecar, process runner | resolution policy |
| `xpm-aur-app` | orchestration and transaction ordering | protocol/file formats |
| `xpm-aur-cli` | clap, yay argv compatibility, config, output | business rules |

## Resolver

`xpm-aur-core::Resolver` talks only to the `PackageCatalog` port:

```rust
trait PackageCatalog {
    fn installed(&self) -> Result<Vec<Candidate>>;
    fn official_candidates(&self, dep: &Dependency) -> Result<Vec<Candidate>>;
    fn aur_candidates(&self, dep: &Dependency) -> Result<Vec<Candidate>>;
}
```

The resolver:

1. marks installed packages that satisfy a dependency;
2. prefers official candidates over AUR candidates, matching yay;
3. prefers exact names over `provides`;
4. recursively resolves AUR runtime, make and check dependencies;
5. groups split packages by package base;
6. topologically sorts bases so dependencies build first;
7. detects conflicts/replacements and optionally plans removals;
8. emits an immutable `InstallPlan`.

Official dependencies are not traversed: xpm solves them in one transaction.
This is deliberate; duplicating xpm's dependency resolver in xpm-aur would create
two sources of truth.

## Transaction order

```text
xpm sync
  → resolve plan
  → remove conflicts/replacements (optional)
  → xpm install official dependencies
  → xpm install official explicit targets
  → for each AUR base in build order:
       git checkout
       validate .SRCINFO
       makepkg --nodeps --cleanbuild
       xpm install split dependencies
       xpm install split explicit packages
       update sidecar state
```

Only `xpm-aur` decides the order. The adapters perform exactly one operation per
call, which keeps failures attributable.

## Databases

- Installed packages: pacman local DB, because xpm mirrors every transaction
  there. This is the shared source of truth for both tools.
- Official sync metadata: xpm's sync DB (`/var/lib/xpm/sync`) when present,
  falling back to pacman's sync DB. This matters because `xpm sync` refreshes
  its own database, not pacman's.
- Sync DBs are read as gzip, xz, zstd or plain tar.
- AUR metadata: RPC v5 for resolution; `.SRCINFO` in the checkout for
  validation and split-package names.
- AUR history: `$XDG_STATE_HOME/xpm-aur/state.json`, lock-protected and written
  atomically. It is a sidecar, not a replacement for the package DB.

## Failure model

- AUR, DB, build, process and state failures are typed errors in their crate.
- `xpm-aur-app` maps them to `AppError`; only the CLI formats them.
- A failed build/install leaves the previous sidecar state intact; the next run
  can retry. Installing a base updates the sidecar only after xpm succeeds.
- Non-zero exits from xpm/makepkg/git become errors; stdout parsing is limited
  to stable formats.

## Testing

- Core tests use fake catalogs and cover versions, dependencies, providers,
  constraints, split builds, cycles and conflicts.
- AUR tests use JSON fixtures for RPC records and sample `.SRCINFO`.
- Pacman DB tests build real tar archives (gzip/zstd) and parse them.
- CLI tests cover yay argv rewriting.
- End-to-end manual check: build `yay` from AUR with a fake xpm binary; verify
  artifact creation and sidecar contents without touching the host package DB.
