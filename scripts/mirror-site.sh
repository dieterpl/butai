#!/usr/bin/env bash
#
# Build the release mirror that GitHub Pages serves at
# https://dieterpl.github.io/butai/ — a second home for the release files.
#
# Why there is one: in November 2026 GitHub's anonymous release downloads
# (`github.com/<repo>/releases/download/...`) and its anonymous API both started
# answering 504 for most requests, everywhere, while signed-in requests went
# through. Every installer and every updater is anonymous, so a release that
# GitHub had published perfectly well could not be installed. Pages is served
# from a different front end, and the mirror is complete on its own: the files,
# their checksums and the list of versions, so nothing on it needs the API.
#
# What it writes into the output directory:
#
#   <tag>/<file>        every file of the newest releases, verified on the way
#   releases.json       those releases, shaped like GitHub's API answer — the
#                       subset `crates/butai-update` reads, pointing here
#   latest, latest-dev  the newest stable tag and the newest dev tag, one line
#                       each, for `scripts/install.sh` when the API is down
#   install.sh/.ps1     the installers from this checkout
#   install-dev.sh      install.sh set to the dev track, written into the config
#   install-stable.sh   install.sh set to the stable track, written likewise
#   index.html          the same, for a person with a browser
#
# Pages deploys a whole site at a time, so every run rebuilds it from the
# releases rather than adding one: the newest MIRROR_DEV prereleases and
# MIRROR_STABLE stable releases. A Pages site is capped at 1 GB, and a release
# is about 60 MB.
#
# It downloads through `gh`, which signs in — the path that kept working.
#
# Usage:  scripts/mirror-site.sh <out-dir>
# Needs:  gh (signed in, or GH_TOKEN), jq, sha256sum
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${GITHUB_REPOSITORY:-dieterpl/butai}"
MIRROR_URL="${MIRROR_URL:-https://dieterpl.github.io/butai}"
MIRROR_DEV="${MIRROR_DEV:-5}"
MIRROR_STABLE="${MIRROR_STABLE:-3}"

out="${1:?usage: scripts/mirror-site.sh <out-dir>}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

# Newest first, by publish date: GitHub's list is not in date order (it put
# v1.5.0-dev.9 ahead of v1.5.0-dev.11). Drafts are not public.
all=$(gh api "repos/$REPO/releases?per_page=100" \
    --jq '[.[] | select(.draft | not)] | sort_by(.published_at) | reverse')
tags=$(jq -r --argjson dev "$MIRROR_DEV" --argjson stable "$MIRROR_STABLE" '
    ([.[] | select(.prerelease)][:$dev] + [.[] | select(.prerelease | not)][:$stable])
    | sort_by(.published_at) | reverse | .[].tag_name' <<<"$all")

# A tag or a file name is written into paths, JSON and HTML below. Ours are
# all plain, and anything else is skipped rather than escaped.
plain() { [[ $1 =~ ^[A-Za-z0-9._-]+$ ]]; }

entries=()
for tag in $tags; do
    plain "$tag" || { echo "skipping $tag: not a plain name" >&2; continue; }
    echo "==> $tag"
    dir="$out/$tag"
    rm -rf "$dir"
    mkdir -p "$dir"
    gh release download "$tag" --repo "$REPO" --dir "$dir"

    release=$(jq --arg t "$tag" '.[] | select(.tag_name == $t)' <<<"$all")
    assets='[]'
    for path in "$dir"/*; do
        name=$(basename "$path")
        plain "$name" || { echo "skipping $tag/$name: not a plain name" >&2; rm -f "$path"; continue; }
        sum=$(sha256sum "$path" | cut -d' ' -f1)

        # GitHub's own record of the file, where it keeps one, and the
        # release's SHA256SUMS. The mirror republishes neither unless the bytes
        # it fetched match both.
        recorded=$(jq -r --arg n "$name" '.assets[] | select(.name == $n) | .digest // ""' <<<"$release")
        if [ -n "$recorded" ] && [ "$recorded" != "sha256:$sum" ]; then
            echo "error: $tag/$name does not match GitHub's digest ($recorded, got $sum)" >&2
            exit 1
        fi
        if [ -f "$dir/SHA256SUMS" ] && [ "$name" != SHA256SUMS ]; then
            listed=$(awk -v n="$name" '$2 == n || $2 == "*" n { print $1 }' "$dir/SHA256SUMS")
            if [ -n "$listed" ] && [ "$listed" != "$sum" ]; then
                echo "error: $tag/$name does not match SHA256SUMS ($listed, got $sum)" >&2
                exit 1
            fi
        fi

        url="$MIRROR_URL/$tag/$name"
        assets=$(jq --arg n "$name" --arg u "$url" --arg d "sha256:$sum" --argjson s "$(stat -c %s "$path")" \
            '. + [{name: $n, url: $u, browser_download_url: $u, digest: $d, size: $s}]' <<<"$assets")
    done

    entries+=("$(jq --argjson a "$assets" \
        '{tag_name, name, draft, prerelease, published_at, html_url, assets: $a}' <<<"$release")")
done

[ ${#entries[@]} -gt 0 ] || { echo "error: no releases to mirror" >&2; exit 1; }
printf '%s\n' "${entries[@]}" | jq -s . > "$out/releases.json"

jq -r '[.[] | select(.prerelease | not)][0].tag_name // empty' "$out/releases.json" > "$out/latest"
jq -r '[.[] | select(.prerelease)][0].tag_name // empty' "$out/releases.json" > "$out/latest-dev"

cp "$REPO_ROOT/scripts/install.sh" "$REPO_ROOT/scripts/install.ps1" "$out/"

# The same installer with its two defaults changed, so a person picks a track by
# picking a URL rather than typing variables. Generated rather than kept as
# copies, so the three cannot drift; and checked, so a reworded line in
# install.sh fails the build instead of shipping a dev installer that installs
# stable.
for track in dev stable; do
    sed -e "s/^channel=\"\${BUTAI_CHANNEL:-stable}\"$/channel=\"\${BUTAI_CHANNEL:-$track}\"/" \
        -e 's/^set_channel="${BUTAI_SET_CHANNEL:-}"$/set_channel="${BUTAI_SET_CHANNEL:-1}"/' \
        -e "s/^# butai installer\.$/# butai installer, for the $track track (generated by scripts\/mirror-site.sh)./" \
        "$REPO_ROOT/scripts/install.sh" > "$out/install-$track.sh"
    grep -qx "channel=\"\${BUTAI_CHANNEL:-$track}\"" "$out/install-$track.sh" &&
        grep -qx 'set_channel="${BUTAI_SET_CHANNEL:-1}"' "$out/install-$track.sh" ||
        { echo "error: could not make install-$track.sh — did install.sh's defaults change?" >&2; exit 1; }
done
touch "$out/.nojekyll"

# ── index.html ───────────────────────────────────────────────────────────
{
    cat <<HTML
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>butai downloads</title>
<style>
  :root { --bg: #fdfcfa; --ink: #1d1b18; --faint: #6b665e; --line: #e4e0d8; --accent: #b4441c; --code: #f3f0ea; }
  @media (prefers-color-scheme: dark) {
    :root { --bg: #161513; --ink: #ece8e1; --faint: #9a958c; --line: #2e2c28; --accent: #ef8a5a; --code: #211f1c; }
  }
  body { background: var(--bg); color: var(--ink); font: 15px/1.55 system-ui, sans-serif; margin: 0; padding: 32px 16px 64px; }
  main { max-width: 860px; margin: 0 auto; }
  h1 { font-size: 26px; margin: 0 0 4px; }
  h2 { font-size: 18px; margin: 36px 0 6px; }
  p, li { color: var(--faint); }
  a { color: var(--accent); }
  pre { background: var(--code); border: 1px solid var(--line); border-radius: 6px; padding: 10px 12px; overflow-x: auto; font-size: 13px; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  td { border-top: 1px solid var(--line); padding: 5px 8px 5px 0; vertical-align: top; }
  td.sum { font-family: ui-monospace, monospace; color: var(--faint); word-break: break-all; }
  .tag { font-size: 12px; border: 1px solid var(--line); border-radius: 999px; padding: 1px 8px; margin-left: 6px; color: var(--faint); }
</style>
</head>
<body>
<main>
<h1>butai downloads</h1>
<p>A mirror of the <a href="https://github.com/$REPO/releases">GitHub releases</a>, served by GitHub Pages.
Every file here was checked against GitHub's own checksum for it before it was published.</p>
<h2>Install</h2>
<p>Stable releases:</p>
<pre>curl -fsSL $MIRROR_URL/install-stable.sh | sh</pre>
<p>Dev builds, cut from <code>develop</code>:</p>
<pre>curl -fsSL $MIRROR_URL/install-dev.sh | sh</pre>
<p>Either one installs over the butai you have, restarts its daemon with your workspaces kept, and sets
<code>[update] channel</code> so butai keeps itself on that track. Run it again to update.
<code>install.sh</code> is the plain installer, which leaves the config alone.
The installers and <code>butai update</code> already fall back to this mirror when GitHub's download links fail.</p>
HTML
    jq -r '.[] | "\(.tag_name)\t\(.prerelease)\t\(.published_at[:10])"' "$out/releases.json" |
        while IFS=$'\t' read -r tag pre date; do
            label=stable
            [ "$pre" = true ] && label=dev
            echo "<h2 id=\"$tag\">$tag<span class=\"tag\">$label · $date</span></h2>"
            echo "<table>"
            jq -r --arg t "$tag" '.[] | select(.tag_name == $t) | .assets[] | "\(.name)\t\(.digest[7:])"' "$out/releases.json" |
                while IFS=$'\t' read -r name sum; do
                    echo "<tr><td><a href=\"$tag/$name\">$name</a></td><td class=\"sum\">$sum</td></tr>"
                done
            echo "</table>"
        done
    cat <<HTML
</main>
</body>
</html>
HTML
} > "$out/index.html"

echo "==> $(du -sh "$out" | cut -f1) in $out"
