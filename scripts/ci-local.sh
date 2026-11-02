#!/usr/bin/env bash
# Local CI for xpm-aur. No AUR network access is required.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== fmt =="
cargo fmt --all --check

echo "== clippy =="
cargo clippy --all-targets -- -D warnings

echo "== tests =="
cargo test --workspace

echo "== release build =="
cargo build --release -p xpm-aur-cli

echo "== xpm-aur binary =="
./target/release/xpm-aur --version

echo "== xpm build adapter fixture =="
XPM_BIN="${XPM_BIN:-$ROOT/../xpm/target/release/xpm}"
if [[ ! -x "$XPM_BIN" ]]; then
    echo "skip: build ../xpm first (cargo build --release -p xpm) or set XPM_BIN" >&2
    exit 0
fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/pkg"
cat > "$TMP/pkg/PKGBUILD" <<'PKG'
pkgname=xpm-aur-local-ci-fixture
pkgver=1.0
pkgrel=1
pkgdesc="xpm-aur local CI fixture"
arch=('any')
license=('MIT')
source=()
package() {
  install -Dm644 /dev/null "$pkgdir/usr/share/xpm-aur-local-ci-fixture/marker"
}
PKG
"$XPM_BIN" build --json "$TMP/result.json" "$TMP/pkg" >/dev/null
test -s "$TMP/result.json"
grep -q 'xpm-aur-local-ci-fixture' "$TMP/result.json"
test -f "$TMP/pkg/.xpm-packages"/xpm-aur-local-ci-fixture-1.0-1-any.pkg.tar.zst
echo "local CI OK"
