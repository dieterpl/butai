# Windows web launcher and experimental TUI alpha

The `butai.exe` binary runs the same terminal workbench, CLI, and persistent
background daemon as Linux and macOS. Use Windows Terminal on Windows 10 1809+
or Windows 11; the pane backend uses Windows ConPTY. Git commands require Git
for Windows on `PATH`. Install agent CLIs separately as usual.

## Recommended Windows entry point: browser launcher

Download `butai-web-windows-alpha.exe` from the `develop` CI artifact named
`butai-windows-web-alpha-<commit>`. Double-click it to start the daemon and
open the web interface in your default browser. No Rust, Bun or Node installation
is required: the launcher embeds the daemon, browser assets and bridge runtime.
Keep the launcher window open while using the app; closing it stops the web
bridge while the daemon keeps workspaces running. Run the launcher again to
reconnect. The bridge listens only on `127.0.0.1` on an automatically chosen port.

This browser launcher is also an alpha: it avoids the Windows TUI renderer,
but uses the same experimental native daemon and ConPTY backend. Reported TUI
crashes mean the native Windows TUI is **experimental alpha**, unsuitable for
reliable daily use. Linux and macOS retain their existing terminal interface.

State remains in `%USERPROFILE%\.butai`. The embedded daemon is extracted to
`%LOCALAPPDATA%\butai\web-runtime\<checksum>` and checked before starting.
Existing running daemons are reused; stop an older daemon before switching
builds if you need the new backend. `BUTAI_HOME` selects a separate state directory.

## Build and run the experimental TUI

Install Rust 1.88+ and Visual Studio C++ Build Tools (Desktop development with
C++), then run from the repository root:

```powershell
cargo build --release -p butai
.\target\release\butai.exe
```

`cargo run -p butai -- standalone` runs the TUI with a temporary daemon.
The release workflow builds `x86_64-pc-windows-msvc` natively and packages a
`butai.exe` tarball. `scripts/install.ps1` downloads that artifact, checks its
SHA256SUMS entry, and adds the installation directory to the user's PATH. It
accepts `-Version` and `-InstallDir`, or `BUTAI_VERSION` and `BUTAI_INSTALL_DIR`.
Windows assets become available when a release containing this port is cut.
For manual upgrades, run `butai kill-server`, close all clients, then replace
`butai.exe`; saved workspaces return when the daemon starts again.

## Display compatibility

The Windows TUI alpha defaults to `[ui] glyphs = "ascii"`: borders, arrows, spinners
and meters use single-column ASCII alternatives that do not require a special
font. Normal text, accented characters, CJK and emoji remain Unicode. This is
a display setting; copied text and saved pane output retain their original
characters. Linux and macOS keep their Unicode default.

For the original symbols with a terminal font that supports them, add:

```toml
[ui]
glyphs = "unicode"
```

The Windows client enables UTF-8 while attached and defers wrapping at the last
column so painting the bottom-right corner cannot scroll the whole interface.
It restores the previous console modes and encoding when it exits.

## Shells and configuration

State and configuration live in `%USERPROFILE%\.butai`, with the same
`BUTAI_HOME`, `BUTAI_SOCKET` and `BUTAI_SESSION_FILE` overrides. Windows defaults
to `%COMSPEC%` (`cmd.exe`). To use PowerShell, put this in `config.toml`:

```toml
[general]
default_shell = "pwsh.exe"
```

Process commands use `/D /C` with a temporary batch file for cmd,
`-EncodedCommand` for PowerShell, and `-c` for Unix-style shells. Write `.butai.toml` commands for the configured shell.
Agent lookup respects `PATHEXT`, including npm's `.cmd` wrappers. Batch agents
launch through Windows PowerShell so their paths and arguments survive quoting.

## Local transport and remote hosts

The socket path remains the endpoint identity and the location of its lock
file. Windows maps the absolute path to a deterministic named pipe; it does
not create an AF_UNIX socket file there. The pipe ACL grants access only to the
current user and SYSTEM, and rejects network clients. HTTP and the framed
pane protocol share that pipe. Unix keeps its existing socket transport,
terminal recovery and daemon detachment code.

`butai proxy` exposes either protocol over standard input/output. SSH connections
from Windows to Unix hosts bridge a local named pipe to one `ssh ... butai proxy`
process per connection; Unix clients continue using socket forwarding and SSH
multiplexing. Windows needs OpenSSH on `PATH` and a working noninteractive key
or agent setup for remote workspaces.

To update a connected Unix server from the Windows client, enable the existing
remote updater in that server's `~/.butai/config.toml`:

```toml
[update]
channel = "dev"
allow_remote = true
```

Run `:reload-config` on that server's tab, then use `:update` there or select
the server's update action under SETTINGS → MACHINES. The server downloads
its own platform's release, verifies its checksum and restarts; connected
clients detach and saved workspaces return. This works independently of the
Windows client's local self-update limitation. Only published releases appear
in the updater; CI artifacts do not. Use `channel = "stable"` to follow stable
releases instead of betas.

## Current limits

- Content search skips binary files and files larger than 1 MiB.
- In-place self-update is disabled on Windows; install new releases manually.
- CPU and RAM telemetry use Windows APIs. Network, disk, swap and temperature
  samplers that use `/proc` or Mach are unavailable on Windows.
- Shell rows keep their shell label because ConPTY has no Unix foreground
  process-group query. Automatic SSH handoff detection is Unix-only; use the
  host picker on Windows.
- Remote host discovery expects a Unix shell on the SSH server. Native Windows
  SSH servers and Unix socket tools such as `curl --unix-socket` are not covered.
- The Windows web launcher currently connects to its configured local daemon.
  Adding further Windows or SSH daemons through the browser is not covered.

CI builds and lints the Windows target, tests named-pipe lifecycle and both
protocols, and exercises ConPTY with a managed process and a batch agent.
Linux and macOS run the existing workspace regression suite.

## Alpha artifacts

The `develop` CI run builds a native MSVC Windows executable and uploads a
`butai-windows-tui-alpha-<commit>` artifact after its Windows tests pass. The ZIP
contains `butai.exe`, `VERSION.txt`, `SHA256SUMS`, the license and this guide.
The current underlying build version is `1.3.0-dev.3.beta.1`, which sorts after `1.3.0-dev.2` on the
development update channel. Tagged releases still build all supported targets.

To build the self-contained browser launcher from source:

```powershell
cargo build --release -p butai
cd web
bun install --frozen-lockfile
bun run build
bun scripts/build-desktop.ts ../target/release/butai.exe
```

The result is `dist/butai-web-windows-alpha.exe` at the repository root.
CI tests the compiled launcher against real Windows IPC, including embedded
assets, REST requests, event streams and the pane WebSocket handshake.
