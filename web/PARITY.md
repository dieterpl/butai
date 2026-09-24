# Web / terminal interface comparison

Reference: `crates/butai-client/src/chrome/{mod,model}.rs`, `workbench.rs`,
`theme.rs`, and the real `docs/images/{workbench,booth,settings}.svg` captures.
The terminal is the design reference. Internal web route ids `work` and `home`
remain stable; their visible names are Agents and Booth.

| Area | Difference found | Web change |
| --- | --- | --- |
| Navigation | Separate work/home/git/files/docker/usage buttons; Docs missing | One view menu: agents → files → git → docker → docs → usage |
| Booth | Treated as a workspace view, with the workspace still selected | Independent Booth chip beside workspace tabs; no active workspace chip on Booth |
| Workspace tabs | Names and machine badges without terminal numbering or close control | Numbered `host:workspace` labels, attention/away marks, accent selection, close button on the active workspace |
| View shortcuts | React shell did not attach the existing Alt/prefix keyboard layer | View cycling, direct view keys, numbered workspace selection, workspace cycling, focus commands, doubled prefix passthrough |
| Terminal input | Pane input could receive navigation chords; entering Booth stole the fleet's focus | Capture app chords before pane input; Booth streams without automatically focusing its terminal |
| Booth layout | Fleet took 26%, Compute only 20%; tray changed height | Terminal quarter-width columns capped at 40/36 cells, fixed four-line attention tray, hierarchy connectors, compact Compute rows |
| Agents layout | Pixel-based rail widths; missing stage title/frame | Terminal defaults of 28/38 cells, framed stage with selected pane name, agent/process proportions, saved rail width settings |
| Agent state | Web-specific bracket marks and generic “working” labels | Terminal sprites, WAIT/done/exit tokens, unread marks, spinner and elapsed turn time |
| Changes | Commit text field, fetch/pull/push button grid, empty section headings | Compact file list with count/branch title and clickable `c commit · g git · ? keys`; commit opens a prompt |
| Diffs | Opened in a modal, leaving the terminal behind it | Changes diffs take the stage; selecting an agent/process restores its terminal; file frames, independent old/new line numbers, source token highlighting |
| Git menu | Searchable list of every operation | Terminal group → operation navigation, mnemonics, j/k/arrows, Enter and Escape/back |
| Git keyboard | Row/footer operations lacked a page keyboard handler | Same row and page verb tables dispatch keyboard actions; Tab changes columns; j/k moves or scrolls |
| Help / settings | Settings gear in header; exit always returned to Agents | Footer controls remember and return to the page they were entered from; expanded Settings choices retain their Escape behavior |
| Layout | Footer controls absent; saved widths ignored | Layout mode resizes focused rails/sections with arrows or h/j/k/l; Tab selects a rail; Enter/Escape exits; Alt-z collapses rails |
| Detach | No control | Footer detach releases the mounted stage connection; attach restores the browser view |
| Hosts / projects | Shell offered neither terminal header control | Host roster/add/remove and new workspace controls, using the existing bridge/actions layer |
| Density | 22/20/26px chrome rows and 15px terminal font | 18px chrome rows, default 14px font and matching terminal line spacing |
| Palette | Default web palette; panel backgrounds and Blueprint info colors differed | Default Blueprint dark, terminal ground for panels, terminal status colors, corrected cyan info colors; explicit saved themes remain respected |
| Telemetry | Static meter bars; network gauges omitted | Daemon history drawn as terminal braille traces; CPU identity, RAM/swap, GPU history, network receive/transmit traces, disk readouts without a trend graph |
| Overlays | Page key listeners could act behind a modal | Dialogs keep keyboard events inside their own surface |

Files and Docs already share a browser; Git, Docker, Usage, Settings and Help
already have dedicated pages. The existing shared kit supplies square frames,
monospace text, bracket buttons, selection bands, themed colors, and no CSS
transitions throughout those pages.

## Verification

- `bun run typecheck`
- `bun run build`
- Unit suites: verbs, fleet, settings/docs, files, gauges, graph, links, visual data
- `bun test/navigation.browser.mjs`: isolated Chromium and Vite, fixture REST
  replies and WebSocket interception; no real daemon or user workspace is changed.
  Covers menu order, cycling, Booth, workspace selection, utility return,
  terminal chord isolation, stage diffs, grouped git menu, rail keys, layout
  resizing, collapse, detach/attach, new workspace creation, telemetry rendering,
  and 800px/320px chrome widths. Screenshots go to `/var/tmp/butai-web-parity/`.

These checks verify web behavior and geometry; they are not a pixel differential
against a live terminal. Browser font rendering, native clipboard/file controls,
and bridge socket configuration differ from terminal/SSH integration. The diff
body uses the same file-frame and numbered source-line design; its
lightweight web tokenizer is separate from the terminal language scanner.
