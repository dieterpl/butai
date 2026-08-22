#!/usr/bin/env bash
#
# Build release tarballs of `butai` for every supported target.
#
# One code path per target, and one artifact shape: a `.tar.gz` holding the
# binary plus README and LICENSE. There is no per-platform special case —
# macOS is packaged exactly like Linux, because this repository ships a
# binary, not an app bundle.
#
# Builder selection is automatic:
#   * the host triple                  -> native `cargo build`
#   * another Apple target on a Mac    -> native `cargo build` (both darwin
#                                         arches link on either Mac)
#   * anything else                    -> `cross` (needs Docker)
#
# The macOS binaries are ad-hoc signed by the linker as a side effect of
# building on a Mac. That is not cosmetic: arm64 macOS refuses to exec an
# unsigned binary ("Killed: 9"). Building darwin targets anywhere other than a
# Mac means signing them yourself.
#
# Requirements: rustup + stable on PATH. For any target needing `cross`,
# a running Docker daemon and `cargo install cross --locked`.
#
# Usage:
#   scripts/release.sh                                       # every target
#   TARGETS="x86_64-unknown-linux-musl" scripts/release.sh   # a subset
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BIN=butai
DIST="$REPO_ROOT/dist"
# Respect CARGO_TARGET_DIR — this tree is often built with it pointed off-repo,
# and hardcoding `target/` would look for the binary where cargo never put it.
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"

# The full matrix. Keep this list in sync with the one in
# .github/workflows/release.yml and the install table in README.md.
ALL_TARGETS="
x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu
x86_64-unknown-linux-musl
aarch64-unknown-linux-musl
armv7-unknown-linux-gnueabihf
aarch64-apple-darwin
x86_64-apple-darwin
"
TARGETS="${TARGETS:-$ALL_TARGETS}"

die() { echo "error: $*" >&2; exit 1; }

# Version from the workspace Cargo.toml [workspace.package] section.
VERSION="$(cargo metadata --no-deps --format-version 1 \
  | grep -o '"name":"butai","version":"[^"]*"' \
  | head -n1 | sed 's/.*"version":"\([^"]*\)".*/\1/')"
[ -n "${VERSION:-}" ] || die "could not determine version from cargo metadata"

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"

echo ">> butai release v$VERSION"
echo ">> host:    $HOST_TRIPLE"
echo ">> targets:" $TARGETS

rm -rf "$DIST"
mkdir -p "$DIST"

# Does this target build natively on this host, or does it need `cross`?
needs_cross() {
  local target="$1"
  [ "$target" = "$HOST_TRIPLE" ] && return 1
  # Either Mac can link both Apple arches; cross has no darwin images at all.
  case "$HOST_TRIPLE:$target" in
    *-apple-darwin:*-apple-darwin) return 1 ;;
  esac
  return 0
}

built=0
for target in $TARGETS; do
  echo
  echo "==> building $BIN for $target"

  if needs_cross "$target"; then
    case "$target" in
      *-apple-darwin)
        die "$target must be built on a Mac — cross has no darwin image, and an
       unsigned arm64 binary will not exec. Run this script on macOS, or let
       .github/workflows/release.yml build it on a macos runner."
        ;;
    esac
    command -v cross >/dev/null 2>&1 \
      || die "'$target' needs cross-compilation but 'cross' is not installed.
       install it with: cargo install cross --locked (and start Docker)"
    builder=(cross)
  else
    rustup target add "$target" >/dev/null 2>&1 || true
    builder=(cargo)
  fi

  "${builder[@]}" build --release --target "$target" -p "$BIN"

  bin_path="$TARGET_DIR/$target/release/$BIN"
  [ -f "$bin_path" ] || die "expected binary not found at $bin_path"

  # Stage: binary + docs, then tar it up.
  stage="$DIST/$BIN-$VERSION-$target"
  mkdir -p "$stage"
  cp "$bin_path" "$stage/"
  cp README.md LICENSE "$stage/"

  tarball="$BIN-$VERSION-$target.tar.gz"
  tar -C "$DIST" -czf "$DIST/$tarball" "$(basename "$stage")"
  rm -rf "$stage"
  echo "    packaged dist/$tarball"
  built=$((built + 1))
done

[ "$built" -gt 0 ] || die "no targets built"

# Checksums over every tarball, with the same tool name CI uses.
if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$DIST" && sha256sum ./*.tar.gz | sed 's| \./| |' > SHA256SUMS )
else
  ( cd "$DIST" && shasum -a 256 ./*.tar.gz | sed 's| \./| |' > SHA256SUMS )
fi

echo
echo ">> done — $built target(s). Artifacts in dist/:"
ls -1 "$DIST"
