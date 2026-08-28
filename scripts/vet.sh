#!/usr/bin/env bash
#
# Vet a branch — every check CI runs, and then the build itself, against your
# real work.
#
#   scripts/vet.sh                    the working tree, uncommitted changes and all
#   scripts/vet.sh feat/minimap       a branch, in a worktree of its own
#   scripts/vet.sh --run              ...and leave a daemon up on it to drive
#   scripts/vet.sh feat/x --full      the standard testsuite instead of smoke
#
# The last step is the point of the script. Everything above it is CI, which
# will run anyway; what CI cannot tell you is whether the thing is any good to
# use. `--run` builds the branch and starts a daemon on it with `$BUTAI_HOME`
# pointed at `~/.butai-dev`, so it gets its own socket, session store, pane
# dumps and logs — and your real `$HOME`, ssh config, shell profile, git
# identity and repositories, because a build tried without those has not been
# tried. Your own daemon keeps running throughout; the two do not meet.
#
# Nothing here is destructive. The worktree is removed at the end unless
# `--keep`; `~/.butai-dev` is left alone, since the session in it is the whole
# reason to come back.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# The rustup shim, when there is one. `rust-toolchain.toml` pins the toolchain
# and only rustup honours it, so a distro cargo earlier on PATH builds this tree
# with the wrong compiler — or, if it is old enough, fails to parse `Cargo.lock`
# and reports that as the problem.
[ -x "$HOME/.cargo/bin/cargo" ] && PATH="$HOME/.cargo/bin:$PATH"

BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'
CYAN=$'\033[36m'; DIM=$'\033[2m'; OFF=$'\033[0m'
[ -t 1 ] || { BOLD=; RED=; GREEN=; YELLOW=; CYAN=; DIM=; OFF=; }

die()  { printf '%serror:%s %s\n' "$RED" "$OFF" "$*" >&2; exit 1; }
note() { printf '%s==>%s %s\n' "$CYAN" "$OFF" "$*"; }

branch=""
run=0
full=0
keep=0
suite=1
dev_home="${BUTAI_DEV_HOME:-$HOME/.butai-dev}"

usage() {
    sed -n '3,9p' "$0" | sed 's/^# \{0,1\}//'
    cat <<'EOF'
Options:
  --run         build the branch and start a daemon on it when the gate passes
  --full        testsuite `standard` (~10 min) instead of `smoke` (~1 min)
  --no-suite    skip the docker testsuite entirely
  --keep        keep the worktree after the run
  -h, --help    this

Environment:
  BUTAI_DEV_HOME   where --run keeps its state (default ~/.butai-dev)
EOF
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --run) run=1 ;;
        --full) full=1 ;;
        --no-suite) suite=0 ;;
        --keep) keep=1 ;;
        -h|--help) usage ;;
        -*) die "unknown option: $1" ;;
        *) [ -z "$branch" ] || die "one branch at a time (got '$branch' and '$1')"; branch="$1" ;;
    esac
    shift
done

# ── where the checks run ─────────────────────────────────────────────────
#
# A named branch gets a worktree, so your working copy — and whatever is
# half-finished in it — is untouched. No branch means *this* tree, uncommitted
# changes included, which is the case worth optimising for: the thing you most
# want to vet is usually the thing you have not committed yet.
worktree=""
cleanup() {
    if [ -n "$worktree" ] && [ "$keep" -eq 0 ]; then
        git worktree remove --force "$worktree" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

if [ -n "$branch" ]; then
    git rev-parse --verify "$branch" >/dev/null 2>&1 || die "no such branch: $branch"
    worktree="$(mktemp -d "${TMPDIR:-/tmp}/butai-vet-XXXXXX")/tree"
    note "checking out ${BOLD}$branch${OFF} in a worktree"
    # Detached: a branch checked out twice is a branch two builds can move.
    git worktree add --detach --quiet "$worktree" "$branch"
    tree="$worktree"
    what="$branch"
else
    tree="$REPO_ROOT"
    what="$(git rev-parse --abbrev-ref HEAD)"
    git diff --quiet && git diff --cached --quiet || what="$what ${DIM}(+ uncommitted)${OFF}"
fi

# One target directory across every vet run, so the second one is incremental.
# Outside the tree because a worktree is thrown away at the end, and rebuilding
# the world each time would make the script something nobody runs.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/butai-vet-target}"

printf '\n%s┌─ vetting %s%s\n' "$BOLD" "$what" "$OFF"
printf '%s│  tree:   %s%s\n' "$DIM" "$tree" "$OFF"
printf '%s└─ target: %s%s\n\n' "$DIM" "$CARGO_TARGET_DIR" "$OFF"

# ── the gate ─────────────────────────────────────────────────────────────
passed=(); failed=(); skipped=()

step() {
    local name="$1"; shift
    printf '%s──%s %s\n' "$CYAN" "$OFF" "$name"
    if ( "$@" ) 2>&1 | sed 's/^/   /'; then
        printf '   %s✓ %s%s\n\n' "$GREEN" "$name" "$OFF"
        passed+=("$name")
    else
        printf '   %s✗ %s%s\n\n' "$RED" "$name" "$OFF"
        failed+=("$name")
    fi
}

skip() {
    printf '%s──%s %s\n   %s· skipped — %s%s\n\n' "$CYAN" "$OFF" "$1" "$YELLOW" "$2" "$OFF"
    skipped+=("$1")
}

in_tree() { ( cd "$tree" && "$@" ); }

if command -v cargo >/dev/null 2>&1; then
    # The same three CONTRIBUTING.md asks for, with the warnings-are-errors
    # flag CI sets — so a clippy lint that only fails in CI fails here too.
    export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
    step "cargo fmt"    in_tree cargo fmt --all --check
    step "cargo clippy" in_tree cargo clippy --all-targets --all-features
    step "cargo test"   in_tree cargo test --all --all-features

    # `--all-features` above turned on `butai-protocol`'s `ts` feature, whose
    # export tests rewrite the browser client's DTOs. If that changed anything,
    # the checked-in bindings are stale — the exact check CI runs, and the one
    # that catches a daemon field no client ever learned about.
    step "generated TypeScript is current" \
        in_tree git diff --exit-code --stat -- web/src/protocol/generated/
else
    skip "cargo" "no cargo on PATH"
fi

if command -v bun >/dev/null 2>&1; then
    step "bun install"   in_tree bash -c 'cd web && bun install --frozen-lockfile'
    step "bun typecheck" in_tree bash -c 'cd web && bun run typecheck'
    step "bun test"      in_tree bash -c 'cd web && bun test'
    step "bun build"     in_tree bash -c 'cd web && bun run build'
else
    skip "web client" "no bun on PATH — see web/README.md"
fi

if [ "$suite" -eq 0 ]; then
    skip "docker testsuite" "--no-suite"
elif ! command -v docker >/dev/null 2>&1; then
    skip "docker testsuite" "no docker on PATH"
elif ! docker info >/dev/null 2>&1; then
    skip "docker testsuite" "docker is installed but not running"
else
    profile=smoke; [ "$full" -eq 1 ] && profile=standard
    step "testsuite ($profile)" in_tree ./testsuite/run.sh "$profile"
fi

# ── the verdict ──────────────────────────────────────────────────────────
printf '%s── verdict%s\n' "$BOLD" "$OFF"
[ ${#passed[@]}  -gt 0 ] && printf '   %s✓%s %s\n' "$GREEN" "$OFF" "${passed[*]}"
[ ${#skipped[@]} -gt 0 ] && printf '   %s·%s %s\n' "$YELLOW" "$OFF" "${skipped[*]}"
[ ${#failed[@]}  -gt 0 ] && printf '   %s✗%s %s\n' "$RED" "$OFF" "${failed[*]}"
printf '\n'

if [ ${#failed[@]} -gt 0 ]; then
    printf '%s%s did not pass.%s Nothing was installed and no daemon was started.\n\n' \
        "$RED" "$what" "$OFF"
    exit 1
fi

if [ "$run" -eq 0 ]; then
    printf '%s%s passes.%s Add %s--run%s to try it against your own work.\n\n' \
        "$GREEN" "$what" "$OFF" "$BOLD" "$OFF"
    exit 0
fi

# ── run it against the real setup ────────────────────────────────────────
command -v cargo >/dev/null 2>&1 || die "--run needs cargo, and there is none on PATH"

note "building the release binary"
in_tree cargo build --release -p butai 2>&1 | sed 's/^/   /'
bin="$CARGO_TARGET_DIR/release/butai"
[ -x "$bin" ] || die "no binary at $bin"

sock="$dev_home/butai.sock"
# `sockaddr_un.sun_path` is 104 bytes on macOS and 108 on Linux. A daemon that
# cannot bind fails at the moment it would otherwise have started working, with
# an error that does not say why — so it is worth asking first.
if [ "${#sock}" -gt 100 ]; then
    die "the dev socket path is ${#sock} bytes, over the 100-byte budget:
       $sock
       Set BUTAI_DEV_HOME to something shorter."
fi

# Seed it once from the real config, so this is *your* setup rather than a
# default one — same keymap, same theme, same agents. A copy and not a symlink,
# and that matters: the client writes back to config.toml (answering no to an
# update prompt is a `declined_version` written into it), so a link would let a
# build you are still vetting edit the config your real butai reads.
if [ ! -d "$dev_home" ]; then
    note "seeding ${BOLD}$dev_home${OFF} from ~/.butai"
    mkdir -p "$dev_home"
    chmod 700 "$dev_home"
    [ -f "$HOME/.butai/config.toml" ] && cp "$HOME/.butai/config.toml" "$dev_home/"
    [ -d "$HOME/.butai/themes" ] && cp -r "$HOME/.butai/themes" "$dev_home/"
    printf '   %sconfig and themes copied; session, panes and logs start empty%s\n' "$DIM" "$OFF"
fi

version="$("$bin" --version 2>/dev/null || echo unknown)"

cat <<EOF

${GREEN}${what} passes, and is built.${OFF}  ${DIM}${version}${OFF}

Drive it — your projects, your ssh, your dotfiles; its own daemon:

    ${BOLD}BUTAI_HOME=$dev_home $bin${OFF}

Your own butai is untouched and still running. When you are done with this one,
stop it ${BOLD}by socket${OFF} — never by pattern, since \`pkill -f butai\` would take
the real one with it:

    BUTAI_HOME=$dev_home $bin kill-server

EOF

[ "$keep" -eq 0 ] && [ -n "$worktree" ] && printf '%sNote: the worktree goes away when this exits; the binary at%s\n%s%s%s\n%sstays, and keeps working. Use --keep to hold the sources too.%s\n\n' \
    "$DIM" "$OFF" "$DIM" "$bin" "$OFF" "$DIM" "$OFF"

exit 0
