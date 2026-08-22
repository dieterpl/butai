"""Start, configure and inspect a real `butai daemon` process.

Everything the daemon reads or writes outside a project lives under `~/.butai`
(`butai_core::paths::butai_dir`), so giving each daemon its own `HOME` isolates
its socket, config, logs and session store in one move. That is what makes it
safe to run a dozen differently-configured daemons in the same container.
"""

import os
import shutil
import signal
import subprocess
import tempfile
import time

from .butai import Framed, Http, Target

__all__ = ["Daemon", "Config", "binary_path", "fakeagent_dir", "temp_base"]

START_TIMEOUT = 20.0
STOP_TIMEOUT = 10.0

# `sockaddr_un.sun_path` is 104 bytes on macOS and 108 on Linux, so a daemon
# whose HOME sits under a deep temp directory cannot bind at all. Leave headroom
# for `/home/.butai/butai.sock` and fail with something actionable if a caller
# still manages to exceed it.
MAX_SOCKET_PATH = 100


def binary_path():
    return os.environ.get("BUTAI_BIN", "/usr/local/bin/butai")


def fakeagent_dir():
    return os.environ.get("BUTAI_FAKEAGENTS", "/opt/butai-testsuite/fakeagents")


def temp_base():
    """The shortest usable directory to put daemon roots in.

    `tempfile.gettempdir()` is `/var/folders/<hash>/<hash>/T` on macOS, which on
    its own is most of the socket-path budget — hence the preference for `/tmp`.
    Override with `BUTAI_TEST_TMP`.
    """
    override = os.environ.get("BUTAI_TEST_TMP")
    if override:
        os.makedirs(override, exist_ok=True)
        return override
    if os.path.isdir("/tmp") and os.access("/tmp", os.W_OK):
        return "/tmp"
    return tempfile.gettempdir()


class Config:
    """Builder for `~/.butai/config.toml`.

    Defaults differ from butai's own in exactly one way that matters:
    `exit_when_empty` is off. The daemon normally exits once its last workspace
    closes, which is right for a user and fatal for a test that kills a
    workspace and then asks another question.
    """

    def __init__(self):
        self.general = {
            "default_shell": "/bin/sh",
            "exit_when_empty": False,
            "scrollback": 5000,
        }
        self.agents = []
        self.theme = {}
        self.ui = {}
        self.keys = {}
        self.extra = ""

    def agent(self, name, command, args=(), env=None):
        self.agents.append(
            {"name": name, "command": command, "args": list(args), "env": dict(env or {})}
        )
        return self

    def fake_agents(self, *names):
        """Register the scripted agent CLIs shipped with the suite."""
        for name in names:
            self.agent(name, os.path.join(fakeagent_dir(), name))
        return self

    def shell_agent(self, name="sh"):
        """A plain shell as an agent — the trick the in-repo e2e tests use to
        drive real agent-state transitions without installing any agent CLI."""
        return self.agent(name, "/bin/sh")

    def set(self, **general):
        self.general.update(general)
        return self

    def render(self):
        out = ["[general]"]
        for k, v in self.general.items():
            out.append(f"{k} = {_toml(v)}")
        if self.keys:
            out.append("\n[keys]")
            for k, v in self.keys.items():
                out.append(f"{_toml(k)} = {_toml(v)}")
        if self.theme:
            out.append("\n[theme]")
            for k, v in self.theme.items():
                out.append(f"{k} = {_toml(v)}")
        if self.ui:
            out.append("\n[ui]")
            for k, v in self.ui.items():
                out.append(f"{k} = {_toml(v)}")
        for agent in self.agents:
            out.append("\n[[agents]]")
            out.append(f"name = {_toml(agent['name'])}")
            out.append(f"command = {_toml(agent['command'])}")
            out.append(f"args = {_toml(agent['args'])}")
            if agent["env"]:
                pairs = ", ".join(f"{k} = {_toml(v)}" for k, v in agent["env"].items())
                out.append(f"env = {{ {pairs} }}")
        if self.extra:
            out.append("\n" + self.extra)
        return "\n".join(out) + "\n"


def _toml(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(_toml(v) for v in value) + "]"
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'


class Daemon:
    """A running `butai daemon`, isolated in its own HOME."""

    def __init__(self, name="d", config=None, env=None, root=None, write_config=True):
        self.name = name
        self.config = config if config is not None else Config()
        self.write_config = write_config
        self._owns_root = root is None
        # The directory name is short and opaque rather than derived from the
        # test name: every byte here comes out of the socket-path budget, and
        # `name` is still carried for error messages.
        self.root = root or tempfile.mkdtemp(prefix="butai", dir=temp_base())
        self.home = os.path.join(self.root, "home")
        self.work = os.path.join(self.root, "work")
        self.butai_dir = os.path.join(self.home, ".butai")
        self.socket = os.path.join(self.butai_dir, "butai.sock")
        self.proc = None
        self.stderr_path = os.path.join(self.root, "daemon.stderr")
        self._stderr = None
        self._extra_env = dict(env or {})
        self.http = Http(self.socket)

    # -- lifecycle ---------------------------------------------------------

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc):
        self.stop()
        return False

    def env(self):
        env = dict(os.environ)
        env.update(
            {
                "HOME": self.home,
                "BUTAI_SOCKET": self.socket,
                "RUST_LOG": os.environ.get("BUTAI_TEST_RUST_LOG", "info"),
                "RUST_BACKTRACE": "1",
            }
        )
        # A pane inherits the daemon's environment, and the daemon sets `$BUTAI`
        # in each one itself. Inheriting a stale value from whatever launched
        # the suite would make every nesting check meaningless.
        env.pop("BUTAI", None)
        env.update(self._extra_env)
        return env

    def start(self):
        if len(self.socket) > MAX_SOCKET_PATH:
            raise RuntimeError(
                f"socket path is {len(self.socket)} bytes, over the {MAX_SOCKET_PATH}-byte "
                f"budget for a Unix socket:\n  {self.socket}\n"
                "Set BUTAI_TEST_TMP to a shorter directory."
            )
        os.makedirs(self.butai_dir, mode=0o700, exist_ok=True)
        os.makedirs(self.work, exist_ok=True)
        if self.write_config:
            with open(os.path.join(self.butai_dir, "config.toml"), "w") as fh:
                fh.write(self.config.render())
        self._stderr = open(self.stderr_path, "wb")
        self.proc = subprocess.Popen(
            [binary_path(), "daemon"],
            env=self.env(),
            cwd=self.work,
            stdin=subprocess.DEVNULL,
            stdout=self._stderr,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.wait_ready()
        return self

    def wait_ready(self, timeout=START_TIMEOUT):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(
                    f"daemon exited with {self.proc.returncode} before binding:\n{self.stderr()}"
                )
            if os.path.exists(self.socket):
                # The socket file appears before the listener accepts, so
                # readiness means a request that actually round-trips.
                try:
                    self.http.get("/v1/workspaces", timeout=2.0)
                    return
                except Exception:
                    pass
            time.sleep(0.05)
        raise RuntimeError(f"daemon never became ready in {timeout}s:\n{self.stderr()}")

    def stop(self, timeout=STOP_TIMEOUT):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        if self._stderr:
            self._stderr.close()
            self._stderr = None
        if self._owns_root and not os.environ.get("BUTAI_KEEP_TMP"):
            shutil.rmtree(self.root, ignore_errors=True)

    def kill(self, sig=signal.SIGKILL):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(sig)

    @property
    def pid(self):
        return self.proc.pid if self.proc else None

    def alive(self):
        return self.proc is not None and self.proc.poll() is None

    def wait_dead(self, timeout=10.0):
        """Wait for the daemon to exit; returns its code, or None if it lived."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                return self.proc.returncode
            time.sleep(0.05)
        return None

    # -- clients -----------------------------------------------------------

    def framed(self, encoding="json", **kw):
        return Framed(self.socket, encoding=encoding, **kw)

    def attach(self, target=None, cols=80, rows=24, cwd=None, encoding="json"):
        """Connect, handshake, and return the ready client."""
        client = self.framed(encoding=encoding)
        client.hello(target=target, cols=cols, rows=rows, cwd=cwd or self.work)
        return client

    def stage(self, ws=None, path=None, cols=80, rows=24, encoding="json"):
        """A client streaming the pane a workspace has on its stage.

        The daemon draws exactly one thing — a program's cells coming off a
        PTY — so a test that wants to see output attaches to the pane holding
        it. The workbench around it is JSON on `/v1/*` and every client
        composes its own, which is why nothing here reads a rail.

        Opens a workspace on `path` when `ws` is not given, so the common case
        is one call. Returns `(ws, client)`.
        """
        if ws is None:
            ws = self.http.new_workspace(path=path or self.work)
        pane = self.staged_pane(ws)
        client = self.framed(encoding=encoding)
        client.hello(target=Target.pane(pane), cols=cols, rows=rows, cwd=self.work)
        return ws, client

    def staged_pane(self, ws, past=None, timeout=20.0):
        """The pane on a workspace's stage, once it is one the test has not seen.

        Polled, and past a pane rather than merely for one, because both ends
        race: a workspace exists before the shell it opens with does, and a
        spawn takes the stage a moment after the call that asked for it
        returns. `past` is the pane being left behind.
        """
        detail = self.http.poll_until(
            f"/v1/workspaces/{ws}",
            lambda d: isinstance(d, dict)
            and d.get("stage") is not None
            and d["stage"] != past,
            f"workspace {ws} staged a pane past {past}",
            timeout=timeout,
        )
        return detail["stage"]

    def cli(self, *args, timeout=30, check=False, cwd=None, env=None):
        """Run a `butai` subcommand against this daemon."""
        full_env = self.env()
        full_env.update(env or {})
        return subprocess.run(
            [binary_path(), *args],
            env=full_env,
            cwd=cwd or self.work,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=check,
        )

    # -- logs --------------------------------------------------------------

    def stderr(self):
        try:
            with open(self.stderr_path, "r", errors="replace") as fh:
                return fh.read()
        except OSError:
            return ""

    def log(self):
        """The daemon's rolling file log plus anything it wrote to stderr."""
        parts = []
        log_dir = os.path.join(self.butai_dir, "logs")
        if os.path.isdir(log_dir):
            for name in sorted(os.listdir(log_dir)):
                try:
                    with open(os.path.join(log_dir, name), "r", errors="replace") as fh:
                        parts.append(fh.read())
                except OSError:
                    pass
        parts.append(self.stderr())
        return "\n".join(parts)

    def log_lines(self, needle):
        return [line for line in self.log().splitlines() if needle in line]

    def panics(self):
        return self.log_lines("panicked at")

    def slow_loops(self):
        """`core loop blocked for Nms` warnings — the daemon's own stall alarm."""
        return self.log_lines("core loop blocked")

    def slowest_loop_ms(self):
        worst = 0
        for line in self.slow_loops():
            for token in line.replace("ms", " ").split():
                if token.isdigit():
                    worst = max(worst, int(token))
        return worst

    def warnings(self):
        return [line for line in self.log().splitlines() if " WARN " in line]

    def assert_healthy(self):
        """Fail loudly on the two things that must never happen."""
        panics = self.panics()
        assert not panics, "daemon panicked:\n" + "\n".join(panics[:10])
        assert self.alive(), f"daemon died (code {self.proc.returncode}):\n{self.stderr()[-4000:]}"
