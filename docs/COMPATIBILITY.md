# Compatibility

Native subcommands are canonical. The compatibility layer rewrites common yay
forms before clap parses them.

| yay/pacman | xpm-aur | Notes |
| --- | --- | --- |
| `-S pkg` | `install pkg` | official preferred, AUR fallback |
| `-Syu` | `upgrade` | xpm upgrade first, then AUR |
| `-Sy` | `sync` | xpm sync only |
| `-Ss query` | `search query` | AUR RPC |
| `-Si pkg` | `info pkg` | AUR RPC |
| `-Sp pkg` | `install --print pkg` | plan only |
| `-R pkg` | `remove pkg` | through xpm |
| `-Rns pkg` | `remove --nosave --recursive pkg` | |
| `-Q` / `-Qq` | `query` / `query --quiet` | tracked AUR packages |
| `-Qm` | `query` | tracked AUR packages |
| `-Sc` / `-Scc` | `clean` / `clean --all` | cache only |
| `-V` | `--version` | |

Supported long options: `--noconfirm`, `--needed`, `--asdeps`, `--devel`,
`--aur`, `--repo`, `--rebuild`, `--print`, `--remove-conflicts`, `--config`,
`--build-dir`, `--cache-dir`, `--state`.

Not implemented: `-F` (files DB), `-Sw` (download only), `-Sc` package cache
signatures, `--answerclean`, `--editmenu`, `--diffmenu`, `--sudoloop`, and other
yay-specific UX flags. Unknown forms are passed to clap so they fail loudly.
