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
| `xpm-aur-build` | `xpm build` invocation, JSON artifact discovery | resolution, AUR RPC, xpm internals |
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
6. topologically sorts bases so dependencies build first and records each
   base's direct `depends_on` edges in the plan;
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
  → xpm install runtime official dependencies (make/check-only deps stay
    inside the chroot)
  → xpm install official explicit targets
  → scheduler over AUR bases:
       build ready bases concurrently (up to the job limit):
         git checkout
         validate .SRCINFO
         xpm build (shared base-devel template; xpm installs the base's
         official and prebuilt AUR dependencies in the disposable root,
         then makepkg --nodeps --cleanbuild)
       as each build finishes, on the calling thread:
         xpm install split dependencies
         xpm install split explicit packages
         update sidecar state
```

Only `xpm-aur` decides the order. xpm owns the makepkg invocation; xpm-aur
handles the AUR/build plan and passes the checkout to `xpm build`. A base is
ready only after every base in its `depends_on` list is built and installed.
Builds may overlap; xpm installs and sidecar updates are serialized.

## Databases

- Installed packages: xpm's local DB (`/var/lib/xpm/local`) is the native
  source of truth. Pacman's local DB is merged only with
  `--compat-pacman-db` / `[database] compat_pacman = true`.
- Official sync metadata: xpm's sync DB (`/var/lib/xpm/sync`). Pacman's sync
  DB is a fallback only in explicit compatibility mode; `xpm sync` refreshes
  xpm's own database.
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
- DB tests build real tar archives (gzip/zstd) and parse them; pacman merge tests are explicit compatibility cases.
- CLI tests cover yay argv rewriting.
- End-to-end manual check: build `yay` from AUR with a fake xpm binary; verify
  artifact creation and sidecar contents without touching the host package DB.
