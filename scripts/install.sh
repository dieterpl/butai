#!/bin/sh
#
# butai installer.
#
#   curl -fsSL https://raw.githubusercontent.com/dieterpl/butai/main/scripts/install.sh | sh
#
# Downloads the right prebuilt binary for this machine, verifies its checksum
# when the release publishes one, and drops `butai` somewhere on your PATH.
#
# Knobs (all optional):
#   BUTAI_VERSION=v0.3.0    install a specific tag instead of the latest
#   BUTAI_INSTALL_DIR=~/bin  install somewhere other than the default
#
set -eu

REPO="dieterpl/butai"
BIN="butai"

say()  { printf '%s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || err "this installer needs \`$1\` and could not find it"
}

need uname
need mkdir
need tar

if command -v curl >/dev/null 2>&1; then
    fetch()   { curl -fsSL "$1" -o "$2"; }
    fetch_so() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch()   { wget -qO "$2" "$1"; }
    fetch_so() { wget -qO- "$1"; }
else
    err "this installer needs \`curl\` or \`wget\` and found neither"
fi

# ── what are we running on? ──────────────────────────────────────────────
os=$(uname -s)
arch=$(uname -m)

case "$os" in
    Linux)  os_name=linux ;;
    Darwin) os_name=macos ;;
    MINGW*|MSYS*|CYGWIN*)
        err "Windows is not supported natively — butai's transport is Unix domain
       sockets and its client drives the tty through termios. Under WSL2 this
       installer works as normal, so run it from your WSL shell." ;;
    *) err "unsupported operating system: $os" ;;
esac

case "$arch" in
    x86_64|amd64)   arch_name=x86_64 ;;
    arm64|aarch64)  arch_name=aarch64 ;;
    armv7l|armv7)   arch_name=armv7 ;;
    *) err "unsupported architecture: $arch" ;;
esac

# Every target ships the same artifact: a tarball named for its Rust triple.
if [ "$os_name" = macos ]; then
    target="${arch_name}-apple-darwin"
elif [ "$arch_name" = armv7 ]; then
    target="armv7-unknown-linux-gnueabihf"
else
    # Prefer the static musl build wherever glibc isn't the system libc (Alpine
    # and friends), since the gnu build would fail to load there. `ldd --version`
    # writes to stderr on glibc and to stdout on musl, hence the redirect.
    if ldd --version 2>&1 | grep -qi musl; then
        target="${arch_name}-unknown-linux-musl"
    else
        target="${arch_name}-unknown-linux-gnu"
    fi
fi

# ── which version? ───────────────────────────────────────────────────────
version="${BUTAI_VERSION:-}"
if [ -z "$version" ]; then
    say "==> finding the latest release"
    version=$(fetch_so "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1)
    [ -n "$version" ] || err "could not determine the latest release — set BUTAI_VERSION=vX.Y.Z and retry"
fi
bare_version=${version#v}
info "version $version"
info "target  $target"

base="https://github.com/$REPO/releases/download/$version"

# ── download ─────────────────────────────────────────────────────────────
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t butai)
trap 'rm -rf "$tmp"' EXIT INT TERM

# What a release publishes today: `<bin>-<version>-<triple>.tar.gz`, holding the
# binary plus README and LICENSE — one shape for every target, from both
# `scripts/release.sh` and `.github/workflows/release.yml`. A bare binary named
# for its triple is the older shape, and is tried second so a tag from before
# the tarballs still installs.
#
# Which one arrived decides how it is unpacked, and that answer used to be read
# from a variable nothing ever assigned: with `set -u` the installer died right
# here, one line after printing the version and target it was about to fetch —
# so *every* run of it failed, whatever the release held.
tar_asset="${BIN}-${bare_version}-${target}.tar.gz"
bare_asset="${BIN}-${target}"

say "==> downloading $tar_asset"
if fetch "$base/$tar_asset" "$tmp/$tar_asset" 2>/dev/null; then
    asset="$tar_asset"
    asset_is_tar=1
elif fetch "$base/$bare_asset" "$tmp/$bare_asset" 2>/dev/null; then
    info "no tarball for $target; took the bare binary"
    asset="$bare_asset"
    asset_is_tar=0
else
    err "download failed: $base/$tar_asset (does that release have an asset for $target?)"
fi

# ── verify, when the release publishes checksums ─────────────────────────
if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null; then
    if command -v sha256sum >/dev/null 2>&1; then
        sum=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
    elif command -v shasum >/dev/null 2>&1; then
        sum=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
    else
        sum=""
    fi

    if [ -n "$sum" ]; then
        want=$(grep -F "$asset" "$tmp/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 | head -n 1)
        if [ -z "$want" ]; then
            info "no checksum published for $asset — skipping verification"
        elif [ "$sum" = "$want" ]; then
            info "checksum ok"
        else
            err "checksum mismatch for $asset
  expected $want
  got      $sum
This is worth investigating rather than working around."
        fi
    else
        info "no sha256 tool available — skipping verification"
    fi
else
    info "release publishes no SHA256SUMS — skipping verification"
fi

# ── unpack ───────────────────────────────────────────────────────────────
if [ "$asset_is_tar" -eq 1 ]; then
    tar -xzf "$tmp/$asset" -C "$tmp"
    src=$(find "$tmp" -type f -name "$BIN" -perm -u+x 2>/dev/null | head -n 1)
    [ -n "$src" ] || err "no \`$BIN\` executable inside $asset"
else
    src="$tmp/$asset"
fi
chmod +x "$src"

# ── install ──────────────────────────────────────────────────────────────
if [ -n "${BUTAI_INSTALL_DIR:-}" ]; then
    dest_dir="$BUTAI_INSTALL_DIR"
elif [ -w /usr/local/bin ] 2>/dev/null; then
    dest_dir=/usr/local/bin
else
    dest_dir="$HOME/.local/bin"
fi

mkdir -p "$dest_dir"
mv "$src" "$dest_dir/$BIN"

say ""
say "installed $BIN $bare_version -> $dest_dir/$BIN"

case ":$PATH:" in
    *":$dest_dir:"*) ;;
    *)
        say ""
        say "$dest_dir is not on your PATH. Add this to your shell profile:"
        say ""
        say "    export PATH=\"$dest_dir:\$PATH\""
        ;;
esac

say ""
say "Then, from any project directory:"
say ""
say "    butai"
say ""
