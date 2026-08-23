#!/bin/sh
#
# butai installer.
#
#   curl -fsSL https://raw.githubusercontent.com/dieterpl/butai/main/scripts/install.sh | sh
#
# Downloads the right prebuilt binary for this machine, verifies its checksum
# when the release publishes one, and drops `butai` somewhere on your PATH.
#
# It is also how you *upgrade* one, so it finishes the job: installing a binary
# does not restart anything, and the daemon already running is the one that was
# started by the old build. Left alone that is a new client talking to an old
# daemon — the skew butai reports in its footer. So when this replaces an
# existing install it stops the daemon too, which is not destructive: the open
# workspaces and every pane's output are snapshotted first and restored by the
# build that comes up next.
#
# From inside butai there is nothing to install *into* safely — stopping the
# daemon would close the workbench you are reading this in — so there it says so
# and leaves it to you. `butai update` is the one that can do it from in there,
# because it restarts itself afterwards.
#
# Knobs (all optional):
#   BUTAI_VERSION=v0.3.0    install a specific tag instead of the latest
#   BUTAI_INSTALL_DIR=~/bin  install somewhere other than the default
#   BUTAI_NO_RESTART=1       install the binary and leave the daemon alone
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
# Was there one here already? Asked before the move, because afterwards there
# always is — and it decides whether this is an install or an upgrade.
had_one=0
[ -e "$dest_dir/$BIN" ] && had_one=1
mv "$src" "$dest_dir/$BIN"

say ""
if [ "$had_one" -eq 1 ]; then
    say "updated $BIN -> $bare_version in $dest_dir"
else
    say "installed $BIN $bare_version -> $dest_dir/$BIN"
fi

# ── hand the daemon over to the new build ────────────────────────────────
#
# The socket file is the test rather than `butai ls`, which would *start* a
# daemon to answer: every command that talks to one connects-or-spawns. A stale
# socket left by a crash is harmless here — `kill-server` cleans it up.
sock="${BUTAI_SOCKET:-$HOME/.butai/butai.sock}"
if [ "$had_one" -eq 1 ] && [ -S "$sock" ] && [ -z "${BUTAI_NO_RESTART:-}" ]; then
    if [ -n "${BUTAI:-}" ]; then
        # Inside a pane of the very daemon we would be stopping.
        say ""
        say "the running daemon is still the old build, and you are inside it."
        say "when you are ready, from outside butai:"
        say ""
        say "    butai update          # or: butai kill-server, then butai"
    elif "$dest_dir/$BIN" kill-server >/dev/null 2>&1; then
        say "stopped the old daemon — your workspaces are saved and come back"
    else
        say ""
        say "could not stop the running daemon; it is still the old build."
        say "run this when convenient (your workspaces are kept):"
        say ""
        say "    butai kill-server"
    fi
fi

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
say "You only need this script once: butai asks before updating itself from"
say "here on, and \`butai update\` does it on demand."
say ""
