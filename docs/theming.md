# Theming

butai's chrome — tabs, rails, borders, status bar — draws from a **theme**: a set
of named roles mapped to colors. Pane content is never themed. Whatever your
agent, editor or `git diff` printed passes through untouched, which is the rule
[design.md](design.md#color--truecolor-chrome-untouched-pane-content) sets out.

## Switching

Set `[theme] name` in `~/.butai/config.toml` and start the client:

```toml
[theme]
name = "gruvbox-dark"
```

`:theme` used to do this at runtime, back when the daemon composed every frame
and could repaint all attached clients at once. It cannot now: a theme colours a
screen, each client draws its own, and one machine's terminal being dark while
another's is light is the point rather than a problem. The command is still in
the vocabulary and answers with where to set it instead, so an old binding gets
a sentence rather than silence.

Or set it in `~/.butai/config.toml` and let the daemon read it at startup:

```toml
[theme]
name = "blueprint-dark"
```

If you edited the file by hand while butai is running, `:reload-config` picks it
up without a restart.

## Built-in themes

| Name | |
|---|---|
| `blueprint-dark` | Default. Deep blue-grey grounds, one blue accent, amber and green for state. |
| `blueprint-light` | The same palette for light terminals. |
| `catppuccin-mocha` | Catppuccin's darkest flavour. Pair with `syntax_theme = "base16-mocha.dark"`. |
| `gruvbox-dark` | Warm retro. Pair with `syntax_theme = "base16-eighties.dark"`. |
| `nord` | Arctic blue-greys: Polar Night grounds, Frost accent, Aurora for state. |
| `solarized-light` | Light background. Pair with `syntax_theme = "Solarized (light)"`. |
| `tokyonight` | The colors butai shipped before themes existed. |
| `terminal` | Pins nothing — every role defers to your terminal's own palette. |

The SETTINGS page (`alt-s`) walks this list and applies each one as the cursor
passes it, so you can see a palette on your own screen before choosing it;
leaving without pressing Enter puts the old one back.

`terminal` is the one to pick if you already have a colorscheme you like and want
butai to inherit it. Every other theme sends 24-bit color, which overrides the
terminal palette by design.

## Roles

| Role | Used for |
|---|---|
| `ground` | Window background |
| `surface` | Background of overlays — help, pickers |
| `sunken` | Tab-strip background |
| `selection` | Cursor row in a focused list |
| `selection_dim` | Cursor row in an unfocused list |
| `ink` | Primary text |
| `muted` | Secondary text — `[+ agent]`, `[ commit... ]`, and other buttons |
| `faint` | Hints, section labels, placeholders, dim rows |
| `on_accent` | Text drawn on an `accent` or `attention` fill (the active tab, the commit caret) |
| `rule` | Panel borders |
| `rule_focus` | Border of the focused panel |
| `accent` | Markers, the active tab, section headers |
| `info` | Informational state — an agent that finished, directories in the tree |
| `ok` | `ok`, staged files, diff additions, low system load |
| `attention` | Working, modified files, badges, mid system load |
| `danger` | `WAIT`, `FAIL(n)`, untracked files, diff deletions, high load |
| `status_bg` / `status_fg` | Footer background and text |

Colors are semantic, not decorative — `ok` is green-ish because it means "fine",
not because the theme wanted green there. Keep that mapping when you write one,
or the rails stop being scannable. Note also that no state in butai is
color-only: every one keeps its glyph (`[!]`, `FAIL`), so themes never have to
carry the whole signal.

## Writing a theme

Drop a file in `~/.butai/themes/`. The filename (without `.toml`) is the
theme's name.

```toml
# ~/.butai/themes/mine.toml
extends = "blueprint-dark"   # optional; start from another theme

[colors]
accent = "#ff8800"
faint  = "ansi:8"
ground = "default"
```

Then `name = "mine"` under `[theme]`.

- **`extends`** may name a built-in or another file in the same directory.
  Without it, the theme starts from `blueprint-dark`.
- **`[colors]`** overrides only the roles you list; everything else is inherited.
- Values are `#rrggbb`, `ansi:N` (0–255, resolved by your terminal), or
  `default` (the terminal's default foreground/background).
- An unknown role or a malformed color **warns and is skipped** — the rest of
  the theme still loads.

Set `BUTAI_THEME_DIR` to search somewhere else, which is mostly useful for
trying a theme out without touching your config directory.

### Themes to copy

[`examples/themes/`](../examples/themes) has working files to start from —
`cp` one into `~/.butai/themes/` and name it under `[theme]`:

Every file here but `mine.toml` is a *built-in written out in full*. You do not
need any of them to use those themes — the name alone works — they are here to
copy when you want a built-in with two things changed.

| File | |
|---|---|
| [`mine.toml`](../examples/themes/mine.toml) | The common shape: `extends` a built-in, change three roles. |
| [`blueprint-light.toml`](../examples/themes/blueprint-light.toml) | Every role, commented — the one to read first. |
| [`catppuccin-mocha.toml`](../examples/themes/catppuccin-mocha.toml) | |
| [`gruvbox-dark.toml`](../examples/themes/gruvbox-dark.toml) | |
| [`nord.toml`](../examples/themes/nord.toml) | |
| [`solarized-light.toml`](../examples/themes/solarized-light.toml) | |

### Tweaking one color without a file

Any key in `[theme]` besides `name` and `syntax_theme` is a role override
applied over the selected theme:

```toml
[theme]
name = "tokyonight"
accent = "#ff8800"
```

The pre-theme key names `border` and `border_focused` still work here; they now
mean `rule` and `rule_focus`.

## Syntax highlighting

Source in the editor and the diff view is coloured from the same palette as
everything else — comments, strings, numbers, keywords and types each resolve to
a role, so a theme covers code without a second set of colours to keep in step.

`syntax_theme` named a [syntect](https://github.com/trishume/syntect) theme back
when the daemon ran files through syntect for its own editor pane. It is still
**accepted and ignored**, so an existing config neither breaks nor has the name
mistaken for a role override.

## Which clients this covers

The bundled terminal client. Every client draws its own chrome now, so a theme
is a property of the client reading this file — which is what lets two people on
one daemon see different colours, and one person's laptop and phone disagree.

A client that draws against a system palette (the macOS and iOS apps) is
deliberately not themed by this. The browser client (`web/`) **offers these
palettes too**: its SETTINGS page carries `blueprint-dark`, `blueprint-light` and
`tokyonight` copied role for role out of `crates/butai-client/src/theme.rs`,
three more out of `examples/themes/`, and its own two. The role vocabulary above
is joined to CSS custom-property names by a table in
`web/src/logic/settings.ts`. It does not read this config file: the bridge has no
access to one and the choice is per browser, in `localStorage`. So the *palettes*
are shared and the *store* is not, which is the same split as everywhere else
here — a theme is a property of the client reading it.

What every client does share is *pane* content: the cells a program wrote arrive
with the program's own colours and are never re-themed by anybody.

## How the browser client loads one

**The mechanism:** a theme is `VARS` written onto `<html>` as inline custom
properties, and nothing else. No second stylesheet, no class on the root, no
per-component styles to re-render. `web/src/logic/settings.ts` holds the palettes
and `VARS` — the table joining each role above to the CSS variable it drives —
and `web/src/theme.ts` is the only code that writes them. Inline style, so it
beats anything a stylesheet says, and `colorScheme` is set alongside them because
the scrollbars, the form controls and the canvas the browser paints behind the
page all read that and none of them reads a custom property.

**There are no colour literals under `web/src/`.** `settings.ts` already owns the
palettes; a second copy in the view layer would be two statements of one
decision, and the drift between them is exactly what the rewrite removed. So the
palettes are read at runtime rather than restated.

**The default is `system`, and `system` means *follow the OS*.** That is not a
fallback; it is an instruction, and it is what an untouched browser did before
any of this existed. It resolves to `web-dark` or `web-light` — the two palettes
this client has always drawn — according to `prefers-color-scheme`.

That resolution is a **function call**, `resolveTheme(name, prefersDark())`,
rather than a media query. The client carries no stylesheet palette to fall
through to, so the older trick of *removing* the variables and letting a
`prefers-color-scheme` block show through would leave the page with no palette at
all. Same two palettes, same OS preference, reached without a media query — and
because `system` is a palette that *moves*, `useTheme` listens for the OS
flipping rather than only re-resolving on render. Without that, a page left open
across a sunset keeps the morning's colours.

`bootTheme()` runs from `main.tsx` before React renders, so the palette is on
`<html>` before the first frame that has anything in it. `index.html` declares
`<meta name="color-scheme" content="dark light">` for the moments before even
that: an OS set to dark gets a dark canvas from the user agent itself, with no
flash of white in between. Reading the stored preference is wrapped in a
`try` — a browser can throw outright on `localStorage` (Safari's private mode, a
blocked third-party context), and a client that will not boot because it could
not read a colour preference is worse than one that draws the default.

Tailwind supplies the geometry and never the colour. Under v4 the theme is
`@theme` in `web/src/styles.css`, naming those same custom properties, so one
write restyles Tailwind's whole output — including every translucent surface and
every shadow, since v4 resolves colours through `color-mix()`. Two extra
variables, `--term-bg` and `--term-fg`, come from `termColors(pal)` and are what
the stage's canvas clears to, so the terminal and the chrome around it agree
before a single cell has arrived.

## Where this lives

| Section | Source |
|---|---|
| Roles, and what each draws | [`crates/butai-client/src/theme.rs`](../crates/butai-client/src/theme.rs) |
| Built-in themes | [`crates/butai-client/src/theme.rs`](../crates/butai-client/src/theme.rs), [`examples/themes/`](../examples/themes) |
| Writing a theme, `extends`, `BUTAI_THEME_DIR` | [`crates/butai-client/src/theme.rs`](../crates/butai-client/src/theme.rs) |
| Switching, `[theme]` config keys | [`crates/butai-client/src/config.rs`](../crates/butai-client/src/config.rs) |
| The browser palettes, roles → CSS variables (`VARS`), `resolveTheme` | [`web/src/logic/settings.ts`](../web/src/logic/settings.ts) |
| Applying one to `<html>`, `system` without a media query, the boot write | [`web/src/theme.ts`](../web/src/theme.ts) |
| The colour → Tailwind bridge (`@theme`), and the only stylesheet | [`web/src/styles.css`](../web/src/styles.css) |
| What the browser paints before the palette lands | [`web/index.html`](../web/index.html) |
| The assertions that hold the palettes to the docs | [`web/test/settings-docs.test.ts`](../web/test/settings-docs.test.ts) |
| Roles, the built-in palettes, `extends`, `BUTAI_THEME_DIR` | `crates/butai-client/src/theme.rs` |
| `[theme]` parsing, role overrides, `syntax_theme` | `crates/butai-client/src/config.rs` |
| The SETTINGS page that walks the list and previews as it goes | `crates/butai-client/src/chrome/settings.rs` |
| Syntax roles for the editor and the diff view | `crates/butai-client/src/chrome/mod.rs` |
| The browser client's palettes, the role → CSS-variable table, and its store | `web/src/logic/settings.ts` |
| The same roles applied to `<html>`, and the terminal's two | `web/src/theme.ts` |
| The browser's SETTINGS page: the swatch grid, and the live sample screen | `web/src/pages/SettingsPage.tsx` |
| The palette a pane's cells are resolved against | `web/src/logic/palette.ts`, `web/src/stage/Screen.ts` |
| Themes to copy | [`examples/themes/`](../examples/themes) |
