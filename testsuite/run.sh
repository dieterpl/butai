#!/usr/bin/env bash
#
# Build the butai test image and run a profile against it.
#
#   ./testsuite/run.sh smoke                  ~1 min   quick gate
#   ./testsuite/run.sh standard               ~10 min  the default
#   ./testsuite/run.sh soak --minutes 30               drift detection
#   ./testsuite/run.sh standard --real-agents          adds the real agent CLIs
#   ./testsuite/run.sh standard --filter http --keep
#
# Profiles are cumulative: standard runs smoke too, soak runs all three.
#
# A few scenarios need container limits that cannot be changed from inside a
# running container, so standard and soak also run short extra passes with
# --pids-limit, --memory and --cpus applied. Reports land in testsuite/out/.

# No `-u`: macOS still ships bash 3.2, where an empty "${array[@]}" under `set
# -u` is an error rather than nothing.
set -eo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(dirname "$here")"
image="${BUTAI_TEST_IMAGE:-butai-testsuite:latest}"
# Reports land beside the suite by default. `BUTAI_TEST_OUT` moves them, which is
# worth doing when the working copy sits on a network mount — the container
# writes several files per lane and some mounts handle that poorly.
out="${BUTAI_TEST_OUT:-$here/out}"

profile="standard"
real_agents=0
build=1
keep=0
list=0
lanes=1
platform=""
minutes=""
scale=""
filters=()

die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
note() { printf '\033[36m==>\033[0m %s\n' "$*"; }

usage() {
    cat <<'EOF'
usage: run.sh [smoke|standard|soak] [options]

  smoke     ~1 min   API and protocol gate
  standard  ~10 min  the default: full API, git, agents, apps, stress
  soak      long     adds drift detection (see --minutes)

Options:
  --real-agents     build the layer that installs claude/codex/gemini/aider and
                    run against them (credentials are read from the environment;
                    tests skip cleanly when they are absent)
  --filter PATTERN  only run tests matching a name, module or tag (repeatable)
  --minutes N       soak duration (default 30)
  --scale N         stress load multiplier (default 1)
  --no-lanes        skip the constrained-container passes
  --no-build        reuse the existing image
  --keep            keep the container's temp directories for debugging
  --platform P      docker platform, e.g. linux/amd64
  --list            list the tests a profile would run, then exit
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        smoke|standard|soak) profile="$1" ;;
        --real-agents) real_agents=1 ;;
        --filter) filters+=("$2"); shift ;;
        --filter=*) filters+=("${1#*=}") ;;
        --minutes) minutes="$2"; shift ;;
        --scale) scale="$2"; shift ;;
        --no-lanes) lanes=0 ;;
        --no-build) build=0 ;;
        --keep) keep=1 ;;
        --platform) platform="$2"; shift ;;
        --list) list=1 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done

command -v docker >/dev/null 2>&1 || die "docker is not installed"
docker info >/dev/null 2>&1 || die "cannot reach the docker daemon — is Docker Desktop running?"

platform_args=()
if [ -n "$platform" ]; then
    platform_args=(--platform "$platform")
fi

# ---------------------------------------------------------------------------
# build
# ---------------------------------------------------------------------------
if [ "$build" = 1 ]; then
    note "building $image (the first run compiles butai; later runs hit the cache)"
    DOCKER_BUILDKIT=1 docker build \
        "${platform_args[@]}" \
        -f "$here/Dockerfile" \
        -t "$image" \
        "$repo"

    if [ "$real_agents" = 1 ]; then
        note "building $image-real (adds the real agent CLIs)"
        DOCKER_BUILDKIT=1 docker build \
            "${platform_args[@]}" \
            --build-arg "BASE=$image" \
            -f "$here/real-agents/Dockerfile" \
            -t "$image-real" \
            "$here/real-agents"
    fi
fi

run_image="$image"
if [ "$real_agents" = 1 ]; then
    run_image="$image-real"
fi

mkdir -p "$out"

common_env=()
if [ "$keep" = 1 ]; then
    common_env+=(-e BUTAI_KEEP_TMP=1)
fi
if [ -n "$scale" ]; then
    common_env+=(-e "BUTAI_SCALE=$scale")
fi

# Credentials are forwarded only when set, so the real-agent lane skips with a
# reason instead of failing.
for var in ANTHROPIC_API_KEY OPENAI_API_KEY GEMINI_API_KEY GOOGLE_API_KEY; do
    if [ -n "${!var}" ]; then
        common_env+=(-e "$var")
    fi
done

suite_args=("$profile")
for f in "${filters[@]}"; do
    suite_args+=(--filter "$f")
done
if [ -n "$minutes" ]; then
    suite_args+=(--minutes "$minutes")
fi

if [ "$list" = 1 ]; then
    exec docker run --rm "${platform_args[@]}" "$run_image" "${suite_args[@]}" --list
fi

failures=()

# Arguments before `--` go to docker, after it to the suite.
# `--init` gives the container a real pid 1, so PTY children that outlive their
# pane get reaped instead of accumulating as zombies — which matters most in the
# pid-limited lane.
run_lane() {
    local name="$1"; shift
    local -a docker_args=()
    local -a extra_args=()
    local target="docker"
    local arg
    for arg in "$@"; do
        if [ "$arg" = "--" ]; then
            target="suite"
            continue
        fi
        if [ "$target" = "suite" ]; then
            extra_args+=("$arg")
        else
            docker_args+=("$arg")
        fi
    done

    note "lane: $name"
    if ! docker run --rm --init \
        "${platform_args[@]}" \
        "${common_env[@]}" \
        "${docker_args[@]}" \
        -v "$out:/out" \
        "$run_image" \
        "${suite_args[@]}" "${extra_args[@]}" --out "/out/$name"
    then
        failures+=("$name")
    fi
}

run_lane main

if [ "$lanes" = 1 ] && [ "$profile" != "smoke" ] && [ ${#filters[@]} -eq 0 ]; then
    pids_limit=120
    run_lane pids \
        --pids-limit "$pids_limit" \
        -e BUTAI_LANE=pids \
        -e "BUTAI_PIDS_LIMIT=$pids_limit" \
        -- --filter limits

    run_lane memory \
        --memory 1g --memory-swap 1g \
        -e BUTAI_LANE=memory \
        -e BUTAI_MEMORY_LIMIT=1g \
        -- --filter limits

    run_lane cpu \
        --cpus 1 \
        -e BUTAI_LANE=cpu \
        -e BUTAI_CPU_LIMIT=1 \
        -- --filter limits
fi

echo
if [ ${#failures[@]} -eq 0 ]; then
    note "all lanes passed — reports in $out"
    exit 0
fi

printf '\033[31mfailed lanes:\033[0m %s\n' "${failures[*]}"
printf 'reports in %s\n' "$out"
exit 1
