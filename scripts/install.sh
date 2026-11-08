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
#   BUTAI_VERSION=v0.3.0     install a specific tag instead of the latest
#   BUTAI_CHANNEL=dev        take the newest prerelease off the dev track
#   BUTAI_INSTALL_DIR=~/bin  install somewhere other than the default
#   BUTAI_NO_RESTART=1       install the binary and leave the daemon alone
#
# The two channels are two sets of tags. A version with a `-` in it — `1.3.0-dev.1`
# — is cut from `develop` and published as a GitHub prerelease, which is kept out
# of `releases/latest`; a bare one is cut from `main`. That is why a stable
# install never sees a dev build, and why reaching one has to be asked for.
#
# A dev install is normally a *second* butai rather than a replacement:
#
#   BUTAI_CHANNEL=dev BUTAI_INSTALL_DIR=~/.butai-dev/bin BUTAI_NO_RESTART=1 sh install.sh
#   BUTAI_HOME=~/.butai-dev ~/.butai-dev/bin/butai
#
# `BUTAI_HOME` gives it its own socket, session store, pane dumps and logs, so
# the stable butai beside it keeps running and the two never meet.
#
set -eu

REPO="dieterpl/butai"
BIN="butai"
# The same releases on GitHub Pages; see `scripts/mirror-site.sh`.
MIRROR="https://dieterpl.github.io/butai"

say()  { printf '%s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || err "this installer needs \`$1\` and could not find it"
}

need uname
need mkdir
need tar

# A request that fails is tried again. GitHub's anonymous endpoints — the only
# kind an installer has — spent November 2026 answering most requests with a
# 504, often after eleven seconds, and a few without, so one refusal says
# little. `fetch` takes how many tries a source is worth (default three) and
# `fetch_so` tries twice. curl retries only what is worth retrying (timeouts,
# 408, 429, 5xx) and fails a 404 at once; busybox's wget has no such switch, so
# it gets a plain loop. With curl nothing waits forever: a stalled request is
# dropped and the next source asked. wget keeps its own defaults, because `-T`
# crashes the busybox 1.30 that Ubuntu ships.
#
# A download also asks for `application/octet-stream`. The browser links and
# the mirror ignore it; the API's per-asset link needs it to send the file
# rather than a description of it.
if command -v curl >/dev/null 2>&1; then
    fetch() {
        curl -fsSL --connect-timeout 15 --max-time 300 --retry $((${3:-3} - 1)) --retry-delay 1 \
            -H 'Accept: application/octet-stream' "$1" -o "$2"
    }
    fetch_so() { curl -fsSL --connect-timeout 15 --max-time 60 --retry 1 --retry-delay 1 "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() {
        _try=1
        while [ "$_try" -le "${3:-3}" ]; do
            wget -q -O "$2" --header='Accept: application/octet-stream' "$1" && return 0
            [ "$_try" -lt "${3:-3}" ] && sleep "$_try"
            _try=$((_try + 1))
        done
        return 1
    }
    fetch_so() {
        wget -q -O- "$1" && return 0
        sleep 1
        wget -q -O- "$1"
    }
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
channel="${BUTAI_CHANNEL:-stable}"
case "$channel" in
    stable|dev) ;;
    *) err "BUTAI_CHANNEL is \`stable\` or \`dev\`, not \`$channel\`" ;;
esac

version="${BUTAI_VERSION:-}"
if [ -n "$version" ]; then
    :
elif [ "$channel" = dev ]; then
    say "==> finding the latest dev release"
    # `releases/latest` is exactly what the dev track is not, so this reads the
    # list and picks out of it.
    #
    # **Whitespace goes first, and that is the whole trick.** The API answers
    # pretty-printed — one field per line — so a `grep` for `"prerelease": true`
    # matches a line holding nothing else, and the tag it belongs to is fifteen
    # lines above it. Flattening first puts every field of a release back on one
    # line, and *then* splitting on `{` gives one chunk per release object, with
    # `tag_name`, `draft` and `prerelease` together in it: the object's fields
    # run in that order with no brace between them, so no chunk boundary can
    # fall between the tag and the flags that describe it. It also survives the
    # compact shape, since flattening one is a no-op.
    #
    # Newest *published*, not highest version — the list is in publish order,
    # and comparing versions is not something to write in `sed`. A stable patch
    # cut after a dev tag is the case where the two differ, and `BUTAI_VERSION`
    # is the way to say exactly which one you meant. The installed butai then
    # keeps itself current with the real comparison; see `crates/butai-update`.
    #
    # When the API does not answer at all, the mirror names its newest dev tag.
    version=$( (fetch_so "https://api.github.com/repos/$REPO/releases?per_page=30" 2>/dev/null || true) \
        | tr -d ' \t\n' \
        | tr '{' '\n' \
        | grep '"prerelease":true' \
        | grep -v '"draft":true' \
        | sed -n 's/.*"tag_name":"\([^"]*\)".*/\1/p' \
        | head -n 1)
    [ -n "$version" ] || version=$(fetch_so "$MIRROR/latest-dev" 2>/dev/null | head -n 1 || true)
    [ -n "$version" ] || err "could not find a dev release on GitHub or its mirror — set BUTAI_VERSION=vX.Y.Z-dev.N and retry"
else
    say "==> finding the latest release"
    version=$( (fetch_so "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null || true) \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1)
    [ -n "$version" ] || version=$(fetch_so "$MIRROR/latest" 2>/dev/null | head -n 1 || true)
    [ -n "$version" ] || err "could not determine the latest release — set BUTAI_VERSION=vX.Y.Z and retry"
fi
# Read from the network and about to be used in URLs and paths, so it must look
# like a tag.
case "$version" in
    *[!A-Za-z0-9._-]*|'') err "\`$version\` does not look like a release tag" ;;
esac
bare_version=${version#v}
info "version $version"
info "target  $target"

base="https://github.com/$REPO/releases/download/$version"

# ── download ─────────────────────────────────────────────────────────────
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t butai)
trap 'rm -rf "$tmp"' EXIT INT TERM

# GitHub's description of this release, one JSON object per line — the
# flattening the dev lookup above explains. It holds what the browser link
# does not: each file's API link and the sha256 GitHub recorded for it. Missing
# when the API is down, and everything below copes with that.
release=$( (fetch_so "https://api.github.com/repos/$REPO/releases/tags/$version" 2>/dev/null || true) \
    | tr -d ' \t\n' | tr '{' '\n')

# An asset's own object is cut in two by the `uploader` object inside it: the
# first part holds its name and API link, the second its digest and browser
# link. So each is looked up by what shares its part.
api_url() {
    printf '%s\n' "$release" | grep -F "\"name\":\"$1\"" \
        | sed -n 's|.*"url":"\(https://api\.github\.com/repos/[^"]*/releases/assets/[0-9]*\)".*|\1|p' \
        | head -n 1
}
recorded_sum() {
    printf '%s\n' "$release" | grep -F "/releases/download/$version/$1\"" \
        | sed -n 's/.*"digest":"sha256:\([0-9a-f]\{64\}\)".*/\1/p' \
        | head -n 1
}

# Does the release publish this file? Yes when the API did not answer, since
# then there is no list to say otherwise.
has_asset() {
    [ -z "$release" ] || printf '%s\n' "$release" | grep -qF "\"name\":\"$1\""
}

# One file of the release, from wherever answers first: GitHub's link, then
# the mirror, then the API's link — the order they kept working in when the
# first one stopped. GitHub's link gets two tries rather than three, because the
# mirror behind it is the one that kept answering.
get_asset() {
    has_asset "$1" || return 1
    if fetch "$base/$1" "$2" 2 2>/dev/null; then return 0; fi
    if fetch "$MIRROR/$version/$1" "$2" 2>/dev/null; then
        info "GitHub's link failed; took $1 from the mirror"
        return 0
    fi
    _api=$(api_url "$1")
    if [ -n "$_api" ] && fetch "$_api" "$2" 2 2>/dev/null; then
        info "GitHub's link failed; took $1 through the API"
        return 0
    fi
    rm -f "$2"
    return 1
}

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
if get_asset "$tar_asset" "$tmp/$tar_asset"; then
    asset="$tar_asset"
    asset_is_tar=1
elif get_asset "$bare_asset" "$tmp/$bare_asset"; then
    info "no tarball for $target; took the bare binary"
    asset="$bare_asset"
    asset_is_tar=0
else
    err "download failed: $tar_asset from GitHub, its API and $MIRROR (does $version have an asset for $target?)"
fi

# ── verify ───────────────────────────────────────────────────────────────
#
# Against the sha256 GitHub recorded when the file was uploaded, when the API
# answered: that holds wherever the bytes came from, the mirror included. Else
# against the release's SHA256SUMS, which comes from the same place the file did.
if command -v sha256sum >/dev/null 2>&1; then
    sum=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    sum=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
else
    sum=""
fi

want=$(recorded_sum "$asset")
sum_from="GitHub's record"
if [ -z "$want" ] && get_asset SHA256SUMS "$tmp/SHA256SUMS"; then
    want=$(grep -F "$asset" "$tmp/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 | head -n 1)
    sum_from="SHA256SUMS"
fi

if [ -z "$sum" ]; then
    info "no sha256 tool available — skipping verification"
elif [ -z "$want" ]; then
    info "no checksum published for $asset — skipping verification"
elif [ "$sum" = "$want" ]; then
    info "checksum ok ($sum_from)"
else
    err "checksum mismatch for $asset
  expected $want ($sum_from)
  got      $sum
This is worth investigating rather than working around."
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
# Which daemon this install replaces, in the order `crates/butai-protocol`'s
# `paths.rs` resolves it: `BUTAI_HOME` moves a whole butai and takes its socket
# along, `BUTAI_SOCKET` moves that one socket, and neither is `~/.butai`. This
# used to read only `BUTAI_SOCKET`, so installing a second butai with
# `BUTAI_HOME=~/.butai-dev` stopped the *real* daemon on the way past.
if [ -n "${BUTAI_HOME:-}" ]; then
    sock="$BUTAI_HOME/butai.sock"
else
    sock="${BUTAI_SOCKET:-$HOME/.butai/butai.sock}"
fi
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

if [ "$channel" = dev ]; then
    # Which config, and the answer is not this shell's `BUTAI_HOME`. A dev build
    # is usually installed to a directory of its own and *launched* with one, so
    # the file that governs it is the one belonging to the butai it will be —
    # naming `~/.butai` here would put the everyday install on the dev track,
    # which is the opposite of a second butai.
    say ""
    say "this is a dev build. To keep it on that track — prereleases as well as"
    say "stable ones — put this in the config of the butai it runs as:"
    say ""
    say "    [update]"
    say "    channel = \"dev\""
    say ""
    if [ -n "${BUTAI_HOME:-}" ]; then
        say "which is $BUTAI_HOME/config.toml, from the BUTAI_HOME set here."
    else
        say "\$BUTAI_HOME/config.toml when you launch it with BUTAI_HOME set — a"
        say "second butai beside your own — or $HOME/.butai/config.toml when this"
        say "*is* your butai. SETTINGS → ABOUT → release channel writes it either way."
    fi
    say ""
    say "Without it, this build follows the stable track and sits quiet until"
    say "stable overtakes it."
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
