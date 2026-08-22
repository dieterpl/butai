# syntax=docker/dockerfile:1.7
#
# The butai test image: a real daemon binary, a real Linux userspace, and the
# terminal applications the suite drives. Build context is the repository root:
#
#   docker build -f testsuite/Dockerfile -t butai-testsuite .
#
# `testsuite/run.sh` does that for you.

# ---------------------------------------------------------------------------
# Build the daemon
# ---------------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build

# libgit2 and zlib are compiled from vendored C by the `cc` crate, so a C
# toolchain is mandatory. There is deliberately nothing else: git2 is built with
# default features off (no openssl-sys, no libssh2-sys) and syntect uses
# fancy-regex, so there is no oniguruma either.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates

# Cache mounts keep the registry and target directory out of the image layers,
# which also means the build never touches the host's `target/` — worth having
# when the working copy lives on a network mount.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release -p butai \
    && install -m 0755 target/release/butai /usr/local/bin/butai

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Three groups, and it is worth knowing which is which:
#   the daemon needs      git (push/show shell-outs), GNU grep (workspace
#                         search), procps and coreutils (pane commands);
#   the suite needs       python3 (standard library only, no pip);
#   the app matrix needs  the terminal programs it drives.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        coreutils \
        git \
        grep \
        procps \
        ncurses-bin \
        python3 \
        bash \
        zsh \
        fish \
        vim \
        nano \
        less \
        htop \
        btop \
        ncdu \
        mc \
        tmux \
        tree \
        jq \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /usr/local/bin/butai /usr/local/bin/butai

# A repository owned by a *different* uid than the daemon runs as. libgit2
# refuses to open it without `safe.directory`, and butai swallows that error —
# so the workspace simply has no CHANGES rail. It is the most common "butai is
# broken in Docker" report, and `test_30_git.py` pins both the symptom and the
# fix. Built here because reproducing it needs root, which the suite does not
# have at runtime.
RUN mkdir -p /opt/butai-testsuite/foreign-repo \
    && cd /opt/butai-testsuite/foreign-repo \
    && git init -q --initial-branch=main \
    && echo "owned by another user" > README.md \
    && git add README.md \
    && git -c user.name=other -c user.email=other@example.invalid \
        commit -q -m "a repo this container's user does not own" \
    && echo "uncommitted" >> README.md \
    && chown -R 4242:4242 /opt/butai-testsuite/foreign-repo \
    && chmod -R a+rX /opt/butai-testsuite/foreign-repo

COPY testsuite/suite /opt/butai-testsuite/suite
COPY testsuite/tests /opt/butai-testsuite/tests
COPY testsuite/fakeagents /opt/butai-testsuite/fakeagents
RUN chmod -R 0755 /opt/butai-testsuite/fakeagents

RUN useradd --create-home --uid 1000 --shell /bin/bash butai \
    && mkdir -p /out \
    && chown butai:butai /out

ENV HOME=/home/butai \
    SHELL=/bin/bash \
    TERM=xterm-256color \
    BUTAI_BIN=/usr/local/bin/butai \
    BUTAI_FAKEAGENTS=/opt/butai-testsuite/fakeagents \
    BUTAI_FOREIGN_REPO=/opt/butai-testsuite/foreign-repo \
    BUTAI_OUT=/out \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

USER butai
WORKDIR /opt/butai-testsuite

# No git identity is configured on purpose: the fixtures set their own per repo,
# and `test_30_git.py` asserts what a bare container does without one.

ENTRYPOINT ["python3", "-m", "suite"]
CMD ["standard"]
