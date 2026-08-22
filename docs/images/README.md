# Screenshots

These are **real captures**, not mockups: the butai binary is run under a pty
against a throwaway git repo, and the rendered cell grid is written straight out
as SVG. Every status in them is genuine — the `FAIL(2)` really is two failing
checks, the git counts really are the working tree.

| File | Referenced by | Shows |
| --- | --- | --- |
| `workbench.svg` | [README](../../README.md) hero, [landing page](../index.html) | The whole frame: AGENTS / PROCESSES / SYSTEM rails, an agent's reply on the stage, CHANGES on the right. The SYSTEM block is the shooting machine's own hardware. **It is redacted after shooting** — see *Redaction* below. |
| `changes-diff.svg` | README → *Review and commit*, landing page | A file's diff on the stage, opened with `d` from the CHANGES rail: one card per file, every line numbered on the side it exists in, syntax-highlighted. |
| `booth.svg` | [workbench.md](../workbench.md) | BOOTH — every workspace across every connected machine, one list, with each machine's COMPUTE column beside it. |
| `help.svg` | [keys.md](../keys.md), [workbench.md](../workbench.md) | The in-app key reference (`?`). |
| `settings.svg` | [theming.md](../theming.md), [configuration.md](../configuration.md) | The SETTINGS page. |
| `agent-working.svg` | README → *The workbench*, landing page | An agent mid-run — the rail row carries a live spinner and elapsed time. **⚠ Still stale:** shot 2026-07-25, before the tab bar became BOOTH and before the spaces became one button on it, and it predates the DSK gauge. `shoot.py` still has no `agent-working` frame, so it cannot be re-shot — either add one, or drop the image from the README. It is the only stale capture left. |

They are SVG rather than PNG because the source is a character grid: text stays
crisp at any zoom, the files are ~20–60 KB instead of a few hundred, and they
diff as text.

## Redaction — do not skip this

The SYSTEM rail reports the machine you shot on, and that is real: the CPU model,
the GPU model, the network interfaces (a Tailscale device appears by name) and
every mount point with its size. Three of these images were published carrying a
CPU model before anyone noticed.

After shooting, rewrite those strings to generic equivalents **of exactly the
same character count**. The SVG pins each run with `textLength` and
`lengthAdjust="spacingAndGlyphs"`, so a replacement of a different length is
silently stretched or squeezed instead of failing. The vocabulary matches the
website's captures, so the README and butai.dev tell one story:

| real | published |
|---|---|
| `Ryzen 7 2700` / `Ryzen 7 5700` | `8-core x86  ` |
| `Tesla P40` | `discrete ` |
| `enp8s0 1G` / `enp1s0 1G` | `eth0 1G  ` |
| `tailscale0` | `vpn0      ` |
| `/media/nvme` / `/media/fast` | `/work      ` |
| `…/storage` / `…/archive` | `/data    ` |

Re-parse every file as XML afterwards; a botched substitution breaks the SVG
rather than merely looking wrong.

## Re-shooting them

Screenshots go stale when the chrome changes, so a change to the frame, the
rails, the status markers or the palette means re-shooting.

```sh
scripts/shoot.py                    # stage a repo, shoot everything, tear down
scripts/shoot.py --only workbench   # one shot
scripts/shoot.py --dump             # also print each reconstructed screen
```

The script stages what it photographs — a small Rust crate with a real branch,
staged and unstaged edits and an untracked file, plus agent doubles that draw
what a real agent CLI draws. Nothing on screen is faked: butai reads agent state
off the pane's own output, so a double that draws the same thing *is* the same
thing as far as the workbench is concerned. Empty rails photograph badly, which
is why any of this staging exists.

Shots available: `workbench`, `changes`, `booth`, `help`, `settings`.

### Safety

`shoot.py` stands up its own daemon under a throwaway `HOME` on an explicit
`--socket`, and tears it down by socket. Both halves matter:

- **A daemon on the default paths restores the real session** — it will open
  your workspaces and spawn your agents.
- **`BUTAI_SOCKET` is inherited** from any butai pane you run this in, so a
  throwaway `HOME` alone is not isolation. The script passes `--socket`
  explicitly, which wins over the environment.
- **Never `pkill -f butai`.** It matches whatever daemon you are actually using.
  Kill by socket: `butai --socket /tmp/bshot/b.sock kill-server`.
- Keep `--home` short. The socket lives under it and `sockaddr_un.sun_path` is
  108 bytes.

### Reading a capture

The client draws cell by cell, so **no word on screen is a contiguous run of
bytes in the pty stream**. Grepping the raw capture always says no, even when
the feature works. Read the reconstruction instead — `--dump` prints it.

### Adding a shot

`SHOTS` in `scripts/shoot.py` maps a name to `(keys to get there, keys to get
back, caption)`. A new surface is a new entry, not new capture code.

## The one still missing

`agent-working.svg` predates the current chrome and no shot reproduces it: it is
a *focused* agent pane rather than the default frame. Either add a shot that
stages an agent and presses `enter` on its row, or drop the image and let
`workbench.svg` — which already carries a working agent with its spinner and
elapsed time — cover it in the two places it is referenced.

### A short demo GIF would beat any of these

One loop of *dispatch agent → watch it work → read the diff → commit* says more
than five frames. [`vhs`](https://github.com/charmbracelet/vhs) drives a real
terminal from a script and emits a reproducible GIF — keep it under ~10 s and
land it here as `demo.gif`.

## Housekeeping

Keep these small. Images live in the repository forever, and a bloated clone is
a real cost for a tool people are meant to install in one command.
