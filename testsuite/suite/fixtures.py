"""Workspace, git-repo and probe-script builders.

There are no fixture files on disk anywhere in butai — the crate tests build
everything from `tempfile` + `git2::Repository::init`. This module keeps that
convention, so a fixture is always a few lines of code next to the test that
needs it rather than a directory someone has to go read.
"""

import os
import subprocess

__all__ = [
    "write",
    "workspace",
    "git",
    "git_repo",
    "dirty_repo",
    "bare_remote",
    "repo_with_remote",
    "conflicting_branches",
    "big_repo",
    "butai_toml",
    "probe",
    "PROBES",
]


def write(path, text, mode=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write(text)
    if mode is not None:
        os.chmod(path, mode)
    return path


def workspace(root, name="ws", files=None, butai_file=None):
    """A project directory, optionally with a `.butai.toml` and seed files."""
    path = os.path.join(root, name)
    os.makedirs(path, exist_ok=True)
    for rel, text in (files or {}).items():
        write(os.path.join(path, rel), text)
    if butai_file:
        write(os.path.join(path, ".butai.toml"), butai_file)
    return path


def butai_toml(processes=(), autostart=(), name=None):
    """Render a `.butai.toml`. `processes` is a list of (name, cmd[, ready])."""
    out = []
    if name:
        out.append(f'name = "{name}"')
    for entry in processes:
        proc_name, cmd = entry[0], entry[1]
        ready = entry[2] if len(entry) > 2 else None
        out.append("\n[[processes]]")
        out.append(f'name = "{proc_name}"')
        out.append(f'cmd = "{_esc(cmd)}"')
        if ready is not None:
            out.append(f'ready = "{_esc(ready)}"')
    if autostart:
        out.append("\n[agents]")
        out.append("autostart = [" + ", ".join(f'"{a}"' for a in autostart) + "]")
    return "\n".join(out) + "\n"


def _esc(text):
    return text.replace("\\", "\\\\").replace('"', '\\"')


# ---------------------------------------------------------------------------
# git
# ---------------------------------------------------------------------------


def git(path, *args, check=True, env=None):
    full_env = dict(os.environ)
    full_env.setdefault("GIT_CONFIG_NOSYSTEM", "1")
    full_env.update(env or {})
    return subprocess.run(
        ["git", "-C", str(path), *args],
        capture_output=True,
        text=True,
        check=check,
        env=full_env,
    )


def git_repo(path, identity=True, initial_commit=True, branch="main"):
    """Initialize a repo. `identity=False` reproduces the container case where
    libgit2 can build a tree but `repo.signature()` fails on commit."""
    os.makedirs(path, exist_ok=True)
    git(path, "init", "-q", f"--initial-branch={branch}")
    if identity:
        git(path, "config", "user.name", "butai testsuite")
        git(path, "config", "user.email", "testsuite@butai.invalid")
    else:
        # Make sure nothing global leaks in and rescues the commit.
        git(path, "config", "--unset-all", "user.name", check=False)
        git(path, "config", "--unset-all", "user.email", check=False)
    git(path, "config", "commit.gpgsign", "false")
    if initial_commit:
        write(os.path.join(path, "README.md"), "# fixture\n")
        git(path, "add", "README.md")
        git(path, "commit", "-q", "-m", "initial commit")
    return path


def dirty_repo(path, tracked_edits=1, untracked=1, staged=0):
    """A repo with a predictable working tree: N modified, N untracked, N staged."""
    git_repo(path)
    for i in range(max(tracked_edits, staged)):
        rel = f"tracked{i}.txt"
        write(os.path.join(path, rel), f"original {i}\n")
        git(path, "add", rel)
    if tracked_edits or staged:
        git(path, "commit", "-q", "-m", "add tracked files")
    for i in range(tracked_edits):
        write(os.path.join(path, f"tracked{i}.txt"), f"modified {i}\n")
    for i in range(staged):
        rel = f"staged{i}.txt"
        write(os.path.join(path, rel), f"staged {i}\n")
        git(path, "add", rel)
    for i in range(untracked):
        write(os.path.join(path, f"untracked{i}.txt"), f"untracked {i}\n")
    return path


def bare_remote(path):
    """A bare repo to push to and fetch from.

    Every remote-sync test uses one of these over a plain filesystem path: the
    suite must never touch a network, and a local remote exercises the same
    daemon code paths — spawn, progress, lock, completion — that a real one does.
    """
    os.makedirs(path, exist_ok=True)
    git(path, "init", "-q", "--bare", "--initial-branch=main")
    return path


def repo_with_remote(path, remote_path, behind=0):
    """A **clean** repo tracking `origin`, optionally `behind` commits back.

    Clean on purpose: `git pull --rebase` refuses a dirty worktree, so a fixture
    that dirtied it would make half the remote-sync tests fail for a reason that
    has nothing to do with syncing. Tests that want local changes make them.

    `behind` commits are made in a scratch clone and pushed, so the working repo
    genuinely trails its upstream without having seen those commits — which is
    what makes ahead/behind and `pull` meaningful rather than a no-op.
    """
    bare_remote(remote_path)
    git_repo(path)
    git(path, "remote", "add", "origin", remote_path)
    git(path, "push", "-q", "-u", "origin", "main")

    scratch = f"{path}-upstream"
    for i in range(behind):
        if i == 0:
            git(os.path.dirname(path), "clone", "-q", remote_path, scratch)
            git(scratch, "config", "user.name", "butai testsuite")
            git(scratch, "config", "user.email", "testsuite@butai.invalid")
        write(os.path.join(scratch, f"upstream{i}.txt"), f"upstream {i}\n")
        git(scratch, "add", f"upstream{i}.txt")
        git(scratch, "commit", "-q", "-m", f"upstream commit {i}")
        git(scratch, "push", "-q", "origin", "main")
    return path


def conflicting_branches(path, name="conflict.txt"):
    """A repo whose `feature` branch conflicts with `main` in exactly one file.

    Determinism comes from the fixture, not from timing: every merge/rebase test
    starts here, so "a conflict" always means the same conflict, on the same
    line, with the same three index stages.
    """
    git_repo(path)
    write(os.path.join(path, name), "base\n")
    git(path, "add", name)
    git(path, "commit", "-q", "-m", "base")

    git(path, "checkout", "-q", "-b", "feature")
    write(os.path.join(path, name), "theirs\n")
    git(path, "commit", "-q", "-am", "theirs")

    git(path, "checkout", "-q", "main")
    write(os.path.join(path, name), "ours\n")
    git(path, "commit", "-q", "-am", "ours")
    return path


def big_repo(path, files=20000, dirs=200):
    """A repo with a lot of untracked files.

    butai's status scan runs `recurse_untracked_dirs(true)` every sampler tick,
    so this is the `node_modules` case that decides whether a real project stays
    responsive.
    """
    git_repo(path)
    per_dir = max(1, files // dirs)
    for d in range(dirs):
        sub = os.path.join(path, "node_modules", f"pkg{d}")
        os.makedirs(sub, exist_ok=True)
        for f in range(per_dir):
            with open(os.path.join(sub, f"file{f}.js"), "w") as fh:
                fh.write("module.exports = {};\n")
    return path


# ---------------------------------------------------------------------------
# terminal probe scripts
# ---------------------------------------------------------------------------

PROBES = {
    # Truecolor, 256-color and the full SGR attribute set. butai's wire `Mods`
    # carries six attributes but its vt100 -> ratatui bridge only forwards four,
    # so this is what shows which survive.
    "sgr": r"""#!/bin/bash
printf '\033[1mBOLD\033[0m \033[2mDIM\033[0m \033[3mITALIC\033[0m '
printf '\033[4mUNDER\033[0m \033[7mREVERSE\033[0m \033[9mSTRIKE\033[0m\n'
printf '\033[5mBLINK\033[0m \033[8mHIDDEN\033[0m\n'
printf '\033[38;2;255;100;0mTRUECOLOR-FG\033[0m '
printf '\033[48;2;0;80;160mTRUECOLOR-BG\033[0m\n'
printf '\033[38;5;196mINDEXED196\033[0m \033[48;5;21mINDEXEDBG21\033[0m\n'
echo SGR-PROBE-DONE
sleep 3600
""",
    # Wide characters, combining marks and emoji. A wide cell occupies two
    # columns and the trailing half arrives as an empty `ch`.
    "unicode": r"""#!/bin/bash
printf 'CJK:\346\227\245\346\234\254\350\252\236\n'
printf 'EMOJI:\360\237\232\200\360\237\216\211\n'
printf 'COMBINING:e\314\201a\314\200\n'
printf 'BOX:\342\224\214\342\224\200\342\224\220\n'
echo UNICODE-PROBE-DONE
sleep 3600
""",
    # Cursor-position and device-attribute queries. vt100 parses but never
    # answers these, so the daemon answers on the child's behalf; if that ever
    # stops, every app that probes its terminal on startup hangs before drawing.
    #
    # bash, not sh: the replies carry no newline, so the read has to be
    # delimiter- and timeout-bounded or an unanswered query wedges the probe.
    "queries": r"""#!/bin/bash
old=$(stty -g 2>/dev/null)
stty raw -echo 2>/dev/null
printf '\033[6n'
IFS= read -r -t 3 -d 'R' cpr
printf '\033[c'
IFS= read -r -t 3 -d 'c' da1
printf '\033[>c'
IFS= read -r -t 3 -d 'c' da2
stty "$old" 2>/dev/null
printf 'CPR=%s|\r\n' "${cpr//$'\033'/}"
printf 'DA1=%s|\r\n' "${da1//$'\033'/}"
printf 'DA2=%s|\r\n' "${da2//$'\033'/}"
echo QUERIES-PROBE-DONE
sleep 3600
""",
    # XTVERSION, which nothing answers. Prints before blocking so a test can
    # tell "started, then waited" from "never started", and gives up on its own
    # so the pane stays inspectable either way.
    "xtversion": r"""#!/bin/bash
echo XTVERSION-PROBE-START
old=$(stty -g 2>/dev/null)
stty raw -echo 2>/dev/null
printf '\033[>0q'
if IFS= read -r -t 8 -d '\' reply; then
    stty "$old" 2>/dev/null
    printf 'XTVERSION-PROBE-ANSWERED %s\r\n' "${reply//$'\033'/}"
else
    stty "$old" 2>/dev/null
    printf 'XTVERSION-PROBE-TIMEOUT\r\n'
fi
sleep 3600
""",
    # Alternate screen: enter, draw, and stay. Text written before the switch
    # must not be visible afterwards.
    "altscreen": r"""#!/bin/bash
echo PRIMARY-SCREEN-TEXT
sleep 0.3
printf '\033[?1049h'
printf '\033[2J\033[H'
echo ALT-SCREEN-TEXT
sleep 3600
""",
    # Reports the size the PTY was given, and again whenever it changes.
    #
    # Polls `stty size` rather than trapping SIGWINCH: a trap handler is a
    # string bash re-parses when the signal arrives, which made this probe
    # occasionally fail to start under load. Polling has no such edge, and the
    # assertion — that the child sees the new geometry — is identical.
    "winsize": r"""#!/bin/bash
last=""
while :; do
    size=$(stty size 2>/dev/null || echo "0 0")
    rows=${size%% *}
    cols=${size##* }
    now="WINSIZE ${cols}x${rows}"
    if [ "$now" != "$last" ]; then
        last=$now
        printf '%s\n' "$now"
    fi
    sleep 0.2
done
""",
    # Steady, bounded output — the soak workload.
    "heartbeat": r"""#!/bin/bash
i=0
while :; do
    i=$((i + 1))
    echo "heartbeat $i"
    sleep 0.5
done
""",
    # Unbounded output as fast as the PTY will take it — the flood workload.
    #
    # `yes` saturates a PTY faster than any shell loop, and the line is a
    # realistic width on purpose: scrollback is capped in *lines*, so a probe
    # emitting 64 KiB lines would hold 5000 x 64 KiB per pane and never plateau
    # within a test. Line-length sensitivity is worth knowing about, but it is a
    # separate question from "does a flooding pane stay bounded".
    "flood": r"""#!/bin/bash
exec yes "flood 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd"
""",
}


def probe(root, name, extra=None):
    """Materialize a probe script and return its path."""
    body = PROBES[name] if extra is None else extra
    path = os.path.join(root, f"probe-{name}.sh")
    write(path, body, mode=0o755)
    return path
