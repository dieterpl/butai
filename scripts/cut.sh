#!/usr/bin/env bash
#
# Set the workspace version, ready to be committed and tagged.
#
# The version lives in four places in the root `Cargo.toml` — `[workspace.package]
# version`, and the three `butai-{protocol,update,server,client}` pins under
# `[workspace.dependencies]`, which carry a `version` as well as a `path` so
# `cargo publish` has something to rewrite. Four strings that must agree, edited
# by hand, is how a release goes out with a crate still pinned to the last one.
#
# It stops at the edit. Committing and tagging are yours, because they are the
# two steps that are hard to take back:
#
#   scripts/cut.sh 1.3.0-dev.1     # on develop — a prerelease
#   scripts/cut.sh 1.3.0           # on main    — a stable release
#
# The `-dev.N` suffix is what `.github/workflows/release.yml` reads to decide
# which track a tag belongs to, and what keeps a dev build out of every stable
# install's update check. See `docs/development.md`.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
note() { printf '\033[36m==>\033[0m %s\n' "$*"; }

[ $# -eq 1 ] || die "usage: scripts/cut.sh <version>   (e.g. 1.3.0, or 1.3.0-dev.1)"
version="$1"

# No leading `v`. The tag carries one and the Cargo version does not, and the
# two being confusable is exactly why `install.sh` and the updater both strip it
# on the way in. Rejecting it here is friendlier than writing `v1.3.0` into a
# manifest cargo will refuse to parse.
case "$version" in
    v*) die "give the bare version, not the tag: ${version#v} rather than $version" ;;
esac

# Semver, with an optional prerelease. `cargo` would catch a malformed one, but
# only after every file was already rewritten.
if ! printf '%s' "$version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    die "'$version' is not a semver version (X.Y.Z, optionally -prerelease)"
fi

case "$version" in
    *-*) track="dev prerelease"; expect_branch="develop" ;;
    *)   track="stable release"; expect_branch="main" ;;
esac

current="$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
[ -n "$current" ] || die "could not read the current version from Cargo.toml"
[ "$current" != "$version" ] || die "Cargo.toml is already at $version"

branch="$(git rev-parse --abbrev-ref HEAD)"
note "$current -> $version  ($track)"
if [ "$branch" != "$expect_branch" ]; then
    # A warning and not a refusal: cutting from a branch is a thing people do
    # deliberately, and the tag is what actually decides the track.
    printf '\033[33mnote:\033[0m a %s is normally cut on \033[1m%s\033[0m; you are on \033[1m%s\033[0m\n' \
        "$track" "$expect_branch" "$branch"
fi

# The `[workspace.package]` version is the first bare `version = ` in the file;
# the four `butai-* = { path = ..., version = "..." }` lines are matched on the
# crate name so nothing else in the manifest is touched. Both are anchored, so
# a dependency that happens to be on `$current` too is left alone.
python3 - "$version" <<'PY'
import re, sys, pathlib

version = sys.argv[1]
path = pathlib.Path("Cargo.toml")
text = path.read_text()

text, package = re.subn(
    r'(?m)^(version = )"[^"]*"', rf'\g<1>"{version}"', text, count=1
)
text, pins = re.subn(
    r'(?m)^(butai-(?:protocol|update|server|client) = \{ path = "[^"]*", version = )"[^"]*"',
    rf'\g<1>"{version}"',
    text,
)

if package != 1:
    sys.exit("error: did not find [workspace.package] version in Cargo.toml")
if pins != 4:
    sys.exit(f"error: rewrote {pins} of the 4 workspace-dependency pins")

path.write_text(text)
print(f"    Cargo.toml: 1 package version + {pins} dependency pins")
PY

note "refreshing Cargo.lock"
cargo check --quiet --workspace 2>&1 | sed 's/^/    /' || die "cargo check failed — the manifest is not consistent"

changed="$(git diff --name-only)"
note "changed:"
printf '%s\n' "$changed" | sed 's/^/    /'

cat <<EOF

Next, once CHANGELOG.md says what this is:

    git commit -am "Cut $version"
    git tag -a v$version -m "v$version"
    git push origin $branch --follow-tags

EOF
